//! W-17 验收:仓库级 MCP 消费配置(docs/examples/ 下两份样例)的**契约测试**。
//!
//! 配置文件里写的命令与参数,必须能原样驱动真 server——配置烂了(工具名改了、
//! 参数拼错、包名不对)这里当场红,而不是等所有者在 Claude Code 里踩坑。
//!
//! `cargo run` 这条命令本身不在测试里 spawn:`cargo test` 持有 build 锁,子 cargo
//! 会死等锁。做法:解析配置 → 取其 `--` 之后的服务端参数 → 仅把 `--wal` 的取值换成
//! 本测试的临时 WAL(hermetic,可重复跑)→ 用 `CARGO_BIN_EXE_wanning-mcp` spawn 真
//! bin 走完整握手。`cargo run` 端到端实录实测记录在档。
//!
//! 各平台配置文件与字段依据(2026-09-02 直核,详见 docs/research/mcp-consumption.md):
//! - Claude Code:项目根 `.mcp.json`,字段 `type/command/args/env`;路径用
//!   `${CLAUDE_PROJECT_DIR:-.}` 展开(spawn 进程的 cwd 不保证是项目根)。
//! - Trae:项目根 `.trae/mcp.json`,字段 `command/args/env`;`${workspaceFolder}`
//!   是其文档声明的唯一支持变量。

mod common;

use std::path::{Path, PathBuf};

use common::{fresh_wal_path, McpProc};
use serde_json::{json, Value};

/// 仓库根(`crates/wanning-mcp` 上两级);用 parent 逐级上跳,别拼 `..`
/// (`Path::parent` 只去尾组件、不解析 `..`,拼出来的比较会错位)。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate 目录必有父级")
        .parent()
        .expect("crates 目录必有父级")
        .to_path_buf()
}

fn read_config(rel: &str) -> Value {
    let path = repo_root().join(rel);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读 {rel}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{rel} 不是合法 JSON: {e}"))
}

/// `mcpServers.wanning` 的 `args`(全字符串)。
fn server_args(config: &Value, rel: &str) -> Vec<String> {
    let entry = &config["mcpServers"]["wanning"];
    assert!(entry.is_object(), "{rel} 缺 mcpServers.wanning: {config}");
    entry["args"]
        .as_array()
        .unwrap_or_else(|| panic!("{rel} 缺 args 数组"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("{rel} args 必须全为字符串: {value}"))
                .to_string()
        })
        .collect()
}

/// 断言 args 形如 `[.., "--", "--wal", <wal>]` 并返回 `--` 之后的服务端参数。
fn server_flags<'a>(args: &'a [String], rel: &str) -> Vec<&'a str> {
    let split = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or_else(|| panic!("{rel} 缺 `--` 分隔符(cargo 参数与服务端参数)"));
    let flags = &args[split + 1..];
    assert_eq!(
        flags.first().map(String::as_str),
        Some("--wal"),
        "{rel} 服务端第一参数必须是 --wal(fail-closed:无审计不服务): {args:?}"
    );
    flags.iter().map(String::as_str).collect()
}

/// 闸零消费零密钥:MCP 配置不得携带任何 env 注入。
fn assert_no_env(entry: &Value, rel: &str) {
    assert!(
        entry["env"].as_object().is_none_or(|env| env.is_empty()),
        "{rel} 的 MCP 配置不得携带任何 env: {entry}"
    );
}

fn assert_cargo_run_args(args: &[String], rel: &str) {
    for expected in ["run", "--quiet", "-p", "wanning-mcp"] {
        assert!(
            args.iter().any(|arg| arg == expected),
            "{rel} 缺 cargo 参数 {expected}: {args:?}"
        );
    }
}

