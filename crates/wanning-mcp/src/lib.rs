//! wanning-mcp:把 Wanning 闸语义暴露为 MCP(Model Context Protocol)server 骨架。
//!
//! 协议要点以官方 spec 为准(modelcontextprotocol.io,`2025-06-18` 版;2026-09-02 已逐条核对):
//! - 传输:stdio,每行一条 JSON-RPC 2.0 消息(UTF-8);MCP 2025-06-18 已移除
//!   JSON-RPC batching(changelog PR #416)——数组输入单条 `-32600` 拒绝、绝不逐条执行;
//! - 通知:无 `id` 的消息一律零响应(JSON-RPC 2.0 §4.1 MUST NOT reply),以通知形式
//!   发来的请求方法同样零响应且**不执行**(结果回不去的改账动作不盲做);
//! - 生命周期:`initialize` 请求(协议版本/能力/clientInfo)→ 服务器响应(版本/能力/serverInfo)
//!   → 客户端发 `notifications/initialized` 通知(通知无响应);
//! - 工具:`tools/list` / `tools/call`;未知工具 → JSON-RPC 错误 `-32602`(spec 原文示例);
//!   参数缺失等属**工具执行错误**,用 `isError: true` 的 result 表达(spec 原文)。
//!
//! **边界(P1 骨架,刻意最小)**:只暴露「闸评估」与「审计读取」两个工具——
//! 零网络、零渠道调用、零真实消费。真实支付永远不该出现在这个工具面上:
//! 闸的职责是判定与审计,消费动作由渠道 adapter(wanning-demo)在闸放行之后另行执行。
//! 撤销(kill switch)**不设工具**:那是所有者侧动作,agent 无权撤销自己的授权
//! (语义对齐 wanning-demo 决策回路的 `BossRevoke`)。
//!
//! **审计不可缺席(fail-closed)**:启动必须给 `--wal`;没有审计日志的闸不服务任何请求。

use std::path::Path;

use serde_json::{json, Value};

use wanning_core::clock::{Clock, SystemClock};
use wanning_core::delegation::Delegation;
use wanning_core::gate::GateDecision;
use wanning_core::intent::SpendIntent;
use wanning_core::policy::{SpendPolicy, VelocityLimit};
use wanning_core::state::WanningState;

/// 本 server 支持且仅支持的 MCP 协议版本(spec 2025-06-18)。
pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const SERVER_NAME: &str = "wanning-mcp";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const TOOL_EVALUATE: &str = "wanning_gate_evaluate";
pub const TOOL_AUDIT_TAIL: &str = "wanning_audit_tail";

/// 启动时注册的演示委托 id(所有者 → agent 的一次真实授权,进 WAL)。
pub const DEFAULT_DELEGATION_ID: &str = "demo-d1";
pub const DEFAULT_CAP_CENTS: u64 = 1_000; // ¥10.00(与 demo 总预算一致)
pub const DEFAULT_HOURS: u64 = 24;

/// W-43a 产品默认的速率护栏:一个滑动窗(`DEFAULT_VELOCITY_WINDOW_SECS`)内至多
/// 这么多笔**成功放行**。保守默认,`--max-spends` 可覆盖(0 = 关掉速率护栏)。
/// 只有成功放行计入,拒绝不占槽(W-27 语义);策略随注册委托落审计(不是内存
/// 软约定),续启同一 WAL 时以 WAL 里的原注册为准。
pub const DEFAULT_MAX_SPENDS_PER_DAY: u32 = 10;
/// 产品默认速率窗口:86 400 秒 = 一天(滑动窗,非自然日)。
pub const DEFAULT_VELOCITY_WINDOW_SECS: u64 = 86_400;

// JSON-RPC 2.0 / MCP 错误码。
const CODE_PARSE_ERROR: i64 = -32700;
const CODE_INVALID_REQUEST: i64 = -32600;
const CODE_METHOD_NOT_FOUND: i64 = -32601;
const CODE_INVALID_PARAMS: i64 = -32602;

