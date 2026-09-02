//! smoke 场景的结构化断言:闭环语义、确定性、审计内容。

use wanning_core::gate::DenyReason;
use wanning_core::wal::{WalDecision, WalRecord};
use wanning_demo::scenario::run_smoke;

#[test]
fn smoke_scenario_closes_the_loop_offline() {
    let first = run_smoke().expect("smoke 跑通");

    // ① 预算内放行:¥5.00,判后累计消费 500 分
    assert_eq!(first.allow_budget_after_cents, 500);
    // ② 超额拒 ③ 撤销后拒
    assert_eq!(first.over_budget_reason, DenyReason::OverBudget);
    assert_eq!(first.after_revoke_reason, DenyReason::Revoked);
    // 审计证据行号:注册/放行/超额拒/撤销/撤销后拒 = 1..5
    assert_eq!(first.allow_line, 2);
    assert_eq!(first.over_budget_line, 3);
    assert_eq!(first.after_revoke_line, 5);
    assert_eq!(first.wal_lines, 5);

    // 两遍运行:全新 WAL(绝不复用/截断),语义与状态指纹完全一致(确定性)。
    let second = run_smoke().expect("smoke 再跑");
    assert_ne!(first.wal_path, second.wal_path, "每次运行必须新 WAL");
    assert_eq!(first.state_hash, second.state_hash, "离线场景必须可复现");
}

#[test]
fn smoke_wal_records_the_full_timeline_with_fixed_ts() {
    let outcome = run_smoke().expect("smoke 跑通");
    let records = wanning_core::wal::read_records(&outcome.wal_path).expect("读回审计");
    assert_eq!(records.len(), 5);

    let kinds: Vec<&str> = records.iter().map(|(_, r)| r.kind()).collect();
    assert_eq!(
        kinds,
        vec![
            "register_delegation",
            "decide",
            "decide",
            "revoke",
            "decide"
        ]
    );
    // 注入时钟:全部记录同一时刻(场景不推时间)。
    assert!(records.iter().all(|(_, r)| r.ts() == 1_700_000_000));

    let decisions: Vec<(WalDecision, Option<DenyReason>)> = records[1..]
        .iter()
        .filter_map(|(_, r)| match r {
            WalRecord::Decide {
                decision, reason, ..
            } => Some((*decision, *reason)),
            _ => None,
        })
        .collect();
    assert_eq!(
        decisions,
        vec![
            (WalDecision::Allow, None),
            (WalDecision::Deny, Some(DenyReason::OverBudget)),
            (WalDecision::Deny, Some(DenyReason::Revoked)),
        ],
        "审计必须如实记下两笔拒绝及其原因"
    );
}
