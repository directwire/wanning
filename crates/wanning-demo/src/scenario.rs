//! 离线场景:全本地 MockClock + 临时 WAL,零网络、零真实消费。
//!
//! 场景是「四卖点的可演示证据」:预算内放行、超额拒、撤销后拒、审计时间线。
//! 每个场景返回结构化结果供测试断言,打印只是展示面;证据行号一律取自
//! [`WanningState::last_wal_line`](wanning_core::state::WanningState::last_wal_line)
//! (写入时的真实偏移),不硬编码。

use std::path::PathBuf;
use std::sync::Arc;

use wanning_core::clock::MockClock;
use wanning_core::delegation::Delegation;
use wanning_core::error::CoreError;
use wanning_core::gate::{DenyReason, GateDecision};
use wanning_core::intent::SpendIntent;
use wanning_core::state::WanningState;
use wanning_core::wal::{read_records, WalDecision, WalRecord};

/// 冒烟场景:注册 → 预算内放行 → 超额拒 → 撤销 → 撤销后拒,打印完整审计时间线。
pub const SCENARIO_SMOKE: &str = "smoke";

/// 四卖点场景(今晚的可演示成果):①预算内放行 ②超额拒 ③撤销后拒 ④审计导出+回放对账。
pub const SCENARIO_FOUR_SELLING_POINTS: &str = "four-selling-points";

/// 全链 mock 闭环场景(W-29):意图 → 闸(含拒绝)→ 京东 mock → 支付宝 mock →
/// 回调结算 → 收据;中途任一步拒绝即短路。实现与测试在 [`crate::full_loop`]。
pub const SCENARIO_FULL_LOOP_MOCK: &str = "full-loop-mock";

/// 当前全部可用场景(供 CLI 报错提示)。
pub const AVAILABLE_SCENARIOS: &[&str] = &[
    SCENARIO_SMOKE,
    SCENARIO_FOUR_SELLING_POINTS,
    SCENARIO_FULL_LOOP_MOCK,
];

/// [`DenyReason`] 的中文说明(终端/审计展示用)。
pub fn deny_reason_zh(reason: &DenyReason) -> &'static str {
    match reason {
        DenyReason::UnknownDelegation => "未知委托",
        DenyReason::NotYetValid => "委托未生效",
        DenyReason::Expired => "委托已过期",
        DenyReason::Revoked => "委托已被撤销(kill switch)",
        DenyReason::Replay => "nonce 重放",
        DenyReason::OverBudget => "超出预算上限",
        DenyReason::Overflow => "金额溢出",
        DenyReason::InvalidAmount => "金额非法",
        DenyReason::InvalidNonce => "nonce 非法",
        DenyReason::InvalidIntent => "意图非法",
        DenyReason::RateLimited => "超出速率限制(滑动窗口)",
        DenyReason::OverCategoryBudget => "超出类目预算",
        DenyReason::MerchantDenied => "商户在黑名单",
        DenyReason::MerchantNotAllowed => "商户不在白名单",
        DenyReason::QuietHours => "处于禁止时段",
    }
}

/// 冒烟场景的结构化结果(测试断言面)。
#[derive(Debug)]
pub struct SmokeOutcome {
    pub allow_budget_after_cents: u64,
    pub over_budget_reason: DenyReason,
    pub after_revoke_reason: DenyReason,
    /// 证据行号(WAL 偏移,1-based):预算内放行 / 超额拒 / 撤销后拒。
    pub allow_line: u64,
    pub over_budget_line: u64,
    pub after_revoke_line: u64,
    pub wal_path: PathBuf,
    pub wal_lines: u64,
    pub state_hash: u64,
}

/// 每次运行用全新 WAL(append-only 禁 truncate,固定文件名会让多次运行的时间线混在一起,
/// 行号证据也就失效)。名字里带进程内原子序号:并行测试同一 tick 多次建临时 WAL 会撞名,
/// 两个用例抢同一把单写者锁(W-21 的教训,落档后此处补齐)。
pub(crate) fn fresh_wal_path(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join("wanning-demo");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let unix_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间早于 Unix 纪元")
        .as_nanos();
    dir.join(format!(
        "{tag}-{unix_nanos}-{}-{seq}.jsonl",
        std::process::id()
    ))
}

