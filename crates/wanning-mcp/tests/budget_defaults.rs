//! W-43a 产品化:默认预算策略(保守默认 + `--budget` 覆盖,复用 W-27 语义)。
//!
//! 产品化要求:新用户不带任何参数启动 `wanning`(即 `wanning-mcp` 的默认参数)
//! 时,闸不能只是「上限 ¥10」——还要有保守的速率护栏(每天最多 N 笔成功消费),
//! 且该默认策略必须**随注册委托落审计**(WAL 里可核),不是藏在内存里的软约定。
//! 显式 `--max-spends 0` = 关掉速率护栏(退回 W-27 之前的行为,字节不漂移)。
//!
//! 这些测试打进程内 `McpServer`(lib 面);真实子进程的 stdio 路径在
//! `stdio_budget.rs`。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

use wanning_mcp::McpServer;

fn fresh_wal_path(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join("wanning-mcp");
    let path = dir.join(format!(
        "w43-budget-{}-{}-{}-{}.jsonl",
        tag,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间早于 Unix 纪元")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    path
}

/// 握手 + notifications/initialized(spec 约定:通知无响应)。
fn handshake(server: &mut McpServer) {
    let response = server
        .handle_line(
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","clientInfo":{"name":"wanning-mcp-tests","version":"0.0.0"}}}"#,
        )
        .expect("initialize 必有响应");
    let value: Value = serde_json::from_str(&response).expect("响应是合法 JSON");
    assert_eq!(
        value["result"]["protocolVersion"], "2025-06-18",
        "握手必须协商成功"
    );
    server.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
}

/// `tools/call wanning_gate_evaluate` 的请求体(nonce 唯一,金额固定,便于数笔数)。
fn evaluate(id: u64, nonce: u64) -> Value {
    json!({
        "jsonrpc":"2.0",
        "id": id,
        "method":"tools/call",
        "params":{
            "name":"wanning_gate_evaluate",
            "arguments":{
                "delegation_id":"demo-d1",
                "nonce": nonce,
                "amount_cents": 10,
                "merchant_id":"jd:shop-1",
                "category":"grocery",
                "memo":"W-43 默认速率护栏"
            }
        }
    })
}

/// 判一次,返回 `structuredContent`(decision/reason/budget_after_cents)。
fn decide(server: &mut McpServer, id: u64, nonce: u64) -> Value {
    let response = server
        .handle_line(&evaluate(id, nonce).to_string())
        .expect("tools/call 必有响应");
    let value: Value = serde_json::from_str(&response).expect("响应是合法 JSON");
    assert_eq!(value["error"], Value::Null, "请求形态合法,不应是协议错误");
    value["result"]["structuredContent"].clone()
}

#[test]
fn velocity_defaults_are_pinned() {
    // 默认策略是「产品默认值」,必须钉死为常量并落档:一天最多 10 笔成功消费。
    assert_eq!(wanning_mcp::DEFAULT_MAX_SPENDS_PER_DAY, 10);
    assert_eq!(wanning_mcp::DEFAULT_VELOCITY_WINDOW_SECS, 86_400);
}

#[test]
fn default_registration_rate_limits_the_eleventh_successful_spend() {
    let wal = fresh_wal_path("default-rate-limit");
    let mut server = McpServer::new(&wal).expect("默认参数启动应成功");
    handshake(&mut server);

    for nonce in 1..=10 {
        let outcome = decide(&mut server, nonce, nonce);
        assert_eq!(outcome["decision"], "allow", "前 10 笔都该放行");
    }
    let eleventh = decide(&mut server, 11, 11);
    assert_eq!(eleventh["decision"], "deny");
    assert_eq!(
        eleventh["reason"], "rate_limited",
        "第 11 笔成功消费应被默认速率护栏拒绝(实际:{eleventh})"
    );
}

#[test]
fn default_velocity_policy_lands_in_the_audit_registration() {
    // 默认策略不是内存软约定:必须随注册委托进 WAL,回放/审计都读得到。
    let wal = fresh_wal_path("default-policy-in-wal");
    let mut server = McpServer::new(&wal).expect("默认参数启动应成功");
    handshake(&mut server);

    let line = std::fs::read_to_string(&wal)
        .expect("WAL 应已创建")
        .lines()
        .next()
        .expect("注册行存在")
        .to_string();
    let record: Value = serde_json::from_str(&line).expect("WAL 行是合法 JSON");
    let velocity = &record["rec"]["delegation"]["policy"]["velocity"];
    assert_eq!(velocity["max_spends"], 10, "默认速率上限应落审计:{line}");
    assert_eq!(velocity["window_secs"], 86_400, "默认窗口应落审计:{line}");
}

#[test]
fn max_spends_zero_attaches_no_velocity_policy_and_never_rate_limits() {
    // `--max-spends 0` = 显式关闭速率护栏;WAL 的注册行不带 velocity 字段,
    // 与 W-27 之前的行为字节一致(默认策略不落 policy 字段的同一先例)。
    let wal = fresh_wal_path("zero-rate-limit");
    let mut server = McpServer::new_full(
        &wal,
        1_000,
        24,
        0,
        wanning_mcp::PayMode::default(),
        wanning_mcp::DEFAULT_PENDING_TTL_SECS,
    )
    .expect("启动应成功");
    handshake(&mut server);

    for nonce in 1..=12 {
        let outcome = decide(&mut server, nonce, nonce);
        assert_eq!(
            outcome["decision"], "allow",
            "关闭速率护栏后第 {nonce} 笔不该被拒(实际:{outcome})"
        );
    }

    let line = std::fs::read_to_string(&wal)
        .expect("WAL 应已创建")
        .lines()
        .next()
        .expect("注册行存在")
        .to_string();
    let record: Value = serde_json::from_str(&line).expect("WAL 行是合法 JSON");
    let policy = &record["rec"]["delegation"]["policy"];
    assert!(
        policy.get("velocity").is_none() || policy["velocity"].is_null(),
        "关闭速率护栏时注册行不应带 velocity 字段:{line}"
    );
}

#[test]
fn budget_override_caps_the_delegation_cap() {
    // `--budget`(产品主别名)覆盖默认上限,语义 = 旧 `--cap-cents`(W-27 总预算)。
    // 上限 500 分:花 10 分放行(budget_after = 累计已花 10),再花 495 分超额拒绝
    // (累计 505 > 500;恰满才算过,所以 500 恰好能花)。
    let wal = fresh_wal_path("budget-override");
    let mut server = McpServer::new_full(
        &wal,
        500,
        24,
        10,
        wanning_mcp::PayMode::default(),
        wanning_mcp::DEFAULT_PENDING_TTL_SECS,
    )
    .expect("启动应成功");
    handshake(&mut server);

    let within = decide(&mut server, 1, 1);
    assert_eq!(within["decision"], "allow");
    assert_eq!(
        within["budget_after_cents"], 10,
        "budget_after = 累计已花(10 分)"
    );

    let request = json!({
        "jsonrpc":"2.0",
        "id": 2,
        "method":"tools/call",
        "params":{
            "name":"wanning_gate_evaluate",
            "arguments":{
                "delegation_id":"demo-d1",
                "nonce": 2,
                "amount_cents": 495,
                "merchant_id":"jd:shop-1",
                "category":"grocery",
                "memo":"W-43 预算覆盖"
            }
        }
    });
    let response = server
        .handle_line(&request.to_string())
        .expect("tools/call 必有响应");
    let value: Value = serde_json::from_str(&response).expect("响应是合法 JSON");
    let outcome = value["result"]["structuredContent"].clone();
    assert_eq!(outcome["decision"], "deny");
    assert_eq!(
        outcome["reason"], "over_budget",
        "累计 10 + 495 = 505 > 500,应拒(实际:{outcome})"
    );
}
