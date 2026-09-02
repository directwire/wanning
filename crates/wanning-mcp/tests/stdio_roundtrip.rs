//! wanning-mcp 验收:真实子进程 stdio 往返(零网络、零真实消费)。
//!
//! 每个用例 spawn 真 bin(`--wal` 指向临时文件),逐行写 JSON-RPC、逐行读响应;
//! 「通知无响应」由『下一条响应必须属于下一个请求』来实证。

mod common;

use std::process::{Command, Stdio};

use common::{fresh_wal_path, McpProc, PROTOCOL_VERSION};
use serde_json::{json, Value};

fn evaluate_call(id: i64, nonce: u64, amount_cents: u64, delegation_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": "wanning_gate_evaluate", "arguments": {
            "delegation_id": delegation_id, "nonce": nonce,
            "amount_cents": amount_cents, "merchant_id": "jd:shop-1",
            "category": "grocery", "memo": "mcp 冒烟" } }
    })
}

#[test]
fn initialize_lists_two_tools_with_schemas() {
    let mut proc = McpProc::spawn(&["--wal", &fresh_wal_path("init").to_string_lossy()]);
    proc.handshake();

    proc.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let value = proc.response();
    let tools = value["result"]["tools"].as_array().expect("tools 数组");
    assert_eq!(tools.len(), 2);
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"wanning_gate_evaluate"), "{names:?}");
    assert!(names.contains(&"wanning_audit_tail"), "{names:?}");

    let evaluate = tools
        .iter()
        .find(|t| t["name"] == "wanning_gate_evaluate")
        .expect("评估工具");
    assert_eq!(evaluate["inputSchema"]["type"], "object");
    let required = evaluate["inputSchema"]["required"].as_array().unwrap();
    assert!(required.contains(&json!("amount_cents")));
    assert!(required.contains(&json!("nonce")));

    proc.shutdown();
}

