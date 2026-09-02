//! W-22 验收:静态审计回放页(WAL → 自包含 HTML 时间线,零后端零外链)。
//!
//! 两层实证:
//! - 库面([`wanning_demo::audit_html`]):报告构建 fail-closed(链断裂/坏行/缺文件
//!   一律报错、绝不产出半页证据),渲染自包含、数据全部 HTML 转义;
//! - CLI 端到端(spawn 真实 bin):`--export-audit <wal> --out <html>` 成功导出 /
//!   篡改 WAL 拒绝且输出文件一个字节都不动。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use wanning_core::clock::MockClock;
use wanning_core::delegation::Delegation;
use wanning_core::error::CoreError;
use wanning_core::intent::SpendIntent;
use wanning_core::state::WanningState;
use wanning_demo::audit_html;

/// 进程内原子序号:防同 tick 撞名(W-21 教训:两个用例抢同一把单写者锁,输方 panic)。
static SEQ: AtomicU64 = AtomicU64::new(0);

fn unique_wal(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("wanning-demo-audit-html-tests");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间早于 Unix 纪元")
        .as_nanos();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("{tag}-{nanos}-{seq}-{}.jsonl", std::process::id()))
}

/// 五行样本账:注册 → 放行 ¥5 → 超额拒 → 撤销 → 撤销后拒(时间逐行推进,页面可读)。
fn build_sample_wal(path: &Path) {
    let clock = MockClock::new(1_700_000_000);
    let mut state = WanningState::with_wal(Arc::new(clock.clone()), path).expect("开 WAL");
    state
        .register_delegation(Delegation::new(
            "d1",
            "老板",
            "claude-code",
            1_000, // ¥10.00
            1_700_000_000,
            1_700_003_600,
            "agent:claude-code",
        ))
        .expect("注册");
    state
        .decide(&SpendIntent::new(
            "d1",
            1,
            500,
            "jd:shop-1",
            "grocery",
            "预算内放行",
        ))
        .expect("放行");
    clock.set_now(1_700_000_060);
    state
        .decide(&SpendIntent::new(
            "d1",
            2,
            900,
            "jd:shop-1",
            "grocery",
            "超出预算",
        ))
        .expect("超额拒");
    clock.set_now(1_700_000_120);
    state.revoke("d1").expect("撤销");
    clock.set_now(1_700_000_180);
    state
        .decide(&SpendIntent::new(
            "d1",
            3,
            100,
            "jd:shop-1",
            "grocery",
            "撤销后再消费",
        ))
        .expect("撤销后拒");
}

/// 手改 WAL 第 `line`(1-based)行内的 memo(不参与判定的字段,语义对账抓不住,
/// 只有完整性链抓得住)。
fn tamper_memo(path: &Path, line: usize) {
    let mut lines = wanning_core::wal::raw_lines(path).expect("读 WAL");
    let mut value: serde_json::Value = serde_json::from_str(&lines[line - 1]).expect("行是 JSON");
    value["rec"]["intent"]["memo"] = serde_json::json!("被改写过的备注");
    lines[line - 1] = value.to_string();
    std::fs::write(path, lines.join("\n") + "\n").expect("重写 WAL");
}

// ---------------------------------------------------------------------------
// 报告构建:对账 + 汇总
// ---------------------------------------------------------------------------

#[test]
fn report_reconciles_and_counts_every_event() {
    let wal = unique_wal("report");
    build_sample_wal(&wal);

    let report = audit_html::build_report(&wal).expect("合法账必出报告");
    assert_eq!(report.rows.len(), 5, "五行样本账");
    assert_eq!(report.counts.register, 1);
    assert_eq!(report.counts.allow, 1);
    assert_eq!(report.counts.deny, 2);
    assert_eq!(report.counts.revoke, 1);
    assert_eq!(
        report.allow_amount_cents, 500,
        "累计放行金额 = 唯一一笔放行"
    );
    assert_ne!(report.replay_state_hash, 0, "五行账回放 hash 非 0");

    // 逐行链:首行 prev = 创世 0,尾行链值 = 链尾。
    assert_eq!(report.rows[0].link.prev, 0);
    assert_eq!(report.rows[4].link.value, report.chain_tail);

    // 预算汇总来自回放态:已用 ¥5.00 / 上限 ¥10.00,撤销态如实呈现。
    assert_eq!(report.delegations.len(), 1);
    let d = &report.delegations[0];
    assert_eq!(d.id, "d1");
    assert_eq!(d.cap_cents, 1_000);
    assert_eq!(d.spent_cents, 500);
    assert_eq!(d.remaining_cents, 500);
    assert!(d.revoked, "撤销态必须如实进入报告");
}

