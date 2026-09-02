//! 本地 JSON mock HTTP server:demo 场景运行时(W-29 `--scenario full-loop-mock`)
//! 与集成测试(W-08/W-10/W-11 起)**共用同一份实现**,原在 `tests/common/`,场景也要
//! 在运行时起本地 mock 才抽进 lib(与 `http.rs` 同一先例:第二个使用方出现才抽)。
//!
//! 边界:只 bind `127.0.0.1:0`(内核挑空闲端口,零外网);按队列依次应答,队列耗尽
//! 即停;这不是通用 HTTP mock 库,只服务本仓 demo 与测试。报文都是**本地 mock 契约**
//! (wanning-demo 自定义字段),不是任何渠道的真实报文。

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// 本地 JSON mock:按队列依次应答,并记录收到的原始请求。
pub struct MockJsonServer {
    pub addr: SocketAddr,
    pub requests: Arc<Mutex<Vec<String>>>,
}

/// 一个应答 = (HTTP 状态码, 响应体)。
pub type MockResponse = (u16, String);

pub fn spawn_json_mock(responses: Vec<MockResponse>) -> MockJsonServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind 本地 mock");
    let addr = listener.local_addr().expect("mock 地址");
    let queue = Arc::new(Mutex::new(VecDeque::from(responses)));
    let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let requests_for_thread = requests.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let Some((status, body)) = queue.lock().expect("queue").pop_front() else {
                break;
            };
            let request = read_http_request(&stream);
            requests_for_thread.lock().expect("requests").push(request);
            let mut stream = stream;
            let reason = match status {
                200 => "OK",
                400 => "Bad Request",
                500 => "Internal Server Error",
                _ => "Error",
            };
            let http = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(http.as_bytes());
            let _ = stream.flush();
        }
    });

    MockJsonServer { addr, requests }
}

impl MockJsonServer {
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn recorded_requests(&self) -> Vec<String> {
        self.requests.lock().expect("requests").clone()
    }
}

/// 读完整 HTTP 请求(header + Content-Length 指定的 body;兼容 Expect: 100-continue)。
fn read_http_request(stream: &TcpStream) -> String {
    let mut stream = stream;
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut buf).expect("读请求头");
        if n == 0 {
            panic!("客户端在 header 结束前断开");
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(pos) = find_subslice(&data, b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let headers = String::from_utf8_lossy(&data[..header_end]).to_ascii_lowercase();
    if headers.contains("expect: 100-continue") {
        let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
    }
    let content_length: usize = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:")?.trim().parse().ok())
        .unwrap_or(0);
    while data.len() < header_end + content_length {
        let n = stream.read(&mut buf).expect("读请求体");
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }
    String::from_utf8_lossy(&data).to_string()
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
