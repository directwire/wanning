//! W-07 验收:护栏与 CLI 的端到端实证。
//!
//! 直接 spawn 真实 bin(`CARGO_BIN_EXE`),对「设/不设 env 两路」做进程级验证:
//! - 不设任何护栏 env + `--dry-run false` → 拒绝、非零退出、报错点名缺什么;
//! - 不设任何护栏 env + 默认 dry-run → 离线场景完整跑通(离线路径永远不需要密钥)。

use std::process::Command;

/// 护栏全部 env(测试里一律显式清除/设置,不受开发者本机环境影响)。
const GUARD_ENV: [&str; 5] = [
    "WANNING_ALLOW_REAL_SPEND",
    "WANNING_GLM_KEY",
    "WANNING_JD_APP_KEY",
    "WANNING_JD_APP_SECRET",
    "WANNING_JD_ACCESS_TOKEN",
];

fn demo_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wanning-demo"));
    for key in GUARD_ENV {
        cmd.env_remove(key);
    }
    cmd
}

#[test]
fn real_path_refuses_without_env_and_names_what_is_missing() {
    let output = demo_bin()
        .args(["--scenario", "smoke", "--dry-run", "false"])
        .output()
        .expect("spawn wanning-demo");

    assert!(
        !output.status.success(),
        "无 env 时真实路径必须非零退出,实际 {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fail-closed"),
        "报错要点明 fail-closed: {stderr}"
    );
    assert!(stderr.contains("WANNING_ALLOW_REAL_SPEND"), "{stderr}");
    assert!(stderr.contains("WANNING_GLM_KEY"), "{stderr}");
    assert!(stderr.contains("WANNING_JD_APP_SECRET"), "{stderr}");
}

#[test]
fn real_path_with_full_env_still_refuses_until_channel_is_wired() {
    // 假密钥:护栏只验证「已配置」;接线检查是下一道门。全程无网络调用。
    let output = demo_bin()
        .args(["--scenario", "smoke", "--dry-run", "false"])
        .env("WANNING_ALLOW_REAL_SPEND", "1")
        .env("WANNING_GLM_KEY", "test-glm-key")
        .env("WANNING_JD_APP_KEY", "test-jd-key")
        .env("WANNING_JD_APP_SECRET", "test-jd-secret")
        .env("WANNING_JD_ACCESS_TOKEN", "test-jd-token")
        .output()
        .expect("spawn wanning-demo");

    assert!(
        !output.status.success(),
        "通道未接线时即使护栏通过也必须拒绝: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("尚未接线"), "{stderr}");
    assert!(
        !stderr.contains("test-glm-key"),
        "密钥绝不能出现在输出里: {stderr}"
    );
}

#[test]
fn dry_run_default_runs_offline_scenario_without_any_env() {
    let output = demo_bin()
        .args(["--scenario", "smoke"])
        .output()
        .expect("spawn wanning-demo");

    assert!(output.status.success(), "离线路径必须跑通: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("四卖点证据"), "{stdout}");
    assert!(stdout.contains("ALLOW"), "{stdout}");
    assert!(stdout.contains("DENY"), "{stdout}");
    assert!(stdout.contains("over_budget"), "{stdout}");
    assert!(stdout.contains("revoked"), "{stdout}");
}

#[test]
fn unknown_scenario_is_refused_with_available_list() {
    let output = demo_bin()
        .args(["--scenario", "nope"])
        .output()
        .expect("spawn wanning-demo");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("未知场景"), "{stderr}");
    assert!(stderr.contains("smoke"), "{stderr}");
}

#[test]
fn missing_scenario_arg_is_refused() {
    let output = demo_bin().output().expect("spawn wanning-demo");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--scenario"), "{stderr}");
}