#[test]
fn report_on_empty_wal_is_a_legal_empty_page() {
    let wal = unique_wal("empty");
    std::fs::write(&wal, "").expect("写空文件");
    let report = audit_html::build_report(&wal).expect("空账是合法状态");
    assert!(report.rows.is_empty());
    assert_eq!(report.chain_tail, 0, "空日志链尾 = 创世值 0");
    assert!(report.delegations.is_empty());
    let html = audit_html::render_html(&report);
    assert!(html.contains("0 行"), "空账要有明示: {html}");
}

// ---------------------------------------------------------------------------
// 渲染:自包含 + 转义 + 诚实呈现
// ---------------------------------------------------------------------------

#[test]
fn render_is_self_contained_static_html() {
    let wal = unique_wal("render");
    build_sample_wal(&wal);
    let report = audit_html::build_report(&wal).expect("出报告");
    let html = audit_html::render_html(&report);

    assert!(html.starts_with("<!DOCTYPE html>"), "完整文档,非片段");
    assert!(html.contains("lang=\"zh-CN\""), "{html}");
    assert!(
        !html.contains("<script"),
        "零 JS:审计回放页不得携带任何可执行脚本"
    );
    assert!(
        !html.contains("http://") && !html.contains("https://"),
        "零外链:file:// 离线可开,不得引用任何远程资源"
    );
    assert!(html.contains("tabular-nums"), "数值列对齐用 tabular-nums");
    assert!(
        html.contains("prefers-color-scheme: dark"),
        "深色模式随系统走(纯 CSS,零 JS)"
    );
}

#[test]
fn render_shows_amounts_times_reasons_and_chain() {
    let wal = unique_wal("render-detail");
    build_sample_wal(&wal);
    let report = audit_html::build_report(&wal).expect("出报告");
    let html = audit_html::render_html(&report);

    assert!(html.contains("¥5.00"), "放行金额要人可读: {html}");
    assert!(html.contains("¥10.00"), "预算上限要人可读: {html}");
    assert!(html.contains("超出预算上限"), "拒绝原因中文呈现");
    assert!(
        html.contains("2023-11-14"),
        "UTC 时间可读(ts=1_700_000_000)"
    );
    assert!(html.contains("22:13:20"), "首行时刻(样本账起点)");
    assert!(
        html.contains("完整性链"),
        "逐行链要可见,不是黑盒一句「已验」"
    );
    assert!(
        html.contains("只改最后一行内容") || html.contains("截尾"),
        "已知边界要诚实落页(链抓不住尾行篡改与截尾,需外部锚点)"
    );
}

#[test]
fn render_escapes_every_data_derived_text() {
    let wal = unique_wal("escape");
    let clock = MockClock::new(1_700_000_000);
    let mut state = WanningState::with_wal(Arc::new(clock.clone()), &wal).expect("开 WAL");
    state
        .register_delegation(Delegation::new(
            "d1",
            "O'Reilly & <Sons>",
            "claude-code",
            1_000,
            1_700_000_000,
            1_700_003_600,
            "agent:claude-code",
        ))
        .expect("注册");
    state
        .decide(&SpendIntent::new(
            "d1",
            1,
            500,
            "jd:<img src=x onerror=alert(1)>",
            "<i>grocery</i>",
            "<script>alert(1)</script> & \"quotes\"",
        ))
        .expect("放行");

    let html = audit_html::render_html(&audit_html::build_report(&wal).expect("出报告"));
    assert!(!html.contains("<script"), "memo 里的脚本必须被转义: {html}");
    assert!(
        html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "转义后仍可读: {html}"
    );
    assert!(
        !html.contains("<img"),
        "merchant 里的标签必须被转义: {html}"
    );
    assert!(html.contains("&amp;"), "& 必须转义(否则后面全是脏数据)");
    assert!(html.contains("&quot;"), "引号必须转义(不破坏属性边界)");
    assert!(!html.contains("<i>"), "category 里的标签必须被转义: {html}");
    assert!(
        !html.contains("O'Reilly & <Sons>"),
        "owner 原文不得未转义直出: {html}"
    );
}

// ---------------------------------------------------------------------------
// fail-closed:坏账绝不产出半页证据
// ---------------------------------------------------------------------------