/// 场景 smoke:一条委托从授权到收权的完整闭环。
pub fn run_smoke() -> Result<SmokeOutcome, CoreError> {
    let wal_path = fresh_wal_path("smoke");
    // 注入时钟:固定 Unix 起点,场景语义与真实时间无关(可复现)。
    let clock = MockClock::new(1_700_000_000);
    let mut state = WanningState::with_wal(Arc::new(clock.clone()), &wal_path)?;

    let delegation = Delegation::new(
        "d1",
        "老板",
        "claude-code",
        1_000, // ¥10.00
        1_700_000_000,
        1_700_003_600,
        "agent:claude-code",
    );
    state.register_delegation(delegation)?;

    // ① 预算内放行:¥5.00
    let allow = state.decide(&SpendIntent::new(
        "d1",
        1,
        500,
        "jd:shop-1",
        "grocery",
        "场景①:预算内放行",
    ))?;
    let allow_line = state.last_wal_line().expect("smoke 必有 WAL");
    let allow_budget_after_cents = match allow {
        GateDecision::Allow {
            budget_after_cents, ..
        } => budget_after_cents,
        GateDecision::Deny { reason } => {
            panic!("场景①必须放行,实际被拒: {reason:?}")
        }
    };

    // ② 超额拒:再来 ¥9.00,累计 ¥14.00 > 上限 ¥10.00
    let over_budget = state.decide(&SpendIntent::new(
        "d1",
        2,
        900,
        "jd:shop-1",
        "grocery",
        "场景②:超出预算",
    ))?;
    let over_budget_line = state.last_wal_line().expect("smoke 必有 WAL");
    let over_budget_reason = over_budget.deny_reason().expect("场景②必须被拒(超额)");

    // ③ 撤销后拒:kill switch 生效,再小的金额也过不去
    state.revoke("d1")?;
    let after_revoke = state.decide(&SpendIntent::new(
        "d1",
        3,
        100,
        "jd:shop-1",
        "grocery",
        "场景③:撤销后再消费",
    ))?;
    let after_revoke_line = state.last_wal_line().expect("smoke 必有 WAL");
    let after_revoke_reason = after_revoke.deny_reason().expect("场景③必须被拒(撤销)");

    let outcome = SmokeOutcome {
        allow_budget_after_cents,
        over_budget_reason,
        after_revoke_reason,
        allow_line,
        over_budget_line,
        after_revoke_line,
        state_hash: state.state_hash(),
        wal_lines: state.wal_line_count().expect("smoke 必有 WAL"),
        wal_path,
    };
    print_smoke_report(&outcome)?;
    Ok(outcome)
}

/// 打印冒烟报告:审计时间线逐行带 WAL 行号(证据偏移)。
fn print_smoke_report(outcome: &SmokeOutcome) -> Result<(), CoreError> {
    println!("=== 离线场景 smoke(全本地 MockClock + 临时 WAL,零网络、零真实消费)===");
    println!("WAL: {}", outcome.wal_path.display());
    println!();
    println!("审计时间线(行号 = WAL 偏移,即证据位置):");
    for (line_no, record) in read_records(&outcome.wal_path)? {
        println!("  行 {line_no:>3} | {}", render_record(&record));
    }
    println!();
    println!(
        "闸最终态:累计消费 {} 分 / 上限 1000 分(剩余 {} 分),state_hash={:016x}",
        outcome.allow_budget_after_cents,
        1_000 - outcome.allow_budget_after_cents,
        outcome.state_hash
    );
    println!(
        "四卖点证据:①预算内放行=行{};②超额拒=行{}({});③撤销后拒=行{}({});④全程审计=上表逐行",
        outcome.allow_line,
        outcome.over_budget_line,
        deny_reason_zh(&outcome.over_budget_reason),
        outcome.after_revoke_line,
        deny_reason_zh(&outcome.after_revoke_reason)
    );
    Ok(())
}

