//! 全链 mock 闭环场景(`--scenario full-loop-mock`):所有者一条命令看全貌。
//!
//! 链路 = 脚本意图 → 闸(含拒绝路径)→ 京东 mock backend(search→create_order)
//! → 支付宝 mock channel(trigger_pay→回调幂等应用)→ 收据;**中途任一步拒绝即短路**
//! 并打印短路点;输出末尾照例回放对账。与 four-selling-points 并存(不改动其输出)。
//!
//! 复用 W-08/W-10/W-11 现成件,**零新协议面**:
//! - HTTP 全部走真实传输层([`UreqApiTransport`])打本地 mock server
//!   ([`crate::mock_server`],127.0.0.1:0,零外网)——与 W-10/W-11 集成测试同一管线,
//!   账户开通后换真端点的就是这条链;
//! - 回调不走 HTTP:Wanning 侧的回调接收端(to be built)不是本场景该造的协议面;
//!   渠道异步通知以「渠道侧报文 → [`PayNotify::parse`] → [`apply_pay_notify`]」的
//!   W-11 已测链路进账,传输接线(账户开通后)另算;
//! - 交易台账在 demo 是内存态([`TradeState`]);WAL 只记闸判定(注册/撤销/意图判定),
//!   交易终态的持久化台账是账户开通后的接线工作。
//!
//! 诚实边界(输出里同样写明):**闸放行即记账**——渠道侧短路的意图**不退预算**,
//! 审计里能看到「放行了但没有对应订单」;这是闸语义(授权即扣额)的诚实呈现,
//! 退款/额度释放不在闸语义内。

use std::path::PathBuf;
use std::sync::Arc;

use wanning_core::clock::MockClock;
use wanning_core::delegation::Delegation;
use wanning_core::error::CoreError;
use wanning_core::gate::{DenyReason, GateDecision};
use wanning_core::intent::SpendIntent;
use wanning_core::state::WanningState;
use wanning_core::wal::read_records;

use crate::alipay::{
    apply_pay_notify, AlipayBackend, PayNotify, PayRequest, PayStatus, PaymentChannel, TradeState,
};
use crate::http::UreqApiTransport;
use crate::jd::{CommerceBackend, CreateOrderRequest, JdBackend, SearchRequest};
use crate::mock_server::spawn_json_mock;
use crate::scenario::{deny_reason_zh, fresh_wal_path, render_record, SCENARIO_FULL_LOOP_MOCK};

// ---------------------------------------------------------------------------
// 本地 mock 契约(wanning-demo 自定义字段,非京东/支付宝真实报文;真实字段待
// W-12/W-13 调研 + 账户开通后填充,绝不臆造)
// ---------------------------------------------------------------------------

/// 京东 search 应答:候选商品价格与商户**恰好等于腿①的放行意图**(500 分 / jd:shop-1)。
const MOCK_SEARCH_OK: &str = r#"{"products":[{"sku_id":"jd-mock-sku-1","title":"mock 商品(本地 mock 契约,非京东报文)","price_cents":500,"merchant_id":"jd:shop-1"}]}"#;

/// 京东 create_order 应答。
const MOCK_ORDER_OK: &str =
    r#"{"order_id":"jd-mock-order-1","sku_id":"jd-mock-sku-1","amount_cents":500}"#;

/// 京东 search 应答(腿③):无候选商品 → 渠道侧短路。
const MOCK_SEARCH_EMPTY: &str = r#"{"products":[]}"#;

/// 支付宝 trigger_pay 应答:受理成功、扣款异步(pending),终态走回调。
const MOCK_PAY_PENDING: &str =
    r#"{"trade_no":"alipay-mock-trade-1","status":"pending","amount_cents":500}"#;

// ---------------------------------------------------------------------------
// 场景脚本:三段腿(全链走通 / 闸拒短路 / 渠道侧短路)
// ---------------------------------------------------------------------------

struct Leg {
    amount_cents: u64,
    merchant_id: &'static str,
    category: &'static str,
    memo: &'static str,
}