#[test]
fn gate_tool_allow_then_replay_then_over_budget_then_unknown_delegation() {
    let mut proc = McpProc::spawn(&["--wal", &fresh_wal_path("gate").to_string_lossy()]);
    proc.handshake();

    // ① 预算内放行(默认上限 ¥10 = 1000 分)。
    proc.send(&evaluate_call(10, 1, 500, "demo-d1"));
    let value = proc.response();
    assert_eq!(value["result"]["isError"], false, "{value}");
    assert_eq!(value["result"]["structuredContent"]["decision"], "allow");
    assert_eq!(
        value["result"]["structuredContent"]["budget_after_cents"],
        500
    );
    assert_eq!(
        value["result"]["structuredContent"]["wal_line"], 2,
        "行1=注册委托"
    );
    assert!(
        value["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("放行"),
        "{value}"
    );

    // ② 同 nonce 重放:拒绝(replay),账本不动。
    proc.send(&evaluate_call(11, 1, 100, "demo-d1"));
    let value = proc.response();
    assert_eq!(value["result"]["structuredContent"]["decision"], "deny");
    assert_eq!(value["result"]["structuredContent"]["reason"], "replay");

    // ③ 超额:1000 + 500 > 1000 → over_budget。
    proc.send(&evaluate_call(12, 2, 600, "demo-d1"));
    let value = proc.response();
    assert_eq!(
        value["result"]["structuredContent"]["reason"],
        "over_budget"
    );

    // ④ 未注册委托:unknown_delegation(agent 不能自授权)。
    proc.send(&evaluate_call(13, 1, 100, "agent-made-up-d1"));
    let value = proc.response();
    assert_eq!(
        value["result"]["structuredContent"]["reason"],
        "unknown_delegation"
    );

    proc.shutdown();
}

#[test]
fn audit_tail_reads_wal_after_decisions() {
    let wal = fresh_wal_path("audit");
    let mut proc = McpProc::spawn(&["--wal", &wal.to_string_lossy()]);
    proc.handshake();

    proc.send(&evaluate_call(20, 1, 300, "demo-d1"));
    let _ = proc.response(); // allow
    proc.send(&evaluate_call(21, 2, 900, "demo-d1"));
    let _ = proc.response(); // over_budget deny

    proc.send(&json!({
        "jsonrpc": "2.0", "id": 22, "method": "tools/call",
        "params": { "name": "wanning_audit_tail", "arguments": { "lines": 10 } }
    }));
    let value = proc.response();
    assert_eq!(value["result"]["isError"], false, "{value}");
    let text = value["result"]["content"][0]["text"]
        .as_str()
        .expect("文本");
    assert!(
        text.contains("budget_after"),
        "WAL 行要能看到预算轨迹: {text}"
    );
    let line_count = text.lines().count();
    assert_eq!(line_count, 3, "注册+allow+deny 共 3 行: {text}");

    proc.shutdown();
}

#[test]
fn protocol_errors_match_spec_wording_and_codes() {
    let mut proc = McpProc::spawn(&["--wal", &fresh_wal_path("proto").to_string_lossy()]);
    proc.handshake();

    // 未知工具 → -32602 "Unknown tool: …"(spec 原文示例)。
    proc.send(&json!({
        "jsonrpc": "2.0", "id": 30, "method": "tools/call",
        "params": { "name": "purge_ledger", "arguments": {} }
    }));
    let value = proc.response();
    assert_eq!(value["error"]["code"], -32602, "{value}");
    assert_eq!(
        value["error"]["message"].as_str().unwrap(),
        "Unknown tool: purge_ledger"
    );

    // 未知方法 → -32601。
    proc.send(&json!({ "jsonrpc": "2.0", "id": 31, "method": "resources/list" }));
    let value = proc.response();
    assert_eq!(value["error"]["code"], -32601, "{value}");

    // 坏 JSON 行 → -32700,id 为 null。
    proc.send_raw("这不是 JSON {");
    let value = proc.response();
    assert_eq!(value["error"]["code"], -32700, "{value}");
    assert!(value["id"].is_null());

    // ping → 空 result。
    proc.send(&json!({ "jsonrpc": "2.0", "id": 32, "method": "ping" }));
    let value = proc.response();
    assert!(value["result"].as_object().expect("空对象").is_empty());

    proc.shutdown();
}

#[test]
fn unsupported_protocol_version_negotiates_down_to_latest_supported() {
    // spec「Version Negotiation」规范条文:server 不支持来版时**必须**回「自己支持的
    // 另一个版本(应为最新)」,由客户端决定接受或断开——不是报错拒绝。
    // 实证(2026-09-02 P1 真插实测,字节级垫片抓包):Claude Code 2.1.234 提议
    // `2025-11-25`,收到 -32602 不按 supported 列表重试,直接判连接 failed。
    let mut proc = McpProc::spawn(&["--wal", &fresh_wal_path("ver").to_string_lossy()]);
    proc.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-11-25", "capabilities": {},
                    "clientInfo": { "name": "claude-code", "version": "2.1.234" } }
    }));
    let value = proc.response();
    assert!(value.get("error").is_none(), "协商不是协议错误: {value}");
    assert_eq!(
        value["result"]["protocolVersion"], PROTOCOL_VERSION,
        "{value}"
    );

    // 荒诞旧版本同样走协商(不是报错):server 永远回自己支持的最高版。
    proc.send(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "initialize",
        "params": { "protocolVersion": "1.0.0", "capabilities": {},
                    "clientInfo": { "name": "legacy", "version": "0" } }
    }));
    let value = proc.response();
    assert!(value.get("error").is_none(), "协商不是协议错误: {value}");
    assert_eq!(
        value["result"]["protocolVersion"], PROTOCOL_VERSION,
        "{value}"
    );
    proc.shutdown();
}