/// 单条审计记录的展示行(full-loop-mock 场景共用)。
pub(crate) fn render_record(record: &WalRecord) -> String {
    match record {
        WalRecord::RegisterDelegation { delegation, .. } => format!(
            "register_delegation | 委托 {} owner={} agent={} 上限={}分 窗口=[{}, {})",
            delegation.id,
            delegation.owner,
            delegation.agent,
            delegation.budget_cap_cents,
            delegation.valid_from,
            delegation.valid_until
        ),
        WalRecord::Revoke { delegation_id, .. } => {
            format!("revoke              | kill switch:撤销委托 {delegation_id}")
        }
        WalRecord::Decide {
            decision,
            intent,
            reason,
            budget_after_cents,
            ..
        } => format!(
            "{} | intent nonce={} amount={}分 merchant={} | 判后累计消费={}分{}",
            match decision {
                WalDecision::Allow => "ALLOW",
                WalDecision::Deny => "DENY ",
            },
            intent.nonce,
            intent.amount_cents,
            intent.merchant_id,
            budget_after_cents,
            reason
                .map(|r| format!(" | reason={}({})", serde_reason(&r), deny_reason_zh(&r)))
                .unwrap_or_default(),
        ),
    }
}

/// DenyReason 的 serde 蛇形名(与 WAL 行内一致,便于对照原文)。
fn serde_reason(reason: &DenyReason) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{reason:?}"))
}

// ---------------------------------------------------------------------------
// 四卖点场景(今晚的可演示成果)
// ---------------------------------------------------------------------------

/// 四卖点场景的结构化结果(测试断言面)。
#[derive(Debug)]
pub struct FourSellingPointsOutcome {
    pub source_name: &'static str,
    pub wal_path: PathBuf,
    pub wal_lines: u64,
    /// 证据行号(WAL 偏移):①放行 / ②超额拒 / 收权 / ③撤销后拒。
    pub allow_line: u64,
    pub over_budget_line: u64,
    pub revoke_line: u64,
    pub after_revoke_line: u64,
    pub allow_budget_after_cents: u64,
    pub state_hash: u64,
    /// 回放重建的 state hash(应与实时一致——审计可对账)。
    pub replay_hash: u64,
    /// 审计完整性链尾(实时侧,写路径逐行累计)。
    pub chain_tail_live: u64,
    /// 审计完整性链尾(读侧独立重算)。
    pub chain_tail_replay: u64,
}

/// 场景 four-selling-points:预算内放行 → 超额拒 → 老板收权 → 撤销后拒 → 审计导出+回放对账。
pub fn run_four_selling_points() -> Result<FourSellingPointsOutcome, CoreError> {
    use crate::decision::{run_decision_loop, LoopConfig, ScriptedSource, StepEvent};

    let wal_path = fresh_wal_path("four-selling-points");
    let clock = MockClock::new(1_700_000_000);
    let mut state = WanningState::with_wal(Arc::new(clock.clone()), &wal_path)?;
    state.register_delegation(Delegation::new(
        "d1",
        "老板",
        "claude-code",
        1_000, // ¥10.00 总预算
        1_700_000_000,
        1_700_003_600,
        "agent:claude-code",
    ))?;

    let mut source = ScriptedSource::selling_points_script("d1");
    let config = LoopConfig {
        delegation_id: "d1".to_string(),
        max_steps: 8,
        revoke_after_n_intents: Some(2),
    };
    let report = run_decision_loop(&mut state, &mut source, &config).map_err(|e| match e {
        crate::decision::LoopError::Core(core) => core,
        crate::decision::LoopError::Decision(decision) => {
            CoreError::WalIo(format!("决策源故障: {decision}"))
        }
    })?;

    // 从真实事件流里取证据行号(不硬编码)。
    let mut spends = report.events.iter();
    let (allow_line, allow_budget_after_cents, over_budget_line, revoke_line, after_revoke_line) =
        match (spends.next(), spends.next(), spends.next(), spends.next()) {
            (
                Some(StepEvent::Spend {
                    decision,
                    wal_line: allow_line,
                    ..
                }),
                Some(StepEvent::Spend {
                    wal_line: over_budget_line,
                    ..
                }),
                Some(StepEvent::BossRevoke {
                    wal_line: revoke_line,
                    ..
                }),
                Some(StepEvent::Spend {
                    wal_line: after_revoke_line,
                    ..
                }),
            ) => {
                let allow_budget_after_cents = match decision {
                    GateDecision::Allow {
                        budget_after_cents, ..
                    } => *budget_after_cents,
                    GateDecision::Deny { .. } => {
                        panic!("四卖点①必须放行")
                    }
                };
                (
                    *allow_line,
                    allow_budget_after_cents,
                    *over_budget_line,
                    *revoke_line,
                    *after_revoke_line,
                )
            }
            _ => panic!("四卖点事件流不完整: {:?}", report.events),
        };

    let outcome = FourSellingPointsOutcome {
        source_name: report.source_name,
        state_hash: state.state_hash(),
        replay_hash: WanningState::replay(&wal_path)?.state_hash(),
        chain_tail_live: state.audit_chain_tail().expect("必有 WAL"),
        chain_tail_replay: wanning_core::wal::read_verified(&wal_path)?.tail,
        wal_lines: state.wal_line_count().expect("必有 WAL"),
        wal_path,
        allow_line,
        over_budget_line,
        revoke_line,
        after_revoke_line,
        allow_budget_after_cents,
    };
    print_four_selling_points(&outcome)?;
    Ok(outcome)
}

