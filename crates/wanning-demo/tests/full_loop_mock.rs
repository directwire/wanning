//! W-29 验收:全链 mock 闭环场景(`--scenario full-loop-mock`,老板一条命令看全貌)。
//!
//! 场景链路:脚本意图 → 闸(含拒绝路径)→ 京东 mock backend(search→create_order)
//! → 支付宝 mock channel(trigger_pay→回调幂等应用)→ 收据;**中途任一步拒绝即短路**
//! 并记录短路点。全部打本地 mock HTTP server(127.0.0.1:0),零外网、零真实消费。

use wanning_demo::alipay::PayStatus;
use wanning_demo::full_loop::{run_full_loop_mock, ShortCircuitStage};

/// 本地 mock 契约(与场景内一致,测试独立声明以便断言证据字段)。
const MOCK_ORDER_ID: &str = "jd-mock-order-1";
const MOCK_TRADE_NO: &str = "alipay-mock-trade-1";

#[test]
fn happy_leg_walks_gate_jd_order_alipay_and_settles_by_callback() {
    let outcome = run_full_loop_mock().expect("场景应完整跑通");
    let leg1 = outcome
        .legs
        .first()
        .expect("第一段腿=全链 happy path")
        .clone();

    // 闸:放行(委托上限 1000 分,本笔 500 分),证据行号 = 注册行之后的第一条判定。
    assert!(leg1.allowed, "腿①应被闸放行");
    assert_eq!(leg1.gate_wal_line, 2, "行1=注册,行2=腿①放行");
    assert_eq!(leg1.budget_after_cents, Some(500), "判后累计消费=500 分");
    assert_eq!(leg1.short_circuit, None, "腿①全链走通,无短路点");
    assert_eq!(leg1.nonce, 1);
    assert_eq!(leg1.merchant_id, "jd:shop-1");

    // 京东:search → create_order,订单带着授权上下文(delegation + nonce)。
    assert_eq!(leg1.order_id.as_deref(), Some(MOCK_ORDER_ID));
    let order_request = outcome
        .jd_requests
        .iter()
        .find(|raw| raw.contains("create_order") || raw.contains(MOCK_ORDER_ID))
        .or_else(|| outcome.jd_requests.get(1))
        .expect("京东 mock 应收到下单请求");
    assert!(
        order_request.contains("\"delegation_id\":\"d1\""),
        "下单请求必须挂授权上下文,实际:{order_request}"
    );
    assert!(
        order_request.contains("\"intent_nonce\":1"),
        "下单请求必须带闸侧 nonce,实际:{order_request}"
    );
    assert!(
        order_request.contains("\"amount_cents\":500"),
        "下单金额必须等于放行金额,实际:{order_request}"
    );

    // 支付宝:trigger_pay 幂等键 = (委托, nonce, 订单) 确定性派生。
    assert_eq!(
        leg1.out_request_no.as_deref(),
        Some("w-d1-1-jd-mock-order-1")
    );
    assert_eq!(leg1.trade_no.as_deref(), Some(MOCK_TRADE_NO));
    let pay_request = outcome
        .alipay_requests
        .first()
        .expect("支付宝 mock 应收到扣款请求");
    assert!(
        pay_request.contains("\"out_request_no\":\"w-d1-1-jd-mock-order-1\""),
        "扣款请求带幂等键,实际:{pay_request}"
    );

    // 回调:渠道侧异步通知应用后台账,状态推进到 success;重复投递同一条 = 幂等 no-op。
    assert_eq!(
        leg1.trade_status_after_callback,
        Some(PayStatus::Success),
        "回调 success 后交易终态应为 success"
    );
    assert_eq!(leg1.callback_applied, Some(true), "首条回调应推进台账");
    assert_eq!(
        leg1.callback_redelivery_noop,
        Some(true),
        "同一条回调重复投递必须是 no-op(W-11 语义在闭环里复现)"
    );
}