#[test]
fn missing_arguments_are_tool_execution_errors() {
    let mut proc = McpProc::spawn(&["--wal", &fresh_wal_path("args").to_string_lossy()]);
    proc.handshake();

    proc.send(&json!({
        "jsonrpc": "2.0", "id": 40, "method": "tools/call",
        "params": { "name": "wanning_gate_evaluate",
                    "arguments": { "delegation_id": "demo-d1", "nonce": 1 } }
    }));
    let value = proc.response();
    assert!(
        value["result"]["isError"].as_bool().expect("isError"),
        "{value}"
    );
    assert!(
        value["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("amount_cents"),
        "{value}"
    );
    assert!(value.get("error").is_none(), "参数缺失不属协议错误");

    proc.shutdown();
}

#[test]
fn server_refuses_to_start_without_wal() {
    let child = Command::new(env!("CARGO_BIN_EXE_wanning-mcp"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wanning-mcp");
    let output = child.wait_with_output().expect("等待退出");
    assert!(!output.status.success(), "没有 --wal 必须拒绝启动");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--wal"), "{stderr}");
    assert!(stderr.contains("fail-closed"), "{stderr}");
}

#[test]
fn zero_hours_config_is_rejected() {
    let wal = fresh_wal_path("zero-hours");
    let child = Command::new(env!("CARGO_BIN_EXE_wanning-mcp"))
        .args(["--wal", wal.to_string_lossy().as_ref(), "--hours", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wanning-mcp");
    let output = child.wait_with_output().expect("等待退出");
    assert!(!output.status.success(), "0 小时授权必须拒绝");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--hours"), "{stderr}");
}

// ---------------------------------------------------------------------------
// W-20(协议边界加固):通知不回响应、batch 拒绝且零执行——stdio 字节级实证
// ---------------------------------------------------------------------------

#[test]
fn notifications_of_request_methods_and_batches_stay_silent_or_rejected() {
    let wal = fresh_wal_path("edges");
    let mut proc = McpProc::spawn(&["--wal", &wal.to_string_lossy()]);
    proc.handshake();

    // ① 通知形式的 tools/call:零响应、零执行(JSON-RPC 2.0:MUST NOT reply;
    //    闸侧:结果回不去的改账动作不盲做)。
    proc.send(&json!({
        "jsonrpc": "2.0", "method": "tools/call",
        "params": { "name": "wanning_gate_evaluate", "arguments": {
            "delegation_id": "demo-d1", "nonce": 1,
            "amount_cents": 500, "merchant_id": "jd:shop-1" } }
    }));
    // ② 紧跟一个有 id 的请求:下一条响应必须属于它(通知不得插队出响应)。
    proc.send(&json!({ "jsonrpc": "2.0", "id": 100, "method": "ping" }));
    let value = proc.response();
    assert_eq!(
        value["id"], 100,
        "通知形式的 tools/call 不得产生响应: {value}"
    );
    assert!(value["result"].as_object().expect("空对象").is_empty());

    // ③ batch 数组行:MCP 2025-06-18 已移除 batching(changelog PR #416)→
    //    单条 -32600,数组里的工具零执行。
    proc.send_raw(concat!(
        r#"[{"jsonrpc":"2.0","id":101,"method":"tools/call","params":{"name":"wanning_gate_evaluate","arguments":{"delegation_id":"demo-d1","nonce":2,"amount_cents":300,"merchant_id":"jd:shop-1"}}},"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}]"#
    ));
    let value = proc.response();
    assert_eq!(value["error"]["code"], -32600, "{value}");
    assert!(
        value["id"].is_null(),
        "batch 输入回单条错误,id 为 null: {value}"
    );

    // ④ batch 里的意图从未被判定、①的通知也没扣账:nonce=2 请求形式 300 分照常
    //    放行,且 budget_after 恰为 300(若①或③被偷偷执行,这里对不上)。
    proc.send(&evaluate_call(102, 2, 300, "demo-d1"));
    let value = proc.response();
    assert_eq!(
        value["result"]["structuredContent"]["decision"], "allow",
        "{value}"
    );
    assert_eq!(
        value["result"]["structuredContent"]["budget_after_cents"], 300,
        "通知与 batch 都不得动账: {value}"
    );

    // ⑤ 审计对账:注册 + ④的放行 = 2 行;通知与 batch 各自零落账。
    proc.send(&json!({
        "jsonrpc": "2.0", "id": 103, "method": "tools/call",
        "params": { "name": "wanning_audit_tail", "arguments": { "lines": 100 } }
    }));
    let value = proc.response();
    let text = value["result"]["content"][0]["text"]
        .as_str()
        .expect("文本");
    assert_eq!(
        text.lines().count(),
        2,
        "注册+allow 两行,通知与 batch 零落账: {text}"
    );

    proc.shutdown();
}

// ---------------------------------------------------------------------------
// W-17(MCP 消费方式草图)带来的生命周期语义:平台重启会话是常态
// ---------------------------------------------------------------------------

#[test]
fn second_startup_on_same_wal_is_idempotent_and_state_survives() {
    // agent 平台重开会话、重连同一 --wal 是常态:二次启动必须照常服务,
    // 而不是被「重复注册」拒之门外(否则 .mcp.json 指向的 WAL 第二次就连不上)。
    let wal = fresh_wal_path("restart");
    let wal_str = wal.to_string_lossy().to_string();

    let mut first = McpProc::spawn(&["--wal", &wal_str]);
    first.handshake();
    first.send(&evaluate_call(50, 1, 400, "demo-d1"));
    let value = first.response();
    assert_eq!(
        value["result"]["structuredContent"]["decision"], "allow",
        "{value}"
    );
    first.shutdown();

    let mut second = McpProc::spawn(&["--wal", &wal_str]);
    second.handshake(); // 若重复注册仍 fail-closed 拒启,这里读不到任何响应
                        // 状态必须原样接续:nonce 1 已耗,重放照拒(不因重启而洗白)。
    second.send(&evaluate_call(51, 1, 100, "demo-d1"));
    let value = second.response();
    assert_eq!(
        value["result"]["structuredContent"]["reason"], "replay",
        "{value}"
    );
    // 且没有为「重复注册」多写一行审计:重启前后行数只随判定增长。
    second.send(&json!({
        "jsonrpc": "2.0", "id": 52, "method": "tools/call",
        "params": { "name": "wanning_audit_tail", "arguments": { "lines": 100 } }
    }));
    let value = second.response();
    let text = value["result"]["content"][0]["text"]
        .as_str()
        .expect("文本");
    let registrations = text
        .lines()
        .filter(|line| line.contains("register_delegation"))
        .count();
    assert_eq!(registrations, 1, "重复启动不得重复注册: {text}");

    second.shutdown();
}

#[test]
fn revoked_delegation_is_never_resurrected_by_restart() {
    // 预置一份 demo-d1 已撤销的 WAL(撤销只能老板侧做:MCP 工具面没有撤销)。
    use wanning_core::clock::{Clock, SystemClock};
    use wanning_core::delegation::Delegation;
    use wanning_core::state::WanningState;

    let wal = fresh_wal_path("revoked");
    {
        let mut state = WanningState::live(&wal).expect("建 WAL");
        let now = SystemClock.now();
        state
            .register_delegation(Delegation::new(
                "demo-d1",
                "老板",
                "mcp-client",
                1_000,
                now,
                now.checked_add(3_600).expect("有效期溢出"),
                "agent:mcp-client",
            ))
            .expect("注册");
        state.revoke("demo-d1").expect("撤销");
    }

    // 二次启动照常服务,但重新注册不得把 kill switch 杀掉的授权复活:
    // 撤销单向,重启后同一委托继续被拒(reason=revoked)。
    let mut proc = McpProc::spawn(&["--wal", &wal.to_string_lossy()]);
    proc.handshake();
    proc.send(&evaluate_call(60, 1, 100, "demo-d1"));
    let value = proc.response();
    assert_eq!(
        value["result"]["structuredContent"]["decision"], "deny",
        "{value}"
    );
    assert_eq!(
        value["result"]["structuredContent"]["reason"], "revoked",
        "{value}"
    );
    proc.shutdown();
}

// ---------------------------------------------------------------------------
// W-18(单写者锁):两个平台并挂同一份 WAL 是真实场景,第二个 server 必须拒启
// ---------------------------------------------------------------------------

#[test]
fn server_refuses_to_start_while_another_process_holds_the_wal() {
    use wanning_core::clock::{Clock, SystemClock};
    use wanning_core::delegation::Delegation;
    use wanning_core::state::WanningState;
    use wanning_core::wal::raw_lines;

    let wal = fresh_wal_path("locked");

    // 另一个「平台」的 server 正在服务(持锁进程,不 drop)。
    let mut holder = WanningState::live(&wal).expect("持锁进程开张");
    let now = SystemClock.now();
    holder
        .register_delegation(Delegation::new(
            "demo-d1",
            "老板",
            "mcp-client",
            1_000,
            now,
            now.checked_add(3_600).expect("有效期溢出"),
            "agent:mcp-client",
        ))
        .expect("注册");

    // 持锁期间 spawn 第二个 server:必须 fail-closed 拒启(退出码非 0、报错可读),
    // 而不是各自拿一张内存账各判各的(预算硬上限会被合力突破,见 core 测试)。
    let output = Command::new(env!("CARGO_BIN_EXE_wanning-mcp"))
        .args(["--wal", &wal.to_string_lossy()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn wanning-mcp");
    assert!(!output.status.success(), "持锁期间必须拒启: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("单写者锁"), "报错要点名单写者锁: {stderr}");
    assert_eq!(
        raw_lines(&wal).expect("读 WAL").len(),
        1,
        "被拒进程不得动审计(一行 = 持锁方的注册)"
    );

    // 持锁方释放后,server 照常能起、照常服务(锁不楔死 WAL)。
    drop(holder);
    let mut proc = McpProc::spawn(&["--wal", &wal.to_string_lossy()]);
    proc.handshake();
    proc.send(&evaluate_call(70, 1, 100, "demo-d1"));
    let value = proc.response();
    assert_eq!(
        value["result"]["structuredContent"]["decision"], "allow",
        "{value}"
    );
    proc.shutdown();
}