#[test]
fn broken_chain_fails_closed_and_writes_nothing() {
    let wal = unique_wal("broken");
    build_sample_wal(&wal);
    tamper_memo(&wal, 2); // 改第 2 行 memo(有后继行引用其链值 → 必断链)

    match audit_html::build_report(&wal) {
        Err(CoreError::WalChainBroken { .. }) => {}
        other => panic!("链断裂必须 fail-closed,实际 {other:?}"),
    }

    // 导出面同样拒绝,且输出文件一个字节都不动。
    let out = wal.with_extension("html");
    std::fs::write(&out, "SENTINEL-不得被覆盖").expect("预置旧输出");
    let result = audit_html::export_audit(&wal, &out, None);
    assert!(result.is_err(), "坏账绝不能产出回放页");
    assert_eq!(
        std::fs::read_to_string(&out).expect("旧输出还在"),
        "SENTINEL-不得被覆盖",
        "失败路径绝不覆盖已有文件"
    );
    assert!(
        !wal.with_extension("html.tmp").exists(),
        "临时文件不得残留(原子写:先写 tmp 再改名)"
    );
}

#[test]
fn half_line_and_missing_wal_fail_closed() {
    let half = unique_wal("half");
    build_sample_wal(&half);
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&half)
        .expect("开");
    f.write_all(b"{\"kind\":\"decide\",\"ts\":1,\"dele\n")
        .expect("追加坏行");
    drop(f);
    assert!(
        matches!(
            audit_html::build_report(&half),
            Err(CoreError::WalBadLine { .. })
        ),
        "半行 JSON 必须 fail-closed"
    );

    let missing = unique_wal("missing");
    assert!(
        audit_html::build_report(&missing).is_err(),
        "缺文件必须报错"
    );
}

// ---------------------------------------------------------------------------
// CLI 端到端(spawn 真实 bin)
// ---------------------------------------------------------------------------

fn demo_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wanning-demo"))
}

#[test]
fn cli_exports_verified_audit_page_end_to_end() {
    let wal = unique_wal("cli-ok");
    build_sample_wal(&wal);
    let out = wal.with_extension("html");

    let output = demo_bin()
        .args(["--export-audit"])
        .arg(&wal)
        .args(["--out"])
        .arg(&out)
        .output()
        .expect("spawn wanning-demo");
    assert!(output.status.success(), "合法账必须导出成功: {output:?}");

    let html = std::fs::read_to_string(&out).expect("输出文件存在");
    assert!(html.starts_with("<!DOCTYPE html>"), "{html}");
    assert!(html.contains("¥5.00"), "{html}");
    assert!(html.contains("超出预算上限"), "{html}");

    // 幂等:同一命令重跑(报告是视图,重算覆盖生成是正常工作流)。
    let output = demo_bin()
        .args(["--export-audit"])
        .arg(&wal)
        .args(["--out"])
        .arg(&out)
        .output()
        .expect("spawn wanning-demo");
    assert!(output.status.success(), "重复导出必须成功: {output:?}");
    assert!(std::fs::read_to_string(&out)
        .expect("重读")
        .contains("¥5.00"));
}

#[test]
fn cli_refuses_broken_chain_and_never_touches_output() {
    let wal = unique_wal("cli-broken");
    build_sample_wal(&wal);
    tamper_memo(&wal, 3);
    let out = wal.with_extension("html");
    std::fs::write(&out, "SENTINEL").expect("预置旧输出");

    let output = demo_bin()
        .args(["--export-audit"])
        .arg(&wal)
        .args(["--out"])
        .arg(&out)
        .output()
        .expect("spawn wanning-demo");

    assert!(!output.status.success(), "篡改账必须非零退出: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("完整性链"), "报错要点名断因: {stderr}");
    assert_eq!(
        std::fs::read_to_string(&out).expect("旧输出还在"),
        "SENTINEL",
        "拒绝路径绝不产出/覆盖输出文件"
    );
}

#[test]
fn cli_arg_discipline_for_export_mode() {
    let wal = unique_wal("cli-args");
    build_sample_wal(&wal);

    // 缺 --out。
    let output = demo_bin()
        .args(["--export-audit"])
        .arg(&wal)
        .output()
        .expect("spawn");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--out"), "{stderr}");

    // 与 --scenario 互斥(两义性即拒,fail-closed)。
    let output = demo_bin()
        .args(["--export-audit"])
        .arg(&wal)
        .args(["--out", "x.html", "--scenario", "smoke"])
        .output()
        .expect("spawn");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("互斥") || stderr.contains("同时"),
        "{stderr}"
    );

    // 未知参数照旧拒绝。
    let output = demo_bin()
        .args(["--export-audit"])
        .arg(&wal)
        .args(["--out", "x.html", "--bogus"])
        .output()
        .expect("spawn");
    assert!(!output.status.success());
}