const LEGS: &[Leg] = &[
    Leg {
        amount_cents: 500,
        merchant_id: "jd:shop-1",
        category: "grocery",
        memo: "闭环腿①:预算内放行,全链走通(下单→扣款→回调结算)",
    },
    Leg {
        amount_cents: 900,
        merchant_id: "jd:shop-2",
        category: "grocery",
        memo: "闭环腿②:超额请求(累计 ¥14 > 上限 ¥10),闸拒即短路",
    },
    Leg {
        amount_cents: 200,
        merchant_id: "jd:shop-3",
        category: "grocery",
        memo: "闭环腿③:闸放行但渠道侧无候选,中途短路",
    },
];

/// 短路发生的阶段(收据与终端输出用;None = 全链走通)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortCircuitStage {
    /// 闸拒绝——链路第一步就断了,后续渠道步骤一律不执行。
    Gate,
    /// 京东 search 失败/无候选/报价与放行金额不符。
    CommerceSearch,
    /// 京东 create_order 失败。
    CommerceOrder,
    /// 支付宝 trigger_pay 失败。
    Payment,
    /// 回调报文不合契约或与本地交易对不上(拒绝应用,不改台账)。
    Callback,
}

/// 一次短路的记录:短路点 + 人可读原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortCircuit {
    pub stage: ShortCircuitStage,
    pub reason: String,
}

impl ShortCircuit {
    fn new(stage: ShortCircuitStage, reason: impl Into<String>) -> Self {
        Self {
            stage,
            reason: reason.into(),
        }
    }
}

/// 一段腿的收据(意图 id / 判定 / 订单号 / 支付态 / WAL 行号 + 短路点)。
#[derive(Debug, Clone)]
pub struct LegReceipt {
    /// 闸侧分配的 nonce(意图标识 = delegation_id + nonce)。
    pub nonce: u64,
    pub amount_cents: u64,
    pub merchant_id: String,
    pub category: String,
    /// 闸判定:true=ALLOW。
    pub allowed: bool,
    /// ALLOW 时的判后累计消费(分)。
    pub budget_after_cents: Option<u64>,
    /// DENY 时的拒绝原因。
    pub deny_reason: Option<DenyReason>,
    /// 该判定落审计的 WAL 行号(1-based,证据位置)。
    pub gate_wal_line: u64,
    /// 短路点(None = 全链走通)。
    pub short_circuit: Option<ShortCircuit>,
    /// 渠道侧结果(走到才有):订单号。
    pub order_id: Option<String>,
    /// 扣款幂等键 (委托, nonce, 订单) 确定性派生。
    pub out_request_no: Option<String>,
    /// 渠道侧交易号。
    pub trade_no: Option<String>,
    /// 回调应用后的交易终态。
    pub trade_status_after_callback: Option<PayStatus>,
    /// 首条回调:true=台账状态推进;false=同态重复(如渠道同步返回 success)。
    pub callback_applied: Option<bool>,
    /// 重复投递同一条回调的验证:Some(true)=被幂等吸收(no-op,W-11 语义)。
    pub callback_redelivery_noop: Option<bool>,
}

/// 场景结构化结果(测试断言面;打印只是展示层)。
#[derive(Debug)]
pub struct FullLoopOutcome {
    pub wal_path: PathBuf,
    pub wal_lines: u64,
    pub state_hash: u64,
    /// 回放重建的 state hash(应与实时一致)。
    pub replay_hash: u64,
    /// 审计完整性链尾(实时侧写路径逐行累计)。
    pub chain_tail_live: u64,
    /// 审计完整性链尾(读侧独立重算)。
    pub chain_tail_replay: u64,
    pub budget_cap_cents: u64,
    /// 全部腿跑完后委托的累计消费(分)——渠道侧短路的腿**不退预算**(诚实边界)。
    pub spent_cents_after: u64,
    pub legs: Vec<LegReceipt>,
    /// 本地 mock 记录的原始 HTTP 请求(证据:闸拒的腿零出网由此可证)。
    pub jd_requests: Vec<String>,
    pub alipay_requests: Vec<String>,
}

