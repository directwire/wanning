//! wanning-mcp 集成测试共用件:真实子进程 + 逐行 JSON-RPC 读写。
//!
//! 每个用例 spawn 真 bin(`--wal` 指向临时文件),逐行写 JSON-RPC、逐行读响应;
//! 「通知无响应」由『下一条响应必须属于下一个请求』来实证。

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

pub const PROTOCOL_VERSION: &str = "2025-06-18";

pub fn fresh_wal_path(tag: &str) -> PathBuf {
    // 名字带进程内原子序号:用例并行起跑,只靠「纳秒+pid」可能同 tick 撞名,
    // 两个用例抢同一把单写者锁,输的一方 WalLocked 起不来(W-21 顺带修)。
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join("wanning-mcp");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间早于 Unix 纪元")
        .as_nanos();
    dir.join(format!(
        "{tag}-{nanos}-{}-{}.jsonl",
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::process::id()
    ))
}

pub struct McpProc {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl McpProc {
    pub fn spawn(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_wanning-mcp"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wanning-mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    pub fn send_raw(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").expect("写 stdin");
        self.stdin.flush().expect("flush stdin");
    }

    pub fn send(&mut self, message: &Value) {
        self.send_raw(&message.to_string());
    }

    /// 读一行原始响应(调用前必须确信下一条响应存在,否则阻塞)。
    pub fn raw_response(&mut self) -> String {
        let mut line = String::new();
        let bytes = self.stdout.read_line(&mut line).expect("读 stdout");
        assert!(bytes > 0, "server 在等待响应时关闭了 stdout");
        line
    }

    pub fn response(&mut self) -> Value {
        let line = self.raw_response();
        serde_json::from_str(line.trim()).expect("响应必须是 JSON")
    }

    /// 标准握手:initialize → notifications/initialized(后者无响应,由后续请求实证)。
    pub fn handshake(&mut self) {
        self.send(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": PROTOCOL_VERSION, "capabilities": {},
                        "clientInfo": { "name": "wanning-mcp-tests", "version": "0.0.0" } }
        }));
        let init = self.response();
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);

        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    }

    /// 优雅收尾:关 stdin(spec:client 关输入流 → server 退出)并回收进程。
    pub fn shutdown(mut self) {
        drop(self.stdin);
        let _ = self.child.wait();
    }
}
