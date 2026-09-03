//! W-23 验收:所有者侧审计锚点(CLI 端到端,spawn 真实 bin)。
//!
//! 主线证据链:签 → 验通过;然后两条 W-21 完整性链抓不住的篡改(整体截尾 /
//! 只改尾行)在锚点下当场现形——这是本功能的立身之本。伪造锚点、错密钥、
//! 坏账拒签、参数纪律各有测试。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use wanning_core::clock::MockClock;
use wanning_core::delegation::Delegation;
use wanning_core::intent::SpendIntent;
use wanning_core::state::WanningState;

/// 进程内原子序号:防同 tick 撞名(W-21 教训:两个用例抢同一把单写者锁,输方 panic)。
static SEQ: AtomicU64 = AtomicU64::new(0);

fn unique_path(tag: &str, ext: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("wanning-demo-anchor-cli-tests");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间早于 Unix 纪元")
        .as_nanos();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("{tag}-{nanos}-{seq}-{}.{ext}", std::process::id()))
}

/// 五行样本账:注册 → 放行 ¥5 → 超额拒 → 撤销 → 撤销后拒。
fn build_sample_wal(path: &Path) {
    let clock = MockClock::new(1_700_000_000);
    let mut state = WanningState::with_wal(Arc::new(clock.clone()), path).expect("开 WAL");
    state
        .register_delegation(Delegation::new(
            "d1",
            "所有者",
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

/// 手改第 `line`(1-based)行内 memo(语义对账抓不住的字段)。
fn tamper_memo(path: &Path, line: usize) {
    let mut lines = wanning_core::wal::raw_lines(path).expect("读 WAL");
    let mut value: serde_json::Value = serde_json::from_str(&lines[line - 1]).expect("行是 JSON");
    value["rec"]["intent"]["memo"] = serde_json::json!("被改写过的备注");
    lines[line - 1] = value.to_string();
    std::fs::write(path, lines.join("\n") + "\n").expect("重写 WAL");
}

/// 所有者密钥(测试夹具;32 字节 = 64 位十六进制,文件带末尾换行——编辑器常态)。
const KEY_HEX: &str = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";

fn write_key_file() -> PathBuf {
    let path = unique_path("key", "hex");
    std::fs::write(&path, format!("{KEY_HEX}\n")).expect("写密钥文件");
    path
}

fn demo_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wanning-demo"))
}

#[test]
fn cli_signs_then_verifies_end_to_end() {
    let wal = unique_path("wal", "jsonl");
    build_sample_wal(&wal);
    let key = write_key_file();
    let anchor = unique_path("anchor", "json");

    let output = demo_bin()
        .args(["--anchor-sign"])
        .arg(&wal)
        .args(["--key"])
        .arg(&key)
        .args(["--out"])
        .arg(&anchor)
        .output()
        .expect("spawn wanning-demo");
    assert!(output.status.success(), "合法账必须签出: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("前 5 行"), "五行样本账: {stdout}");
    assert!(stdout.contains("SHA-256"), "{stdout}");
    assert!(stdout.contains("另行保管"), "保管要求必须印出来: {stdout}");

    // 锚点文件是人能读的 JSON,且不带密钥。
    let anchor_text = std::fs::read_to_string(&anchor).expect("锚点文件存在");
    assert!(
        anchor_text.contains("\"schema\": \"wanning-anchor-v1\""),
        "{anchor_text}"
    );
    assert!(
        !anchor_text.contains("c0ffee"),
        "锚点文件里绝不能出现密钥: {anchor_text}"
    );

    let output = demo_bin()
        .args(["--anchor-verify"])
        .arg(&wal)
        .args(["--anchor"])
        .arg(&anchor)
        .args(["--key"])
        .arg(&key)
        .output()
        .expect("spawn wanning-demo");
    assert!(output.status.success(), "原账必须验过: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("验证通过"), "{stdout}");
    assert!(stdout.contains("被锚前缀 5 行 / 当前账本 5 行"), "{stdout}");
}

#[test]
fn cli_verify_catches_truncation_and_tail_tamper() {
    // W-21 的两个已知盲区,锚点都要抓住:
    // ① 只改尾行内容(无后继行引用,链抓不住);② 整体截尾(余下前缀自成合法链)。
    let wal = unique_path("wal", "jsonl");
    build_sample_wal(&wal);
    let key = write_key_file();
    let anchor = unique_path("anchor", "json");
    let output = demo_bin()
        .args(["--anchor-sign"])
        .arg(&wal)
        .args(["--key"])
        .arg(&key)
        .args(["--out"])
        .arg(&anchor)
        .output()
        .expect("spawn wanning-demo");
    assert!(output.status.success(), "先签出: {output:?}");

    // ① 改尾行(第 5 行)memo:先复制原账再改。
    let tail_tampered = unique_path("wal-tail", "jsonl");
    std::fs::copy(&wal, &tail_tampered).expect("复制原账");
    tamper_memo(&tail_tampered, 5);
    let output = demo_bin()
        .args(["--anchor-verify"])
        .arg(&tail_tampered)
        .args(["--anchor"])
        .arg(&anchor)
        .args(["--key"])
        .arg(&key)
        .output()
        .expect("spawn wanning-demo");
    assert!(!output.status.success(), "改尾行必须非零退出: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("被改"), "要点名「被改」: {stderr}");

    // ② 整体截尾:删掉最后两行。
    let truncated = unique_path("wal-truncated", "jsonl");
    let lines = wanning_core::wal::raw_lines(&wal).expect("读");
    std::fs::write(&truncated, lines[..3].join("\n") + "\n").expect("写截尾副本");
    let output = demo_bin()
        .args(["--anchor-verify"])
        .arg(&truncated)
        .args(["--anchor"])
        .arg(&anchor)
        .args(["--key"])
        .arg(&key)
        .output()
        .expect("spawn wanning-demo");
    assert!(!output.status.success(), "截尾必须非零退出: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("截尾"), "要点名「截尾」: {stderr}");

    // 对照组:原账(未动)照常验过——抓的是篡改,不是误报。
    let output = demo_bin()
        .args(["--anchor-verify"])
        .arg(&wal)
        .args(["--anchor"])
        .arg(&anchor)
        .args(["--key"])
        .arg(&key)
        .output()
        .expect("spawn wanning-demo");
    assert!(output.status.success(), "原账对照必须验过: {output:?}");
}

#[test]
fn cli_verify_rejects_forged_anchor_and_foreign_key() {
    let wal = unique_path("wal", "jsonl");
    build_sample_wal(&wal);
    let key = write_key_file();
    let anchor = unique_path("anchor", "json");
    let output = demo_bin()
        .args(["--anchor-sign"])
        .arg(&wal)
        .args(["--key"])
        .arg(&key)
        .args(["--out"])
        .arg(&anchor)
        .output()
        .expect("spawn wanning-demo");
    assert!(output.status.success(), "{output:?}");

    // 伪造:改锚点声明的行数(MAC 还是旧的)→ 锚点不可信。
    let forged = unique_path("anchor-forged", "json");
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&anchor).expect("读锚点"))
            .expect("锚点是 JSON");
    value["lines"] = serde_json::json!(4);
    std::fs::write(&forged, value.to_string()).expect("写伪造锚点");
    let output = demo_bin()
        .args(["--anchor-verify"])
        .arg(&wal)
        .args(["--anchor"])
        .arg(&forged)
        .args(["--key"])
        .arg(&key)
        .output()
        .expect("spawn wanning-demo");
    assert!(!output.status.success(), "伪造锚点必须非零退出: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("MAC"), "要点名 MAC 不符: {stderr}");

    // 换一把密钥验(不是所有者签的)→ 拒。
    let other_key = unique_path("key2", "hex");
    std::fs::write(
        &other_key,
        "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef\n",
    )
    .expect("写另一把密钥");
    let output = demo_bin()
        .args(["--anchor-verify"])
        .arg(&wal)
        .args(["--anchor"])
        .arg(&anchor)
        .args(["--key"])
        .arg(&other_key)
        .output()
        .expect("spawn wanning-demo");
    assert!(!output.status.success(), "错密钥必须非零退出: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("锚点"), "{stderr}");
}

#[test]
fn cli_sign_refuses_broken_wal_and_never_touches_output() {
    let wal = unique_path("wal", "jsonl");
    build_sample_wal(&wal);
    tamper_memo(&wal, 3); // 中间行:对账先行的第一道(完整性链)就该拒
    let key = write_key_file();
    let anchor = unique_path("anchor", "json");
    std::fs::write(&anchor, "SENTINEL").expect("预置旧输出");

    let output = demo_bin()
        .args(["--anchor-sign"])
        .arg(&wal)
        .args(["--key"])
        .arg(&key)
        .args(["--out"])
        .arg(&anchor)
        .output()
        .expect("spawn wanning-demo");
    assert!(!output.status.success(), "坏账必须拒签: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("完整性链"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(&anchor).expect("旧输出还在"),
        "SENTINEL",
        "拒签绝不碰输出文件"
    );
    let tmp = anchor.with_file_name({
        let mut name = anchor.file_name().unwrap().to_os_string();
        name.push(".tmp");
        name
    });
    assert!(!tmp.exists(), "不留临时文件");
}

#[test]
fn cli_arg_discipline_for_anchor_modes() {
    let wal = unique_path("wal", "jsonl");
    build_sample_wal(&wal);
    let key = write_key_file();

    // 缺 --key。
    let output = demo_bin()
        .args(["--anchor-sign"])
        .arg(&wal)
        .args(["--out", "a.json"])
        .output()
        .expect("spawn");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--key"),
        "缺密钥要指名 --key"
    );

    // --anchor-verify 缺 --anchor。
    let output = demo_bin()
        .args(["--anchor-verify"])
        .arg(&wal)
        .args(["--key"])
        .arg(&key)
        .output()
        .expect("spawn");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--anchor"),
        "验锚点要指名 --anchor"
    );

    // 与 --scenario 互斥。
    let output = demo_bin()
        .args(["--anchor-sign"])
        .arg(&wal)
        .args(["--key"])
        .arg(&key)
        .args(["--out", "a.json", "--scenario", "smoke"])
        .output()
        .expect("spawn");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("互斥"),
        "两义性即拒"
    );

    // --key / --anchor 不能单飞(不随锚点模式用)。
    let output = demo_bin()
        .args(["--scenario", "smoke", "--key"])
        .arg(&key)
        .output()
        .expect("spawn");
    assert!(!output.status.success());
    let output = demo_bin()
        .args(["--scenario", "smoke", "--anchor", "a.json"])
        .output()
        .expect("spawn");
    assert!(!output.status.success());
}