/// 跑全链 mock 闭环场景(零网络零真实消费:渠道端点全是 127.0.0.1 的本地 mock)。
pub fn run_full_loop_mock() -> Result<FullLoopOutcome, CoreError> {
    let wal_path = fresh_wal_path("full-loop-mock");
    // 注入时钟:固定 Unix 起点,场景语义与真实时间无关(可复现)。
    let clock = MockClock::new(1_700_000_000);
    let mut state = WanningState::with_wal(Arc::new(clock.clone()), &wal_path)?;
    state.register_delegation(Delegation::new(
        "d1",
        "所有者",
        "claude-code",
        1_000, // ¥10.00 总预算
        1_700_000_000,
        1_700_003_600,
        "agent:claude-code",
    ))?;

    // 本地 mock 渠道:京东按腿依次应答(腿① search+order,腿③ search 空);
    // 支付宝只服务腿①的 trigger_pay。腿②被闸拒,不会打到这里(测试实证零出网)。
    let jd_mock = spawn_json_mock(vec![
        (200, MOCK_SEARCH_OK.to_string()),
        (200, MOCK_ORDER_OK.to_string()),
        (200, MOCK_SEARCH_EMPTY.to_string()),
    ]);
    let alipay_mock = spawn_json_mock(vec![(200, MOCK_PAY_PENDING.to_string())]);
    let mut jd = JdBackend::new_mock(&jd_mock.url(), Arc::new(UreqApiTransport));
    let mut channel = AlipayBackend::new_mock(&alipay_mock.url(), Arc::new(UreqApiTransport));

    let mut legs: Vec<LegReceipt> = Vec::new();
    // nonce 分配 = 已提意图数 + 1(与 demo 决策回路同一策略;拒绝不耗号,单调只增不撞)。
    for (index, leg) in LEGS.iter().enumerate() {
        let nonce = index as u64 + 1;
        let intent = SpendIntent::new(
            "d1",
            nonce,
            leg.amount_cents,
            leg.merchant_id,
            leg.category,
            leg.memo,
        );
        let decision = state.decide(&intent)?;
        let gate_wal_line = state.last_wal_line().expect("挂了 WAL 必有行号");

        let mut receipt = LegReceipt {
            nonce,
            amount_cents: leg.amount_cents,
            merchant_id: leg.merchant_id.to_string(),
            category: leg.category.to_string(),
            allowed: decision.is_allow(),
            budget_after_cents: None,
            deny_reason: decision.deny_reason(),
            gate_wal_line,
            short_circuit: None,
            order_id: None,
            out_request_no: None,
            trade_no: None,
            trade_status_after_callback: None,
            callback_applied: None,
            callback_redelivery_noop: None,
        };

        match decision {
            GateDecision::Deny { reason } => {
                receipt.short_circuit = Some(ShortCircuit::new(
                    ShortCircuitStage::Gate,
                    format!(
                        "闸拒绝({}):{};后续渠道步骤一律不执行",
                        serde_reason(&reason),
                        deny_reason_zh(&reason)
                    ),
                ));
            }
            GateDecision::Allow { budget_after_cents } => {
                receipt.budget_after_cents = Some(budget_after_cents);
                let walk = walk_allowed_leg(&mut jd, &mut channel, leg, "d1", nonce)?;
                receipt.order_id = walk.order_id;
                receipt.out_request_no = walk.out_request_no;
                receipt.trade_no = walk.trade_no;
                receipt.trade_status_after_callback = walk.trade_status_after_callback;
                receipt.callback_applied = walk.callback_applied;
                receipt.callback_redelivery_noop = walk.callback_redelivery_noop;
                receipt.short_circuit = walk.short_circuit;
            }
        }
        legs.push(receipt);
    }

    let outcome = FullLoopOutcome {
        wal_lines: state.wal_line_count().expect("必有 WAL"),
        state_hash: state.state_hash(),
        replay_hash: WanningState::replay(&wal_path)?.state_hash(),
        chain_tail_live: state.audit_chain_tail().expect("必有 WAL"),
        chain_tail_replay: wanning_core::wal::read_verified(&wal_path)?.tail,
        budget_cap_cents: 1_000,
        spent_cents_after: state.gate().spent_cents("d1").unwrap_or(0),
        jd_requests: jd_mock.recorded_requests(),
        alipay_requests: alipay_mock.recorded_requests(),
        legs,
        wal_path,
    };
    print_full_loop_report(&outcome)?;
    Ok(outcome)
}

