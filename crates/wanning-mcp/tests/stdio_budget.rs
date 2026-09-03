//! W-43a 产品化:预算 CLI 面(`--budget` 主别名 / `--max-spends`)真子进程验证。
//!
//! 产品化要求:用户照 README 抄的命令是 `wanning … --budget 1000`,不是内部的
//! `--cap-cents`。主别名必须真走通 stdio 面(不只是 lib 单测),且与旧别名冲突时
//! fail-closed(两义性即拒,绝不静默二选一)。

mod common;

use std::process::{Command, Stdio};

use common::{fresh_wal_path, McpProc};
use serde_json::{json, Value};

fn evaluate_call(id: i64, nonce: u64, amount_cents: u64) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": "wanning_gate_evaluate", "arguments": {
            "delegation_id": "demo-d1", "nonce": nonce,
            "amount_cents": amount_cents, "merchant_id": "jd:shop-1",
            "category": "grocery", "memo": "W-43 预算 CLI" } }
    })
}

#[test]
fn budget_alias_caps_spend_like_cap_cents() {
    let wal = fresh_wal_path("budget-alias");
    let mut proc = McpProc::spawn(&["--wal", &wal.to_string_lossy(), "--budget", "500"]);
    proc.handshake();

    // 上限 500 分:花 400 分放行(budget_after = 累计已花 400)。
    proc.send(&evaluate_call(1, 1, 400));
    let value = proc.response();
    assert_eq!(value["result"]["isError"], false, "{value}");
    assert_eq!(value["result"]["structuredContent"]["decision"], "allow");
    assert_eq!(
        value["result"]["structuredContent"]["budget_after_cents"],
        400
    );

    // 再花 200 分:100 + 200 > 500 → over_budget。
    proc.send(&evaluate_call(2, 2, 200));
    let value = proc.response();
    assert_eq!(value["result"]["structuredContent"]["decision"], "deny");
    assert_eq!(
        value["result"]["structuredContent"]["reason"],
        "over_budget"
    );

    proc.shutdown();
}

#[test]
fn budget_and_cap_cents_together_are_rejected() {
    // 主别名与旧别名同时出现 = 两义性,fail-closed 拒启(usage 级错误,exit 2)。
    let status = Command::new(env!("CARGO_BIN_EXE_wanning-mcp"))
        .args([
            "--wal",
            &fresh_wal_path("conflict").to_string_lossy(),
            "--budget",
            "500",
            "--cap-cents",
            "1000",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn")
        .wait_with_output()
        .expect("wait");
    assert_eq!(status.status.code(), Some(2), "两义性 = usage 错误");
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(stderr.contains("--budget"), "{stderr}");
    assert!(stderr.contains("--cap-cents"), "{stderr}");
}

#[test]
fn max_spends_zero_disables_rate_limiting() {
    let wal = fresh_wal_path("max-spends-0");
    let mut proc = McpProc::spawn(&["--wal", &wal.to_string_lossy(), "--max-spends", "0"]);
    proc.handshake();

    for nonce in 1..=12 {
        proc.send(&evaluate_call(nonce as i64, nonce, 10));
        let value = proc.response();
        assert_eq!(
            value["result"]["structuredContent"]["decision"], "allow",
            "关闭速率护栏后第 {nonce} 笔不该被拒:{value}"
        );
    }

    proc.shutdown();
}

#[test]
fn max_spends_two_rate_limits_the_third() {
    let wal = fresh_wal_path("max-spends-2");
    let mut proc = McpProc::spawn(&["--wal", &wal.to_string_lossy(), "--max-spends", "2"]);
    proc.handshake();

    for nonce in 1..=2 {
        proc.send(&evaluate_call(nonce as i64, nonce, 10));
        let value = proc.response();
        assert_eq!(
            value["result"]["structuredContent"]["decision"], "allow",
            "前 2 笔放行:{value}"
        );
    }
    proc.send(&evaluate_call(3, 3, 10));
    let value = proc.response();
    assert_eq!(value["result"]["structuredContent"]["decision"], "deny");
    assert_eq!(
        value["result"]["structuredContent"]["reason"], "rate_limited",
        "第 3 笔被速率护栏拒绝:{value}"
    );

    proc.shutdown();
}

#[test]
fn max_spends_requires_a_value() {
    let status = Command::new(env!("CARGO_BIN_EXE_wanning-mcp"))
        .args([
            "--wal",
            &fresh_wal_path("max-spends-missing").to_string_lossy(),
            "--max-spends",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn")
        .wait_with_output()
        .expect("wait");
    assert_eq!(status.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(stderr.contains("--max-spends"), "{stderr}");
}

#[test]
fn budget_zero_is_rejected_before_serving() {
    // 预算上限 0 的委托一注册就零预算:启动即拒(fail-closed),绝不带病服务。
    let status = Command::new(env!("CARGO_BIN_EXE_wanning-mcp"))
        .args([
            "--wal",
            &fresh_wal_path("budget-zero").to_string_lossy(),
            "--budget",
            "0",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn")
        .wait_with_output()
        .expect("wait");
    assert!(!status.status.success(), "预算 0 必须拒启");
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(stderr.contains("--budget"), "{stderr}");
    assert!(stderr.contains("0"), "{stderr}");
}
