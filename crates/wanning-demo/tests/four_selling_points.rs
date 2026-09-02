//! W-09 验收:四卖点场景(今晚的可演示成果)。

use std::process::Command;

use wanning_core::gate::DenyReason;
use wanning_core::wal::{WalDecision, WalRecord};
use wanning_demo::scenario::run_four_selling_points;

#[test]
fn four_selling_points_outcome_matches_the_pitch() {
    let outcome = run_four_selling_points().expect("场景跑通");

    assert!(
        outcome.source_name.contains("离线脚本场景"),
        "输出必须标注数据来源: {}",
        outcome.source_name
    );
    // 证据行号:注册/放行/超额拒/收权/撤销后拒 = 1..5
    assert_eq!(outcome.allow_line, 2);
    assert_eq!(outcome.over_budget_line, 3);
    assert_eq!(outcome.revoke_line, 4);
    assert_eq!(outcome.after_revoke_line, 5);
    assert_eq!(outcome.wal_lines, 5);
    assert_eq!(outcome.allow_budget_after_cents, 500, "只有①真的扣了账");
    // 审计可对账:回放重建 hash 必须与实时一致。
    assert_eq!(
        outcome.state_hash, outcome.replay_hash,
        "回放对账必须一致,否则审计不可信"
    );
}

#[test]
fn four_selling_points_wal_records_all_three_decisions() {
    let outcome = run_four_selling_points().expect("场景跑通");
    let records = wanning_core::wal::read_records(&outcome.wal_path).expect("读回审计");

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
        ]
    );
    // 收权记录在案(kill switch 也要留痕)。
    assert!(matches!(records[3].1, WalRecord::Revoke { .. }));
}

#[test]
fn cli_four_selling_points_prints_all_four_sections() {
    let output = Command::new(env!("CARGO_BIN_EXE_wanning-demo"))
        .args(["--scenario", "four-selling-points"])
        .env_remove("WANNING_ALLOW_REAL_SPEND")
        .output()
        .expect("spawn wanning-demo");

    assert!(output.status.success(), "离线场景必须跑通: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for marker in [
        "【卖点① 预算内放行】",
        "【卖点② 超额拒绝】",
        "【卖点③ 撤销后拒绝(kill switch)】",
        "【卖点④ 全程审计导出 + 回放对账】",
        "证据:WAL 行 2",
        "证据:WAL 行 3",
        "reason=over_budget",
        "reason=revoked",
        "一致",
        "离线脚本场景",
    ] {
        assert!(stdout.contains(marker), "输出缺少 {marker}:\n{stdout}");
    }
}