/// MCP server:一条 JSON-RPC 消息进,零或一条响应出(通知无响应)。
pub struct McpServer {
    state: WanningState,
    initialized: bool,
}

impl McpServer {
    /// 默认参数启动(演示委托:上限 ¥10、有效期 24h、产品默认速率护栏)。
    pub fn new(wal_path: impl AsRef<Path>) -> Result<Self, wanning_core::error::CoreError> {
        Self::new_full(
            wal_path,
            DEFAULT_CAP_CENTS,
            DEFAULT_HOURS,
            DEFAULT_MAX_SPENDS_PER_DAY,
        )
    }

    /// 启动并注册演示委托(总预算/有效期可配;速率护栏用产品默认)。
    pub fn new_with(
        wal_path: impl AsRef<Path>,
        cap_cents: u64,
        hours: u64,
    ) -> Result<Self, wanning_core::error::CoreError> {
        Self::new_full(wal_path, cap_cents, hours, DEFAULT_MAX_SPENDS_PER_DAY)
    }

    /// 启动并注册演示委托,四个产品旋钮全可配(W-43a)。
    ///
    /// `max_spends_per_day` = 速率护栏(W-27 velocity 语义):滑动窗
    /// `DEFAULT_VELOCITY_WINDOW_SECS` 内至多这么多笔**成功放行**;`0` = 显式关掉
    /// 速率护栏(委托不带 velocity 策略,与 W-27 之前的注册行逐字节一致)。
    ///
    /// **必须挂 WAL**(fail-closed:没有审计的闸不服务)。
    ///
    /// 启动**可重入**:agent 平台重启会话、重连同一 `--wal` 是常态,二次启动必须
    /// ① 先整体回放旧 WAL 对账后**接续旧账**(`WanningState::live_resuming`——账本、
    /// 撤销、nonce 全部从审计恢复,绝不带空账本接着判)② 已注册的演示委托**跳过
    /// 注册**:WAL 里已有的注册是唯一事实,改预算=篡改审计;若该委托已被撤销,
    /// 重新注册等于把 kill switch 杀掉的授权复活(语义对齐 wanning-demo 决策回路的
    /// `BossRevoke`,撤销单向)。已撤销/已过期的演示委托继续被闸拒绝,
    /// 想重新演示请换一个新 WAL 路径。
    pub fn new_full(
        wal_path: impl AsRef<Path>,
        cap_cents: u64,
        hours: u64,
        max_spends_per_day: u32,
    ) -> Result<Self, wanning_core::error::CoreError> {
        let mut state = WanningState::live_resuming(wal_path)?;
        let now = SystemClock.now();
        let valid_until = now
            .checked_add(hours.saturating_mul(3600))
            .expect("有效期溢出:hours 配置过大");
        let mut delegation = Delegation::new(
            DEFAULT_DELEGATION_ID,
            "所有者",
            "mcp-client",
            cap_cents,
            now,
            valid_until,
            "agent:mcp-client",
        );
        if max_spends_per_day > 0 {
            delegation = delegation.with_policy(SpendPolicy {
                velocity: Some(VelocityLimit {
                    max_spends: max_spends_per_day,
                    window_secs: DEFAULT_VELOCITY_WINDOW_SECS,
                }),
                ..SpendPolicy::default()
            });
        }
        match state.register_delegation(delegation) {
            Ok(()) => {}
            // 已注册:沿用 WAL 里的原注册,不写任何新审计行。
            Err(wanning_core::error::CoreError::DuplicateDelegation(_)) => {}
            Err(e) => return Err(e),
        }
        Ok(Self {
            state,
            initialized: false,
        })
    }

    /// 已收到 `notifications/initialized`(测试用:生命周期状态可见)。
    pub fn initialized(&self) -> bool {
        self.initialized
    }

