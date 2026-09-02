//! W-30 验收:性能基准(零依赖手写,不引 criterion)。
//!
//! 这里只用**小参数**跑,锁的是「基准函数可运行、口径正确、数值合法」;
//! 真实数字(release 全量参数)以 `cargo run -p wanning-bench --release`
//! 真实跑出并落 docs/benchmarks.md,绝不把测试里的小跑数字当基准写档。

use wanning_bench::{
    audit_html_export, gate_decide_allow, gate_decide_deny_over_budget, run_all, wal_append,
    wal_replay, BenchStats, Sizing, AUDIT_HTML_LINES, GATE_DECIDE_OPS, ROUNDS, WAL_APPEND_LINES,
    WAL_REPLAY_LINES,
};

#[test]
fn gate_allow_bench_measures_positive_throughput() {
    let stats = gate_decide_allow(1_000, 2).expect("Allow 口径基准可跑");
    assert_eq!(stats.ops, 1_000, "每轮操作数如实上报");
    assert_eq!(stats.rounds.len(), 2, "轮数如实上报");
    assert!(stats.median() > 0.0, "吞吐必须为正:{}", stats.median());
    assert_eq!(stats.unit, "判定/s");
}

#[test]
fn gate_deny_bench_measures_positive_throughput() {
    let stats = gate_decide_deny_over_budget(1_000, 2).expect("Deny 口径基准可跑");
    assert_eq!(stats.ops, 1_000);
    assert!(stats.median() > 0.0, "吞吐必须为正:{}", stats.median());
    assert_eq!(stats.unit, "判定/s");
}

#[test]
fn wal_append_bench_measures_positive_throughput() {
    let stats = wal_append(200, 2).expect("WAL 追加基准可跑");
    assert_eq!(stats.ops, 200, "每轮追加行数如实上报");
    assert!(stats.median() > 0.0, "吞吐必须为正:{}", stats.median());
    assert_eq!(stats.unit, "行/s");
}

#[test]
fn wal_replay_bench_measures_positive_throughput() {
    let stats = wal_replay(200, 2).expect("WAL 回放基准可跑");
    assert_eq!(stats.ops, 200, "回放行数如实上报");
    assert!(stats.median() > 0.0, "吞吐必须为正:{}", stats.median());
    assert_eq!(stats.unit, "行/s");
}

#[test]
fn audit_html_export_bench_measures_positive_latency() {
    let stats = audit_html_export(50, 2).expect("审计页导出基准可跑");
    assert_eq!(stats.ops, 50, "导出账本行数如实上报");
    assert!(stats.median() > 0.0, "耗时必须为正:{}", stats.median());
    assert_eq!(stats.unit, "ms");
}

#[test]
fn run_all_produces_all_five_reports_and_default_sizing_is_honest() {
    // 默认口径常量与文档声明一致(docs/benchmarks.md 引用的就是这些)。
    assert_eq!(GATE_DECIDE_OPS, 200_000);
    assert_eq!(WAL_APPEND_LINES, 20_000);
    assert_eq!(WAL_REPLAY_LINES, 50_000);
    assert_eq!(AUDIT_HTML_LINES, 5_000);
    assert_eq!(ROUNDS, 5);

    let sizing = Sizing {
        gate_decide_ops: 500,
        wal_append_lines: 100,
        wal_replay_lines: 100,
        audit_html_lines: 20,
        rounds: 2,
    };
    let reports = run_all(&sizing);
    assert_eq!(reports.len(), 5, "五项基准全跑");
    let labels: Vec<&str> = reports.iter().map(|s| s.label).collect();
    assert_eq!(
        labels,
        vec![
            "gate_decide_allow",
            "gate_decide_deny_over_budget",
            "wal_append",
            "wal_replay",
            "audit_html_export_5k",
        ]
    );
    for stats in &reports {
        assert!(stats.median() > 0.0, "{} 必须测出正数", stats.label);
    }
}

#[test]
fn bench_stats_median_and_min_are_real_statistics() {
    let stats = BenchStats {
        label: "probe",
        unit: "判定/s",
        ops: 3,
        rounds: vec![10.0, 20.0, 30.0, 40.0],
    };
    assert_eq!(stats.median(), 25.0, "偶数轮取中间两轮均值");
    assert_eq!(stats.min(), 10.0);
}