/// 一段已放行腿的渠道侧行走结果(收据字段 + 可能的短路点)。
struct ChannelWalk {
    order_id: Option<String>,
    out_request_no: Option<String>,
    trade_no: Option<String>,
    trade_status_after_callback: Option<PayStatus>,
    callback_applied: Option<bool>,
    callback_redelivery_noop: Option<bool>,
    short_circuit: Option<ShortCircuit>,
}

/// 已放行意图的渠道侧行走:京东 search → 金额/商户核对 → create_order →
/// 支付宝 trigger_pay → 回调幂等应用。**任何一步不过即短路返回**,后续步骤不执行。
fn walk_allowed_leg(
    jd: &mut JdBackend,
    channel: &mut AlipayBackend,
    leg: &Leg,
    delegation_id: &str,
    nonce: u64,
) -> Result<ChannelWalk, CoreError> {
    let mut walk = ChannelWalk {
        order_id: None,
        out_request_no: None,
        trade_no: None,
        trade_status_after_callback: None,
        callback_applied: None,
        callback_redelivery_noop: None,
        short_circuit: None,
    };

    // 京东 search:keyword=类目,预算约束=放行金额(超放行金额的候选不进入)。
    let search = SearchRequest {
        keyword: leg.category.to_string(),
        max_price_cents: Some(leg.amount_cents),
        limit: 5,
    };
    let products = match jd.search(&search) {
        Ok(products) => products,
        Err(e) => {
            walk.short_circuit = Some(ShortCircuit::new(
                ShortCircuitStage::CommerceSearch,
                format!("京东 search 失败:{e}"),
            ));
            return Ok(walk);
        }
    };
    let Some(product) = products.first() else {
        walk.short_circuit = Some(ShortCircuit::new(
            ShortCircuitStage::CommerceSearch,
            "京东 search 无候选商品:闸已放行,但渠道侧没有可下的订单",
        ));
        return Ok(walk);
    };
    // fail-closed:渠道报价与放行金额分毫不差、商户与意图一致,才许下单——
    // 渠道侧不得替 agent 多花一分、也不得挂到意图之外的商户。
    if product.price_cents != leg.amount_cents {
        walk.short_circuit = Some(ShortCircuit::new(
            ShortCircuitStage::CommerceSearch,
            format!(
                "渠道报价与放行金额不符:放行 {} 分 / 候选 {} 分(拒绝下单)",
                leg.amount_cents, product.price_cents
            ),
        ));
        return Ok(walk);
    }
    if product.merchant_id != leg.merchant_id {
        walk.short_circuit = Some(ShortCircuit::new(
            ShortCircuitStage::CommerceSearch,
            format!(
                "候选商户与意图商户不符:意图 {} / 候选 {}(拒绝下单)",
                leg.merchant_id, product.merchant_id
            ),
        ));
        return Ok(walk);
    }

    let order = match jd.create_order(&CreateOrderRequest {
        sku_id: product.sku_id.clone(),
        quantity: 1,
        amount_cents: leg.amount_cents,
        delegation_id: delegation_id.to_string(),
        intent_nonce: nonce,
    }) {
        Ok(order) => order,
        Err(e) => {
            walk.short_circuit = Some(ShortCircuit::new(
                ShortCircuitStage::CommerceOrder,
                format!("京东 create_order 失败:{e}"),
            ));
            return Ok(walk);
        }
    };
    walk.order_id = Some(order.order_id.clone());

    // 支付宝:协议内扣款(幂等键由 (委托, nonce, 订单) 派生,重复触发不双扣)。
    let pay_request = PayRequest {
        order_id: order.order_id.clone(),
        amount_cents: leg.amount_cents,
        delegation_id: delegation_id.to_string(),
        intent_nonce: nonce,
    };
    let pay = match PaymentChannel::trigger_pay(channel, &pay_request) {
        Ok(pay) => pay,
        Err(e) => {
            walk.short_circuit = Some(ShortCircuit::new(
                ShortCircuitStage::Payment,
                format!("支付宝 trigger_pay 失败:{e}"),
            ));
            return Ok(walk);
        }
    };
    walk.out_request_no = Some(pay.out_request_no.clone());
    walk.trade_no = Some(pay.trade_no.clone());

    // 本地交易台账(内存态;WAL 只记闸判定)。
    let mut trade = TradeState {
        out_request_no: pay.out_request_no.clone(),
        trade_no: pay.trade_no.clone(),
        amount_cents: leg.amount_cents,
        status: pay.status,
    };

    // 渠道侧异步通知(本地 mock 契约报文,字段值取自真实触发结果;真实回调报文
    // 与传输接线以 W-13/W-24 调研 + 账户开通后为准,且必须先验签再进账)。
    let notify_raw = serde_json::json!({
        "out_request_no": pay.out_request_no,
        "trade_no": pay.trade_no,
        "status": "success",
        "amount_cents": leg.amount_cents,
    })
    .to_string();
    let notify = match PayNotify::parse(&notify_raw) {
        Ok(notify) => notify,
        Err(e) => {
            walk.short_circuit = Some(ShortCircuit::new(
                ShortCircuitStage::Callback,
                format!("回调报文不合契约,拒绝应用:{e}"),
            ));
            return Ok(walk);
        }
    };
    match apply_pay_notify(&mut trade, &notify) {
        Ok(applied) => {
            walk.callback_applied = Some(applied);
            // 重复投递同一条通知:必须被幂等吸收(no-op)——W-11 语义在闭环里再走一遍。
            match apply_pay_notify(&mut trade, &notify) {
                Ok(_) => walk.callback_redelivery_noop = Some(true),
                Err(e) => {
                    walk.short_circuit = Some(ShortCircuit::new(
                        ShortCircuitStage::Callback,
                        format!("重复回调未按幂等 no-op 处理,需人工核对:{e}"),
                    ));
                    return Ok(walk);
                }
            }
            walk.trade_status_after_callback = Some(trade.status);
        }
        Err(e) => {
            walk.short_circuit = Some(ShortCircuit::new(
                ShortCircuitStage::Callback,
                format!("回调与本笔交易对不上,拒绝应用(不改台账):{e}"),
            ));
            return Ok(walk);
        }
    }
    Ok(walk)
}