    /// 处理一行输入。返回 `Some(响应 JSON 文本)` 或 `None`(通知/空行,不发响应)。
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        let parsed: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(e) => {
                return Some(render_response(
                    Value::Null,
                    Err(jsonrpc_error(CODE_PARSE_ERROR, format!("Parse error: {e}"))),
                ))
            }
        };

        // MCP 2025-06-18 已移除 JSON-RPC batching(changelog PR #416):每条消息必须是
        // 单体 request/notification/response。数组不是合法 MCP 消息——单条 -32600 拒绝,
        // 且**绝不逐条执行**(否则 batch 里的 tools/call 会绕过本函数下面每一条分发纪律)。
        if parsed.is_array() {
            return Some(render_response(
                Value::Null,
                Err(jsonrpc_error(
                    CODE_INVALID_REQUEST,
                    "Invalid Request: JSON-RPC batching 已被 MCP 2025-06-18 移除(changelog \
                     PR #416);每行必须是单体 request/notification/response"
                        .into(),
                )),
            ));
        }

        // JSON-RPC 2.0 §4.1:「The Server MUST NOT reply to a Notification, including
        // those that are within a batch request.」通知(无 id)一律零响应——以通知形式
        // 发来的**请求方法**(ping/tools/call/…)同样零响应,且闸侧不执行:结果无处可回
        // 的改账动作不盲做(fail-closed;通知的 tools/call 若执行,写账却回不了执)。
        if parsed.get("id").is_none() {
            if parsed.get("method").and_then(Value::as_str) == Some("notifications/initialized") {
                self.initialized = true;
            }
            return None;
        }

        let id = parsed.get("id").cloned().unwrap_or(Value::Null);
        let method = match parsed.get("method").and_then(Value::as_str) {
            Some(method) => method.to_string(),
            None => {
                return Some(render_response(
                    id,
                    Err(jsonrpc_error(
                        CODE_INVALID_REQUEST,
                        "Invalid Request: 缺 method".into(),
                    )),
                ))
            }
        };

        match method.as_str() {
            "initialize" => Some(self.handle_initialize(&parsed, id)),
            "ping" => Some(render_response(id, Ok(json!({})))),
            "tools/list" => Some(render_response(id, Ok(self.tools_list()))),
            "tools/call" => match self.handle_tools_call(&parsed) {
                Ok(result) => Some(render_response(id, Ok(result))),
                Err(message) => Some(render_response(
                    id,
                    Err(jsonrpc_error(CODE_INVALID_PARAMS, message)),
                )),
            },
            // 未知请求按 Method not found 拒(通知已在上面零响应返回)。
            _ => Some(render_response(
                id,
                Err(jsonrpc_error(
                    CODE_METHOD_NOT_FOUND,
                    format!("Method not found: {method}"),
                )),
            )),
        }
    }

    /// spec「Version Negotiation」(modelcontextprotocol.io, 2025-06-18, Lifecycle):
    /// 「If the server supports the requested protocol version, it MUST respond with the
    /// same version. Otherwise, the server MUST respond with another protocol version it
    /// supports. This SHOULD be the latest version supported by the server.」
    /// ——协商**不是**报错:不支持来版就回自己支持的最高版,由客户端决定接受或断开。
    /// 实证(2026-09-02 P1 真插实测,字节级垫片抓包):Claude Code 2.1.234 提议
    /// `2025-11-25`,旧实现回 -32602 后客户端不重试,连接直接 failed。
    fn handle_initialize(&self, _parsed: &Value, id: Value) -> String {
        // 本 server 只支持一个版本:无论客户端提议什么,都回本版(= 所支持的最高版)。
        render_response(
            id,
            Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
                "instructions": "Wanning 支付闸:先 tools/list 看 wanning_gate_evaluate 的 schema;每次调用都会写审计 WAL 并可被回放对账。本 server 不触发任何真实支付。"
            })),
        )
    }

    fn tools_list(&self) -> Value {
        json!({
            "tools": [
                {
                    "name": TOOL_EVALUATE,
                    "title": "Wanning 闸评估",
                    "description": "把一笔消费意图交给闸判定(预算/有效期/撤销/重放),判定与拒绝都会写审计 WAL。只判定、不支付:本工具不访问网络、不触发任何真实消费。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "delegation_id": { "type": "string", "description": "委托 id(server 启动时注册,默认 demo-d1)" },
                            "nonce": { "type": "integer", "description": "意图 nonce(从 1 起);同一 scope 内只能成功消费一次,重复即 replay 拒绝" },
                            "amount_cents": { "type": "integer", "description": "金额,单位分(u64,禁浮点)" },
                            "merchant_id": { "type": "string", "description": "商户标识,落审计" },
                            "category": { "type": "string", "description": "消费类目(可省,默认 mcp)" },
                            "memo": { "type": "string", "description": "备注(可省,默认 mcp tools/call)" }
                        },
                        "required": ["delegation_id", "nonce", "amount_cents", "merchant_id"]
                    }
                },
                {
                    "name": TOOL_AUDIT_TAIL,
                    "description": "读取审计 WAL 的最后若干行(委托注册/放行/拒绝的时间线;每行含 seq/prev 完整性链字段,历史行被改动会导致 server 拒启),供对话内自查审计。",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "lines": { "type": "integer", "description": "取最后几行,默认 10" }
                        },
                        "required": []
                    }
                }
            ]
        })
    }

    /// spec 工具错误两分法:未知工具/协议层问题 → `Err`(JSON-RPC 错误);
    /// 参数缺失/业务失败 → `Ok(isError:true 的 result)`。
    fn handle_tools_call(&mut self, parsed: &Value) -> Result<Value, String> {
        let params = parsed.get("params").cloned().unwrap_or(json!({}));
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string);
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        match name.as_deref() {
            None => Ok(tool_error_result("tools/call 缺 name 参数")),
            Some(TOOL_EVALUATE) => Ok(self.evaluate(arguments)),
            Some(TOOL_AUDIT_TAIL) => Ok(self.audit_tail(arguments)),
            Some(other) => Err(format!("Unknown tool: {other}")),
        }
    }

    fn evaluate(&mut self, arguments: Value) -> Value {
        let Some(delegation_id) = get_str(&arguments, "delegation_id") else {
            return tool_error_result("缺少必填参数 delegation_id");
        };
        let Some(nonce) = get_u64(&arguments, "nonce") else {
            return tool_error_result("缺少必填参数 nonce");
        };
        let Some(amount_cents) = get_u64(&arguments, "amount_cents") else {
            return tool_error_result("缺少必填参数 amount_cents");
        };
        let Some(merchant_id) = get_str(&arguments, "merchant_id") else {
            return tool_error_result("缺少必填参数 merchant_id");
        };
        let category = get_str(&arguments, "category").unwrap_or_else(|| "mcp".to_string());
        let memo = get_str(&arguments, "memo").unwrap_or_else(|| "mcp tools/call".to_string());

        let intent = SpendIntent::new(
            delegation_id,
            nonce,
            amount_cents,
            merchant_id,
            category,
            memo,
        );
        match self.state.decide(&intent) {
            Ok(decision) => {
                let wal_line = self.state.last_wal_line();
                let state_hash = format!("{:x}", self.state.state_hash());
                let (text, structured) = match decision {
                    GateDecision::Allow { budget_after_cents } => (
                        format!(
                            "闸放行:金额 {amount_cents} 分,判后累计消费 {budget_after_cents} 分(审计 WAL 行 {wal_line:?},state_hash {state_hash})"
                        ),
                        json!({
                            "decision": "allow",
                            "budget_after_cents": budget_after_cents,
                            "wal_line": wal_line,
                            "state_hash": state_hash
                        }),
                    ),
                    GateDecision::Deny { reason } => (
                        format!(
                            "闸拒绝:{reason}(账本未动、nonce 不耗;审计 WAL 行 {wal_line:?})"
                        ),
                        json!({
                            "decision": "deny",
                            "reason": reason.to_string(),
                            "wal_line": wal_line,
                            "state_hash": state_hash
                        }),
                    ),
                };
                // spec:结构化结果应同时给一份 text 内容。
                json!({
                    "content": [ { "type": "text", "text": text } ],
                    "structuredContent": structured,
                    "isError": false
                })
            }
            Err(e) => tool_error_result(&format!("闸调用失败(fail-closed): {e}")),
        }
    }

    fn audit_tail(&mut self, arguments: Value) -> Value {
        let lines = get_u64(&arguments, "lines").unwrap_or(10).clamp(1, 1000) as usize;
        let Some(wal_path) = self.state.wal_path().map(Path::to_path_buf) else {
            return tool_error_result("本 server 未挂 WAL(不应发生:启动强制要求 --wal)");
        };
        let content = match std::fs::read_to_string(&wal_path) {
            Ok(text) => {
                let tail: Vec<&str> = text.lines().rev().take(lines).collect();
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            }
            Err(e) => return tool_error_result(&format!("读取审计 WAL 失败(fail-closed): {e}")),
        };
        json!({
            "content": [ { "type": "text", "text": content } ],
            "isError": false
        })
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC 组装
// ---------------------------------------------------------------------------

struct JsonRpcError {
    code: i64,
    message: String,
}

fn jsonrpc_error(code: i64, message: String) -> JsonRpcError {
    JsonRpcError { code, message }
}

fn render_response(id: Value, outcome: Result<Value, JsonRpcError>) -> String {
    let body = match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": error.code, "message": error.message }
        }),
    };
    body.to_string()
}