#[test]
fn claude_code_project_config_matches_server_contract() {
    let config = read_config("docs/examples/claude-code.mcp.json");
    let entry = &config["mcpServers"]["wanning"];

    // Claude Code:stdio 显式标注;command 走 PATH(cargo)。
    assert_eq!(entry["type"], "stdio", "stdio 传输(官方 docs 字段 type)");
    assert_eq!(entry["command"], "cargo");
    assert_no_env(entry, ".mcp.json");

    let args = server_args(&config, ".mcp.json");
    assert_cargo_run_args(&args, ".mcp.json");
    let flags = server_flags(&args, ".mcp.json");

    // WAL 路径必须用 `${CLAUDE_PROJECT_DIR:-.}` 锚定项目根——官方文档明确
    // 「不依赖 cwd」,裸相对路径落点随平台 cwd 漂移。
    let wal = flags[1];
    assert!(
        wal.starts_with("${CLAUDE_PROJECT_DIR:-.}"),
        ".mcp.json 的 --wal 必须用 ${{CLAUDE_PROJECT_DIR:-.}} 锚定项目根: {wal}"
    );
    // 展开后必须落在仓库 target/ 内(target/ 已 gitignore,审计不入库)。
    let root = repo_root().to_string_lossy().to_string();
    let expanded_string = wal.replace("${CLAUDE_PROJECT_DIR:-.}", &root);
    let expanded = Path::new(&expanded_string);
    let expected_dir = repo_root().join("target");
    assert_eq!(
        expanded.parent(),
        Some(expected_dir.as_path()),
        "默认 WAL 应在 <仓库根>/target/ 下: {expanded:?}"
    );
}

#[test]
fn trae_project_config_matches_server_contract() {
    let config = read_config("docs/examples/trae.mcp.json");
    let entry = &config["mcpServers"]["wanning"];

    // Trae 文档的 stdio 字段表只有 command/args/env(无 type 字段,不写)。
    assert_eq!(entry["command"], "cargo");
    assert_no_env(entry, ".trae/mcp.json");

    let args = server_args(&config, ".trae/mcp.json");
    assert_cargo_run_args(&args, ".trae/mcp.json");
    let flags = server_flags(&args, ".trae/mcp.json");

    // Trae 官方文档:${workspaceFolder} 是唯一支持的变量,启动时替换为项目根。
    let wal = flags[1];
    assert!(
        wal.starts_with("${workspaceFolder}"),
        ".trae/mcp.json 的 --wal 必须用 ${{workspaceFolder}} 锚定项目根: {wal}"
    );
}

#[test]
fn configured_command_drives_the_real_server() {
    // 取 Claude Code 配置里 `--` 之后的服务端参数,把 --wal 换成临时 WAL,
    // spawn 真 bin:配置写的参数契约必须被真 server 接受并产出判定。
    let config = read_config("docs/examples/claude-code.mcp.json");
    let args = server_args(&config, ".mcp.json");
    let flags = server_flags(&args, ".mcp.json");

    let wal = fresh_wal_path("mcp-json-config");
    let spawn_args: Vec<String> = vec![flags[0].to_string(), wal.to_string_lossy().to_string()];
    let spawn_args: Vec<&str> = spawn_args.iter().map(String::as_str).collect();

    let mut proc = McpProc::spawn(&spawn_args);
    proc.handshake();

    // 工具面契约:评估 + 审计,就这两个(支付/撤销永不进 MCP 面)。
    proc.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let value = proc.response();
    let names: Vec<&str> = value["result"]["tools"]
        .as_array()
        .expect("tools 数组")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(names, vec!["wanning_gate_evaluate", "wanning_audit_tail"]);

    // 一笔放行 + 一笔重放拒绝:配置驱动的 server 是真闸,不是空壳。
    proc.send(&json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "wanning_gate_evaluate", "arguments": {
            "delegation_id": "demo-d1", "nonce": 1,
            "amount_cents": 500, "merchant_id": "jd:shop-1" } }
    }));
    let value = proc.response();
    assert_eq!(value["result"]["isError"], false, "{value}");
    assert_eq!(value["result"]["structuredContent"]["decision"], "allow");
    assert_eq!(value["result"]["structuredContent"]["wal_line"], 2);

    proc.send(&json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": { "name": "wanning_gate_evaluate", "arguments": {
            "delegation_id": "demo-d1", "nonce": 1,
            "amount_cents": 100, "merchant_id": "jd:shop-1" } }
    }));
    let value = proc.response();
    assert_eq!(value["result"]["structuredContent"]["reason"], "replay");

    proc.shutdown();
}