#[test]
fn gate_deny_leg_short_circuits_and_makes_zero_http_calls() {
    let outcome = run_full_loop_mock().expect("场景应完整跑通");
    let leg2 = outcome.legs.get(1).expect("第二段腿=闸拒短路").clone();

    assert!(!leg2.allowed, "腿②(900 分,累计 1400 > 上限 1000)应被拒");
    assert_eq!(leg2.gate_wal_line, 3, "行3=腿②拒绝判定");
    match (&leg2.deny_reason, &leg2.short_circuit) {
        (Some(reason), Some(sc)) => {
            assert_eq!(sc.stage, ShortCircuitStage::Gate, "短路点=闸(第一步)");
            assert_eq!(reason.to_string(), "over_budget");
            assert!(
                sc.reason.contains("超出预算"),
                "短路原因要人可读:{}",
                sc.reason
            );
        }
        _ => panic!("腿②必须有拒绝原因与短路点,实际:{leg2:?}"),
    }
    assert_eq!(leg2.order_id, None, "闸拒后不得下单");
    assert_eq!(leg2.trade_no, None, "闸拒后不得发起扣款");

    // 短路的硬证据:HTTP 调用总数与「腿① 2 次京东 + 腿③ 1 次京东 + 腿① 1 次支付宝」一致,
    // 即腿②零出网。
    assert_eq!(
        outcome.jd_requests.len(),
        3,
        "京东 mock 应恰好收到 3 次请求(腿① search+order,腿③ search);腿②零请求"
    );
    assert_eq!(
        outcome.alipay_requests.len(),
        1,
        "支付宝 mock 应恰好收到 1 次请求(腿①);腿②③零请求"
    );
}

#[test]
fn mid_chain_short_circuit_at_search_keeps_budget_committed_honestly() {
    let outcome = run_full_loop_mock().expect("场景应完整跑通");
    let leg3 = outcome.legs.get(2).expect("第三段腿=渠道侧短路").clone();

    // 闸放行(200 分,累计 700 ≤ 1000),但京东 search 无候选 → 渠道侧短路。
    assert!(leg3.allowed, "腿③应被闸放行(预算足够)");
    assert_eq!(leg3.gate_wal_line, 4, "行4=腿③放行判定");
    assert_eq!(leg3.order_id, None, "无候选商品就不得下单");
    assert_eq!(leg3.trade_no, None, "没下单就不得扣款");
    let sc = leg3.short_circuit.as_ref().expect("腿③必须在渠道侧短路");
    assert_eq!(
        sc.stage,
        ShortCircuitStage::CommerceSearch,
        "短路点=京东 search 阶段"
    );
    assert!(
        sc.reason.contains("无候选") || sc.reason.contains("search"),
        "短路原因要能定位到阶段:{}",
        sc.reason
    );

    // 诚实边界:闸放行即记账——渠道侧短路的意图**不退预算**。
    // 500(腿①)+ 200(腿③)= 700 分,腿③没有任何订单与扣款,预算照样按放行入账;
    // 审计里能看到「行4 放行但没有对应订单」。这条不是设计事故,是闸语义(授权即扣额)
    // 的诚实呈现,场景输出必须带这句话,不许装作没有。
    assert_eq!(outcome.spent_cents_after, 700, "放行即记账:腿③ 200 分不退");
    assert_eq!(outcome.budget_cap_cents, 1_000);
    assert_eq!(outcome.legs.len(), 3);
}

#[test]
fn full_loop_wal_replays_and_chain_tails_match() {
    let outcome = run_full_loop_mock().expect("场景应完整跑通");

    assert_eq!(outcome.wal_lines, 4, "行1 注册 + 行2/3/4 三次判定");
    assert_eq!(
        outcome.state_hash, outcome.replay_hash,
        "实时态必须等于回放态(审计可完整重建)"
    );
    assert_eq!(
        outcome.chain_tail_live, outcome.chain_tail_replay,
        "完整性链尾:实时侧与读侧独立重算一致"
    );
}