/// 工具执行错误(spec:`isError: true` 的 result,不是 JSON-RPC 错误)。
fn tool_error_result(message: &str) -> Value {
    json!({
        "content": [ { "type": "text", "text": message.to_string() } ],
        "isError": true
    })
}

fn get_str(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn get_u64(arguments: &Value, key: &str) -> Option<u64> {
    arguments.get(key).and_then(Value::as_u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试 server + WAL 路径(要断言 WAL 内容的用例用)。
    ///
    /// 名字带进程内原子序号:测试并行起跑,只靠「纳秒+pid」在 Windows 的时钟粒度下
    /// 可能同 tick 撞名 → 两个用例抢同一把单写者锁,输的一方 `WalLocked` panic
    /// (全仓门禁偶发一红、复跑全绿的元凶,2026-09-02 W-21 顺带修)。
    fn fresh_server_with_wal() -> (McpServer, std::path::PathBuf) {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join("wanning-mcp");
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join(format!(
            "unit-{}-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("系统时间早于 Unix 纪元")
                .as_nanos(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let server = McpServer::new(&path).expect("server 构建成功");
        (server, path)
    }

    fn fresh_server() -> McpServer {
        fresh_server_with_wal().0
    }

    #[test]
    fn notifications_of_request_methods_must_not_be_answered() {
        // JSON-RPC 2.0(jsonrpc.org/specification §4.1):「The Server MUST NOT reply
        // to a Notification, including those that are within a batch request.」
        // 请求方法以通知形式(无 id)发来时,同样无响应——连「缺 method」的纯通知也不回。
        let (mut server, _wal) = fresh_server_with_wal();
        for line in [
            r#"{"jsonrpc":"2.0","method":"ping"}"#,
            r#"{"jsonrpc":"2.0","method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/progress"}"#,
            r#"{"jsonrpc":"2.0"}"#,
        ] {
            assert!(server.handle_line(line).is_none(), "通知不得回响应: {line}");
        }
    }

    #[test]
    fn notification_form_tools_call_must_not_execute_nor_touch_ledger() {
        // 通知形式的 tools/call:结果无处可回,盲执行改账却回不了执——闸的 fail-closed
        // 语义要求**不执行、不写 WAL、不耗 nonce**;之后同一 nonce 以请求形式来必须照常放行。
        let (mut server, wal) = fresh_server_with_wal();
        let notification = json!({
            "jsonrpc": "2.0", "method": "tools/call",
            "params": { "name": TOOL_EVALUATE, "arguments": {
                "delegation_id": DEFAULT_DELEGATION_ID, "nonce": 1,
                "amount_cents": 500, "merchant_id": "jd:shop-1" } }
        })
        .to_string();
        assert!(
            server.handle_line(&notification).is_none(),
            "通知形式 tools/call 无响应"
        );

        let wal_lines = std::fs::read_to_string(&wal)
            .expect("读 WAL")
            .lines()
            .count();
        assert_eq!(
            wal_lines, 1,
            "通知形式的 tools/call 不得写 WAL(1 行=注册): {wal_lines}"
        );

        let request = json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": { "name": TOOL_EVALUATE, "arguments": {
                "delegation_id": DEFAULT_DELEGATION_ID, "nonce": 1,
                "amount_cents": 500, "merchant_id": "jd:shop-1" } }
        })
        .to_string();
        let response = server.handle_line(&request).expect("请求形式必有响应");
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["result"]["structuredContent"]["decision"], "allow",
            "nonce 未被通知消耗,照常放行: {response}"
        );
        assert_eq!(
            value["result"]["structuredContent"]["budget_after_cents"], 500,
            "账本未被通知动过"
        );
    }

    #[test]
    fn batch_input_is_single_invalid_request_and_executes_nothing() {
        // MCP 2025-06-18 changelog(PR #416):「Remove support for JSON-RPC batching」
        // ——消息必须是单体 request/notification/response。数组输入回单条 -32600,
        // 且**绝不执行**数组里的任何工具(batch 不是合法 MCP 消息,闸不判)。
        let (mut server, wal) = fresh_server_with_wal();
        let batch = json!([
            {
                "jsonrpc": "2.0", "id": 9, "method": "tools/call",
                "params": { "name": TOOL_EVALUATE, "arguments": {
                    "delegation_id": DEFAULT_DELEGATION_ID, "nonce": 1,
                    "amount_cents": 500, "merchant_id": "jd:shop-1" } }
            },
            { "jsonrpc": "2.0", "method": "notifications/initialized" }
        ])
        .to_string();
        let response = server.handle_line(&batch).expect("数组输入必有响应");
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["error"]["code"], CODE_INVALID_REQUEST, "{response}");
        let message = value["error"]["message"].as_str().unwrap();
        assert!(message.contains("batch"), "报错要点名 batching: {message}");

        let wal_lines = std::fs::read_to_string(&wal)
            .expect("读 WAL")
            .lines()
            .count();
        assert_eq!(
            wal_lines, 1,
            "batch 里的工具绝不被执行(1 行=注册): {wal_lines}"
        );

        // batch 里的「请求」没被偷偷判过:同 nonce 以请求形式再来仍照常放行。
        let request = json!({
            "jsonrpc": "2.0", "id": 10, "method": "tools/call",
            "params": { "name": TOOL_EVALUATE, "arguments": {
                "delegation_id": DEFAULT_DELEGATION_ID, "nonce": 1,
                "amount_cents": 500, "merchant_id": "jd:shop-1" } }
        })
        .to_string();
        let response = server.handle_line(&request).expect("请求形式必有响应");
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["result"]["structuredContent"]["decision"], "allow",
            "batch 不得替请求方消费 nonce: {response}"
        );
    }

    #[test]
    fn blank_lines_and_notifications_emit_nothing() {
        let mut server = fresh_server();
        assert!(server.handle_line("   ").is_none());
        assert!(!server.initialized());
        assert!(server
            .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());
        assert!(server.initialized());
    }

    #[test]
    fn unknown_notification_silently_ignored() {
        let mut server = fresh_server();
        assert!(server
            .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/nope"}"#)
            .is_none());
    }

    #[test]
    fn request_without_method_is_invalid_request() {
        let mut server = fresh_server();
        let response = server
            .handle_line(r#"{"jsonrpc":"2.0","id":3}"#)
            .expect("有响应");
        let value: Value = serde_json::from_str(&response).expect("JSON");
        assert_eq!(value["error"]["code"], CODE_INVALID_REQUEST);
        assert_eq!(value["id"], 3);
    }

    #[test]
    fn evaluate_flow_allow_then_replay_then_over_budget() {
        let mut server = fresh_server();

        let allow = server
            .handle_line(
                &json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": TOOL_EVALUATE, "arguments": {
                        "delegation_id": DEFAULT_DELEGATION_ID, "nonce": 1,
                        "amount_cents": 500, "merchant_id": "jd:shop-1" } }
                })
                .to_string(),
            )
            .expect("响应");
        let value: Value = serde_json::from_str(&allow).unwrap();
        assert_eq!(value["result"]["isError"], false);
        assert_eq!(value["result"]["structuredContent"]["decision"], "allow");
        assert_eq!(
            value["result"]["structuredContent"]["budget_after_cents"],
            500
        );
        assert_eq!(value["result"]["structuredContent"]["wal_line"], 2);
        assert!(value["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("放行"));

        let replay = server
            .handle_line(
                &json!({
                    "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                    "params": { "name": TOOL_EVALUATE, "arguments": {
                        "delegation_id": DEFAULT_DELEGATION_ID, "nonce": 1,
                        "amount_cents": 100, "merchant_id": "jd:shop-1" } }
                })
                .to_string(),
            )
            .expect("响应");
        let value: Value = serde_json::from_str(&replay).unwrap();
        assert_eq!(value["result"]["structuredContent"]["decision"], "deny");
        assert_eq!(value["result"]["structuredContent"]["reason"], "replay");

        let over = server
            .handle_line(
                &json!({
                    "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                    "params": { "name": TOOL_EVALUATE, "arguments": {
                        "delegation_id": DEFAULT_DELEGATION_ID, "nonce": 2,
                        "amount_cents": 10_000, "merchant_id": "jd:shop-1" } }
                })
                .to_string(),
            )
            .expect("响应");
        let value: Value = serde_json::from_str(&over).unwrap();
        assert_eq!(
            value["result"]["structuredContent"]["reason"],
            "over_budget"
        );
    }

    #[test]
    fn missing_arguments_are_tool_execution_errors_not_protocol_errors() {
        let mut server = fresh_server();
        let response = server
            .handle_line(
                &json!({
                    "jsonrpc": "2.0", "id": 4, "method": "tools/call",
                    "params": { "name": TOOL_EVALUATE, "arguments": { "delegation_id": "demo-d1" } }
                })
                .to_string(),
            )
            .expect("响应");
        let value: Value = serde_json::from_str(&response).unwrap();
        assert!(value["result"]["isError"].as_bool().unwrap());
        assert!(
            value["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("nonce"),
            "第一个缺失参数先报(delegation_id 已给): {response}"
        );
        assert!(value.get("error").is_none(), "参数缺失不是协议错误");
    }

    #[test]
    fn unknown_tool_yields_minus_32602_with_spec_wording() {
        let mut server = fresh_server();
        let response = server
            .handle_line(
                &json!({
                    "jsonrpc": "2.0", "id": 5, "method": "tools/call",
                    "params": { "name": "delete_everything", "arguments": {} }
                })
                .to_string(),
            )
            .expect("响应");
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["error"]["code"], CODE_INVALID_PARAMS);
        assert_eq!(
            value["error"]["message"].as_str().unwrap(),
            "Unknown tool: delete_everything"
        );
    }
}