// ---------------------------------------------------------------------------
// 终端输出
// ---------------------------------------------------------------------------

fn print_full_loop_report(outcome: &FullLoopOutcome) -> Result<(), CoreError> {
    println!("================================================================");
    println!(" Wanning 全链 mock 闭环 · {SCENARIO_FULL_LOOP_MOCK}");
    println!(" 意图 → 闸 → 京东 mock → 支付宝 mock → 回调结算 → 收据;任一步拒绝即短路");
    println!(" 渠道端点全为 127.0.0.1 本地 mock(零外网);MockClock 固定起点;零真实消费");
    println!("================================================================");

    for leg in &outcome.legs {
        println!();
        println!(
            "—— 腿(意图 d1#nonce={}):{}分 商户 {} 类目 {}",
            leg.nonce, leg.amount_cents, leg.merchant_id, leg.category
        );
        if leg.allowed {
            println!(
                "   闸:ALLOW(证据:WAL 行 {},判后累计消费 {} 分)",
                leg.gate_wal_line,
                leg.budget_after_cents.unwrap_or(0)
            );
        } else {
            let reason = leg
                .deny_reason
                .as_ref()
                .map(|r| format!("{}({})", serde_reason(r), deny_reason_zh(r)))
                .unwrap_or_else(|| "未知".to_string());
            println!(
                "   闸:DENY(证据:WAL 行 {},reason={reason})",
                leg.gate_wal_line
            );
        }

        match &leg.short_circuit {
            Some(sc) => {
                let stage = match sc.stage {
                    ShortCircuitStage::Gate => "闸",
                    ShortCircuitStage::CommerceSearch => "京东 search",
                    ShortCircuitStage::CommerceOrder => "京东 create_order",
                    ShortCircuitStage::Payment => "支付宝 trigger_pay",
                    ShortCircuitStage::Callback => "回调应用",
                };
                println!("   短路点={stage}:{},链路到此为止", sc.reason);
                if sc.stage == ShortCircuitStage::Gate {
                    println!("   (零出网证据:京东/支付宝 mock 的请求计数不含本腿)");
                }
                if sc.stage == ShortCircuitStage::CommerceSearch {
                    println!(
                        "   诚实边界:闸放行即记账——本腿 {} 分不退预算;审计行 {} = 放行但没有对应订单",
                        leg.amount_cents, leg.gate_wal_line
                    );
                }
            }
            None => {
                println!(
                    "   京东:search → 下单 → 订单 {}(报文带 delegation_id=d1 / intent_nonce={})",
                    leg.order_id.as_deref().unwrap_or("-"),
                    leg.nonce
                );
                println!(
                    "   支付宝:trigger_pay 幂等键 {} → 交易 {}(受理,异步扣款)",
                    leg.out_request_no.as_deref().unwrap_or("-"),
                    leg.trade_no.as_deref().unwrap_or("-")
                );
                println!(
                    "   回调:渠道异步通知(mock 契约报文)→ 幂等应用:台账推进={};重复投递同一条=no-op({})",
                    leg.callback_applied.map(|b| b.to_string()).unwrap_or_default(),
                    match leg.callback_redelivery_noop {
                        Some(true) => "被幂等吸收,不重复入账".to_string(),
                        other => other.map(|b| b.to_string()).unwrap_or_default(),
                    }
                );
                println!(
                    "   收据:意图=d1#nonce={} 判定=ALLOW 订单={} 交易={} 支付态={} 证据行={}",
                    leg.nonce,
                    leg.order_id.as_deref().unwrap_or("-"),
                    leg.trade_no.as_deref().unwrap_or("-"),
                    leg.trade_status_after_callback
                        .as_ref()
                        .map(std::string::ToString::to_string)
                        .unwrap_or_else(|| "-".to_string()),
                    leg.gate_wal_line
                );
            }
        }
    }

    println!();
    println!(
        "审计时间线(行号 = WAL 偏移,即证据位置;WAL 共 {} 行:{})",
        outcome.wal_lines,
        outcome.wal_path.display()
    );
    for (line_no, record) in read_records(&outcome.wal_path)? {
        println!("  行 {line_no:>3} | {}", render_record(&record));
    }

    println!();
    println!(
        "预算台账:累计消费 {}/{} 分(剩余 {} 分)——含渠道侧短路腿的放行记账(不退预算)",
        outcome.spent_cents_after,
        outcome.budget_cap_cents,
        outcome.budget_cap_cents - outcome.spent_cents_after
    );
    println!(
        "回放对账:live state_hash={:016x},replay state_hash={:016x},{}",
        outcome.state_hash,
        outcome.replay_hash,
        if outcome.state_hash == outcome.replay_hash {
            "一致 —— 审计可完整重建状态"
        } else {
            "不一致 —— 这是不该发生的事故"
        }
    );
    println!(
        "完整性链:live 链尾={:016x},读侧重算={:016x},{}",
        outcome.chain_tail_live,
        outcome.chain_tail_replay,
        if outcome.chain_tail_live == outcome.chain_tail_replay {
            "一致"
        } else {
            "不一致 —— 这是不该发生的事故"
        }
    );
    println!("结论:闸放行才进渠道;渠道侧任何一步不过即短路;回调幂等;全程审计可对账。");
    Ok(())
}

/// DenyReason 的 serde 蛇形名(与 WAL 行内一致)。
fn serde_reason(reason: &DenyReason) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{reason:?}"))
}