/// 四卖点分节输出,每节标注证据行号(WAL 偏移)。
fn print_four_selling_points(outcome: &FourSellingPointsOutcome) -> Result<(), CoreError> {
    let cap = 1_000u64;
    let source_name = outcome.source_name;
    let allow_line = outcome.allow_line;
    let over_budget_line = outcome.over_budget_line;
    let revoke_line = outcome.revoke_line;
    let after_revoke_line = outcome.after_revoke_line;
    let spent = outcome.allow_budget_after_cents;
    println!("================================================================");
    println!(" Wanning 四卖点演示 · 意图层支付闸");
    println!(" 数据来源标注:{source_name}");
    println!(" 全离线:本地 MockClock + 临时 WAL,零网络、零真实消费(真调路径被护栏挡)");
    println!("================================================================");

    println!();
    println!("【卖点① 预算内放行】(证据:WAL 行 {allow_line})");
    println!(
        "  agent 请求 ¥5.00;闸放行,累计消费 {spent}/{cap} 分,剩余 {} 分。",
        cap - spent
    );

    println!();
    println!("【卖点② 超额拒绝】(证据:WAL 行 {over_budget_line})");
    println!("  agent 再请求 ¥9.00,累计将达 ¥14.00 > 上限 ¥10.00;闸拒绝(reason=over_budget),");
    println!("  账本不动、nonce 不耗——拒绝只是拒绝,不产生任何副作用。");

    println!();
    println!(
        "【卖点③ 撤销后拒绝(kill switch)】(证据:WAL 行 {revoke_line} 收权 / 行 {after_revoke_line} 拒绝)"
    );
    println!("  老板 revoke 委托 d1;此后 agent 再请求 ¥1.00 也被拒(reason=revoked),");
    println!("  撤销即时生效、单向不可解除,再小的金额也出不去。");

    println!();
    println!(
        "【卖点④ 全程审计导出 + 回放对账】(WAL 共 {} 行:{})",
        outcome.wal_lines,
        outcome.wal_path.display()
    );
    for (line_no, record) in read_records(&outcome.wal_path)? {
        println!("  行 {line_no:>3} | {}", render_record(&record));
    }
    println!(
        "  回放对账:live state_hash={:016x},replay state_hash={:016x},{}",
        outcome.state_hash,
        outcome.replay_hash,
        if outcome.state_hash == outcome.replay_hash {
            "一致 —— 审计可完整重建状态,判定与账本逐笔对得上"
        } else {
            "不一致 —— 这是不该发生的事故"
        }
    );
    println!(
        "  完整性链:审计逐行成链(seq=物理行号,prev=前行链值),live 链尾={:016x},\
         读侧重算={:016x},{}",
        outcome.chain_tail_live,
        outcome.chain_tail_replay,
        if outcome.chain_tail_live == outcome.chain_tail_replay {
            "一致 —— 改历史行/删行/重排/复制,读回验链当场报错(除非把后续整条链重算一遍)"
        } else {
            "不一致 —— 这是不该发生的事故"
        }
    );
    println!(
        "  已知边界:只改最后一行内容、整体截尾,链抓不住——需外部锚点兜底(已落地 W-23: \
         wanning-demo --anchor-sign,老板侧密钥签出锚点文件,验锚点时当场现形)"
    );

    println!();
    println!("结论:预算内才放行 · 超额即拒 · 撤销即时 · 全程留痕且可对账。");
    Ok(())
}
