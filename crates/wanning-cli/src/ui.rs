//! `wanning ui` —— 本地只读仪表盘(W-43b 产品化 GUI 层)。
//!
//! 任务书(`W-43-production-ready.md` §C.8):本地 HTTP 服务,默认 `127.0.0.1`
//! **随机端口**、不监听外网;复用 W-22 审计回放页;实时 tail;撤销按钮走闸本体
//! 同库语义。零新增依赖——手写最小 HTTP/1.1(std `TcpListener`),复用 W-22 的
//! 渲染与口径([`audit_html::build_report`] / [`audit_html::render_html`] /
//! 转义 / 金额 / 时刻 / 链值),不写第二份实现。
//!
//! 面边界(与 W-15 MCP 面「撤销不设工具」同一纪律,只是可信方不同):
//!
//! - **读路径零持锁**:每次请求都完整重跑「验链 → 回放对账两遍」
//!   ([`audit_html::build_report`]),坏账当场亮横幅并隐藏全部撤销表单——
//!   仪表盘显示的是账本事实,不是任何进程的内存态。完整性链逐行验天然没法
//!   「只验增量」(链值引用前一行),增量只用于展示(最新 N 行)。
//! - **写路径只有撤销一个动作**,且走 [`WanningState::live_resuming`] +
//!   [`WanningState::revoke`](WanningState::revoke) 闸本体:write-ahead 落审计、
//!   回放对账 fail-closed、单写者锁,与 demo/MCP/SDK 是同一份实现,零新语义。
//! - **锁冲突 = 拒绝而不是排队**:另一个写进程(wanning-mcp)持锁期间撤销被拒
//!   (409)。这不是可用性疣而是设计:撤销永不作用于一个**活着的闸进程的内存态**
//!   ——活闸不知道这笔撤销,预算照扣,「以为撤了其实没撤到活进程」比撤不了危险
//!   得多。正确动作 = 停掉 MCP server → 撤销(落审计)→ 重启(回放账本,撤销生效)。
//! - **跨站防护三道**(本地服务不是免死金牌:浏览器会替任意网页向
//!   `http://127.0.0.1:端口/` 发跨站表单 POST):①只监听回环;②`Host` 必须是
//!   `127.0.0.1`/`localhost`(DNS rebinding 挂别的域名直接 403);③`Origin` 若出现
//!   必须同源 + POST 必须带进程内随机令牌(随机源 = std `RandomState` 的 OS 熵种子;
//!   令牌只进页面表单,外部页面读不到)。三道全过才碰闸。
//! - **资源有界**(W-41 复审教训「行长无上限」不再重演):请求头 32 KiB、表单体
//!   16 KiB、读超时 10 s、并发连接 32,超限一律断掉,零内存放大。
//!
//! 页面零 JS(meta refresh 自动刷新)、零外链;撤销走 POST→303→GET 的标准往返。

use std::io::{BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use wanning_core::clock::{Clock, SystemClock};
use wanning_core::error::CoreError;
use wanning_core::pending::PendingOutcome;
use wanning_core::state::WanningState;
use wanning_core::wal::{WalDecision, WalRecord};
use wanning_demo::audit_html;
use wanning_demo::scenario::deny_reason_zh;

/// 请求头(含请求行)上限:超限 431 断开,绝不无界读入内存。
const MAX_HEAD_BYTES: usize = 32 * 1024;
/// POST 表单体上限:撤销表单只有两个小字段,16 KiB 绰绰有余。
const MAX_BODY_BYTES: usize = 16 * 1024;
/// 实时滚动条数(最新在前);完整时间线在 `/audit` 回放页。
const TAIL_ROWS: usize = 50;
/// 页面自动刷新间隔(秒)。零 JS:`meta refresh`,浏览器原生行为。
const REFRESH_SECS: u32 = 2;
/// 单连接读超时:半开连接不吊死线程。
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// 并发连接上限:超过直接丢弃(本地单用户仪表盘,浏览器会重试)。
const MAX_CONCURRENT_CONNECTIONS: usize = 32;
/// accept 轮询间隔(nonblocking accept + stop 旗标,零额外依赖)。
const ACCEPT_POLL: Duration = Duration::from_millis(25);

// ---------------------------------------------------------------------------
// CLI 面:参数解析与启动
// ---------------------------------------------------------------------------

/// `wanning ui` 的启动参数错误:用法错(退出码 2)与运行失败(退出码 1)。
#[derive(Debug, PartialEq, Eq)]
pub enum UiStartError {
    Usage(String),
    Failed(String),
}

/// 解析 `wanning ui` 参数并启动仪表盘(供 CLI 与测试共用;CLI 侧随后阻塞等停)。
///
/// 默认账本 = [`wanning_core::paths::default_wal_path`](产品默认
/// `~/.wanning/wal.jsonl`);家目录解析不出 = fail-closed,绝不猜落点。
/// 账本不存在 = 拒启(与 `wanning audit` 同一口径:闸还没跑过判定,仪表盘无事可看)。
/// 端口默认随机(`127.0.0.1:0`),`--port` 显式固定。
pub fn start_from_args(args: &[String]) -> Result<UiServer, UiStartError> {
    let mut wal: Option<PathBuf> = None;
    let mut port: Option<u16> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--wal" => {
                wal = Some(PathBuf::from(next_value(args, &mut index, "--wal")?));
            }
            "--port" => {
                let raw = next_value(args, &mut index, "--port")?;
                port = Some(raw.parse::<u16>().map_err(|_| {
                    UiStartError::Usage(format!("--port 要 0..=65535 的端口号,收到 '{raw}'"))
                })?);
            }
            other => {
                return Err(UiStartError::Usage(format!(
                    "未知参数 '{other}'(用法:wanning ui [--wal <账本>] [--port <端口>])"
                )));
            }
        }
        index += 1;
    }

    let wal = match wal {
        Some(wal) => wal,
        None => wanning_core::paths::default_wal_path().ok_or_else(|| {
            UiStartError::Failed(
                "解析不出默认账本路径(WANNING_HOME / USERPROFILE / HOME 都没有)。\
                 用 `wanning ui --wal <账本路径>` 显式给一个"
                    .to_string(),
            )
        })?,
    };
    if !wal.exists() {
        return Err(UiStartError::Failed(format!(
            "审计账本不存在:{}(闸还没跑过任何判定?先 `wanning init` 挂上闸,或显式给账本路径)",
            crate::slash(&wal)
        )));
    }

    UiServer::start_on(wal, port.unwrap_or(0)).map_err(|e| {
        UiStartError::Failed(format!(
            "仪表盘启动失败(127.0.0.1,端口 {}): {e}",
            port.map(|p| p.to_string())
                .unwrap_or_else(|| "随机".to_string())
        ))
    })
}

fn next_value(args: &[String], index: &mut usize, flag: &str) -> Result<String, UiStartError> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| UiStartError::Usage(format!("{flag} 缺少取值(用 --help 看用法)")))
}

/// `wanning ui` 的 CLI 主体:启动、打印引导、阻塞到停止(Ctrl+C / [`UiServer::stop`])。
pub fn run(args: &[String]) -> Result<(), UiStartError> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print!("{}", crate::USAGE);
        return Ok(());
    }
    let server = start_from_args(args)?;
    let url = format!("http://{}/", server.addr());
    println!("Wanning 仪表盘:{url}(只监听 127.0.0.1,不监听外网)");
    println!(
        "  账本:{} —— 每次请求都重新验链 + 回放对账,坏账当场亮横幅并隐藏撤销",
        crate::slash(server.wal_path())
    );
    println!(
        "  页面零 JS,每 {REFRESH_SECS} 秒自动刷新;撤销走闸本体(落审计/单写者锁/live_resuming 对账)"
    );
    println!("  闸进程(wanning-mcp)持锁期间撤销会被拒:先停掉它,撤销后重启即刻生效");
    println!("  Ctrl+C 退出。");
    server.join();
    Ok(())
}

// ---------------------------------------------------------------------------
// 服务本体
// ---------------------------------------------------------------------------

/// 本地仪表盘服务句柄:CLI 阻塞等待;测试用它拿地址/令牌、打真实 HTTP、停服。
pub struct UiServer {
    addr: SocketAddr,
    token: String,
    wal: PathBuf,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl UiServer {
    /// 启动仪表盘,绑定 `127.0.0.1:0`(随机端口)。
    pub fn start(wal: impl Into<PathBuf>) -> std::io::Result<Self> {
        Self::start_on(wal, 0)
    }

    /// 启动仪表盘,绑定 `127.0.0.1:<port>`;`port = 0` 即随机端口。
    ///
    /// 绑定面在代码里钉死为回环:`is_loopback` 自检不过直接报错——「不监听外网」
    /// 不是约定,是构造保证(连一个非回环的绑定面都不存在)。
    pub fn start_on(wal: impl Into<PathBuf>, port: u16) -> std::io::Result<Self> {
        let wal = wal.into();
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))?;
        let addr = listener.local_addr()?;
        if !addr.ip().is_loopback() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                "仪表盘只允许绑定 127.0.0.1(不监听外网)",
            ));
        }
        let token = new_csrf_token();
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let stop = Arc::clone(&stop);
            let token = token.clone();
            let wal = wal.clone();
            std::thread::Builder::new()
                .name("wanning-ui".to_string())
                .spawn(move || accept_loop(listener, stop, token, wal))?
        };
        Ok(Self {
            addr,
            token,
            wal,
            stop,
            worker: Some(worker),
        })
    }

    /// 实际绑定地址(端口随机时,这里拿到的是真实端口)。
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 审计账本路径(仪表盘展示与撤销都对着它)。
    pub fn wal_path(&self) -> &Path {
        &self.wal
    }

    /// 本进程的表单令牌(撤销 POST 必带;只存在于本进程内存与页面表单里)。
    pub fn token(&self) -> &str {
        &self.token
    }

    /// 请求停止(accept 循环在 [`ACCEPT_POLL`] 内退出;已在跑的请求自然结束)。
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// 等待 accept 循环退出。
    pub fn join(mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for UiServer {
    fn drop(&mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// 进程内随机令牌。随机源 = std `RandomState` 的种子(OS 熵,防 HashDoS 的同一
/// 来源),两次独立取样拼 128 位十六进制。诚实边界:这是**本地 CSRF 闸**的令牌,
/// 不是密码学承诺——它要挡的是「别的网页借浏览器发跨站 POST」,不是持钥攻击者。
fn new_csrf_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut first = RandomState::new().build_hasher();
    first.write(b"wanning-ui-csrf");
    let a = first.finish();
    let mut second = RandomState::new().build_hasher();
    second.write_u64(a);
    let b = second.finish();
    format!("{a:016x}{b:016x}")
}

/// accept 循环:nonblocking + 轮询 stop 旗标(零额外依赖的停服);每连接一线程,
/// 并发超上限直接丢弃。
fn accept_loop(listener: TcpListener, stop: Arc<AtomicBool>, token: String, wal: PathBuf) {
    let _ = listener.set_nonblocking(true);
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        match listener.accept() {
            Ok((stream, _peer)) => {
                if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONCURRENT_CONNECTIONS {
                    active.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                let token = token.clone();
                let wal = wal.clone();
                let conn_active = Arc::clone(&active);
                let spawned = std::thread::Builder::new()
                    .name("wanning-ui-conn".to_string())
                    .spawn(move || {
                        let _ = handle_connection(stream, &token, &wal);
                        conn_active.fetch_sub(1, Ordering::SeqCst);
                    });
                if spawned.is_err() {
                    active.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Err(_) => std::thread::sleep(ACCEPT_POLL),
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP:解析(全部有界)与分发
// ---------------------------------------------------------------------------

/// 请求头读取结果:`TooLarge` = 超上限(431),`ClientGone` = 客户端断开/超时
/// (直接断,不回写——对端都读不到了)。
enum HeadOutcome {
    Head(Vec<u8>),
    TooLarge,
    ClientGone,
}

fn read_head(reader: &mut BufReader<TcpStream>) -> HeadOutcome {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return HeadOutcome::ClientGone,
            Ok(_) => {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    return HeadOutcome::Head(head);
                }
                if head.len() > MAX_HEAD_BYTES {
                    return HeadOutcome::TooLarge;
                }
            }
            Err(_) => return HeadOutcome::ClientGone,
        }
    }
}

/// 一次 HTTP 请求解析后的形态。
struct Request {
    method: String,
    path: String,
    host: Option<String>,
    origin: Option<String>,
    content_length: Option<usize>,
}

enum ParseOutcome {
    Request(Request),
    /// 带状态行与说明的直接回给客户端的错误。
    Bad(&'static str, String),
}

fn parse_request(head: &[u8]) -> ParseOutcome {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let (method, target, version) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    if method.is_empty() || target.is_empty() || !version.starts_with("HTTP/1") {
        return ParseOutcome::Bad("400 Bad Request", "请求行不合法".to_string());
    }

    let mut host: Option<String> = None;
    let mut origin: Option<String> = None;
    let mut content_length: Option<usize> = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return ParseOutcome::Bad("400 Bad Request", "请求头不合法".to_string());
        };
        let value = value.trim().to_string();
        match name.trim().to_ascii_lowercase().as_str() {
            "host" => host = Some(value),
            "origin" => origin = Some(value),
            "content-length" => {
                let Ok(length) = value.parse::<usize>() else {
                    return ParseOutcome::Bad(
                        "400 Bad Request",
                        "Content-Length 不是非负整数".to_string(),
                    );
                };
                if content_length.replace(length).is_some() {
                    return ParseOutcome::Bad(
                        "400 Bad Request",
                        "Content-Length 出现多次".to_string(),
                    );
                }
            }
            _ => {}
        }
    }

    ParseOutcome::Request(Request {
        method: method.to_ascii_uppercase(),
        path: target.split('?').next().unwrap_or_default().to_string(),
        host,
        origin,
        content_length,
    })
}

/// 单连接处理:解析 → 门禁(Host/Origin)→ 分发 → 回写 → **干净断开**。
fn handle_connection(stream: TcpStream, token: &str, wal: &Path) -> std::io::Result<()> {
    // Windows 上 accept 出来的流**继承监听 socket 的 nonblocking 模式**(accept 循环
    // 轮询停服,监听者必为 nonblocking)。不显式恢复阻塞,请求字节还在路上时
    // `read` 会立刻返回 `WouldBlock`,read_head 把它当客户端断开提前关连接——
    // 客户端看到的是 10054 ConnectionReset。本机全量门禁实测抓到过,不是理论。
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let result = route(&mut writer, &mut reader, token, wal);

    // 关闭序列(fail-safe,每条连接无差别执行):flush 后**显式半关闭**发出 FIN,
    // 再把客户端方向的剩余字节排干到 EOF 才放掉 socket。Windows 对「接收队列还有
    // 未读数据」的 socket 执行 close 会直接发 RST,客户端连刚写出的响应都收不到
    // (10054 ConnectionReset,本机全量门禁实测抓到过);排干之后 close,对端看到
    // 的才恒是 FIN 而不是 RST。排干有 READ_TIMEOUT 兜底:恶意客户端最多占住一个
    // 线程一个超时周期,且并发上限封顶,资源有界。
    let _ = writer.flush();
    let _ = writer.shutdown(std::net::Shutdown::Write);
    let mut sink = [0u8; 4096];
    while matches!(reader.read(&mut sink), Ok(n) if n > 0) {}
    result
}

/// 读请求头 → 解析 → 收体 → 门禁(Host/Origin)→ 分发 → 回写。响应写完即返回,
/// 关闭序列在调用方 [`handle_connection`](self)。
fn route(
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    token: &str,
    wal: &Path,
) -> std::io::Result<()> {
    let head = match read_head(reader) {
        HeadOutcome::Head(head) => head,
        HeadOutcome::TooLarge => {
            return respond_simple(
                writer,
                "431 Request Header Fields Too Large",
                "请求头超过上限",
            );
        }
        HeadOutcome::ClientGone => return Ok(()),
    };
    let request = match parse_request(&head) {
        ParseOutcome::Request(request) => request,
        ParseOutcome::Bad(status, detail) => return respond_simple(writer, status, &detail),
    };

    // 先收体再路由(体有界,见 [`MAX_BODY_BYTES`]):后面的门禁拒绝/404/405
    // 也要在响应后**干净关连接**——Windows 对「接收队列还有未读数据」的 socket
    // 执行 close 会发 RST,客户端连错误响应都收不到(10054 ConnectionReset,
    // 本机全量门禁实测抓到过)。声明的体超上限才直接甩 413(滥用客户端,断得对)。
    let body = match request.content_length {
        Some(n) if n > MAX_BODY_BYTES => {
            return respond_simple(writer, "413 Payload Too Large", "表单体超过上限");
        }
        Some(n) => {
            let mut buf = vec![0u8; n];
            if reader.read_exact(&mut buf).is_err() {
                // 体读到一半客户端没了:没有可响应的对象,直接断。
                return Ok(());
            }
            buf
        }
        None => Vec::new(),
    };

    // 门禁第一道:Host 必须是回环名(DNS rebinding 把攻击者域名指到 127.0.0.1
    // 时,Host 是攻击者域名,当场 403)。缺失(HTTP/1.0 裸客户端)不拦——rebinding
    // 必带 Host,拦的是它。
    let port = writer.local_addr()?.port();
    if let Some(host) = &request.host {
        let allowed = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
        if !allowed.contains(host) {
            return respond_simple(writer, "403 Forbidden", "Host 不是本机回环地址");
        }
    }
    // 门禁第二道:Origin 若出现必须同源(浏览器跨站 POST 一定带 Origin)。
    if let Some(origin) = &request.origin {
        let allowed = [
            format!("http://127.0.0.1:{port}"),
            format!("http://localhost:{port}"),
        ];
        if !allowed.contains(origin) {
            return respond_simple(writer, "403 Forbidden", "Origin 跨站,已拒绝");
        }
    }

    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => {
            let (status, page) = dashboard(wal, token);
            respond_html(writer, status, &page)
        }
        ("GET", "/audit") => {
            let page = audit_page(wal);
            respond_html(writer, "200 OK", &page)
        }
        ("POST", "/revoke") => {
            if request.content_length.is_none() {
                return respond_simple(writer, "411 Length Required", "缺少 Content-Length");
            }
            revoke(writer, token, wal, &body)
        }
        ("GET", _) | ("POST", _) => respond_simple(writer, "404 Not Found", "没有这个页面"),
        _ => respond_simple(writer, "405 Method Not Allowed", "方法不允许"),
    }
}

// ---------------------------------------------------------------------------
// 路由:仪表盘 / 回放页 / 撤销
// ---------------------------------------------------------------------------

/// 仪表盘:账本可信 → 完整视图(预算台账 + 实时滚动 + 撤销表单);
/// 账本不可信 → fail-closed 横幅,隐藏全部撤销表单(对不了账的账本,一个写动作
/// 都不给)。两种形态都是 200:页面本身可达,坏的是账本——横幅明示。
fn dashboard(wal: &Path, token: &str) -> (&'static str, String) {
    match audit_html::build_report(wal) {
        Ok(mut report) => {
            report.generated_at_unix = Some(SystemClock.now());
            ("200 OK", render_dashboard(wal, &report, token))
        }
        Err(err) => ("200 OK", render_dashboard_failed(wal, &err)),
    }
}

/// `/audit`:W-22 审计回放页原样复用(完整逐行时间线 + 逐行链节,零 JS 零外链)。
fn audit_page(wal: &Path) -> String {
    match audit_html::build_report(wal) {
        Ok(mut report) => {
            report.generated_at_unix = Some(SystemClock.now());
            audit_html::render_html(&report)
        }
        Err(err) => error_page(
            "审计回放页不可用(fail-closed)",
            &format!("账本读取失败:{err}。坏账绝不产出回放页,证据以 WAL 原文为准。"),
        ),
    }
}

/// 撤销:三道门禁(表单完整 → 令牌 → 闸本体)之后才碰账本。
fn revoke(writer: &mut TcpStream, token: &str, wal: &Path, body: &[u8]) -> std::io::Result<()> {
    let form = match parse_form(body) {
        Ok(form) => form,
        Err(reason) => return respond_simple(writer, "400 Bad Request", reason),
    };
    let Some(delegation_id) = form_value(&form, "delegation") else {
        return respond_simple(writer, "400 Bad Request", "缺少 delegation 字段");
    };
    // 门禁第三道:令牌(进程内随机,只进页面表单;外部网页拿不到)。
    match form_value(&form, "token") {
        Some(candidate) if candidate == token => {}
        _ => {
            return respond_simple(
                writer,
                "403 Forbidden",
                "安全令牌不符:请从仪表盘页面发起撤销(可能的跨站伪造已拦下)",
            );
        }
    }

    // 账本必须还在:live_resuming 对不存在的路径会**新建**空账——那不是撤销,
    // 是悄悄换了一本账。先验存在,fail-closed。
    if !wal.exists() {
        return respond_simple(
            writer,
            "500 Internal Server Error",
            "审计账本已不存在:拒绝撤销,绝不悄悄新建一本空账",
        );
    }

    let mut state = match WanningState::live_resuming(wal) {
        Ok(state) => state,
        Err(CoreError::WalLocked { .. }) => {
            return respond_simple(
                writer,
                "409 Conflict",
                "另一写进程持有单写者锁(闸进程 wanning-mcp 还在跑?)。\
                 撤销被拒:撤销永不作用于一个活着的闸进程的内存态——它不知道这笔撤销, \
                 预算照扣。先停掉 MCP server,再撤销,重启后生效(重启回放账本)。",
            );
        }
        Err(err) => {
            return respond_simple(
                writer,
                "500 Internal Server Error",
                &format!("账本对账失败(fail-closed),拒绝撤销:{err}"),
            );
        }
    };
    match state.revoke(delegation_id) {
        Ok(()) => {
            let lines = state.wal_line_count().unwrap_or(0);
            let tail = state.audit_chain_tail().unwrap_or(0);
            drop(state);
            println!(
                "仪表盘撤销:委托 {delegation_id} 已落审计(账本 {lines} 行,链尾 0x{tail:016x})"
            );
            // POST→303→GET:刷新后的仪表盘直接显示撤销后的台账。
            respond_redirect(writer)
        }
        Err(CoreError::UnknownDelegation(id)) => {
            drop(state);
            respond_simple(
                writer,
                "404 Not Found",
                &format!("未知委托 '{id}':零状态变更、零审计噪音(嵌入方 bug 不留痕)"),
            )
        }
        Err(err) => {
            drop(state);
            respond_simple(
                writer,
                "500 Internal Server Error",
                &format!("撤销失败(账本未追加):{err}"),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// 表单解析(严格 urlencoded)
// ---------------------------------------------------------------------------

/// 解析 `application/x-www-form-urlencoded` 体(`k=v&k=v`),`+` = 空格,
/// `%XX` 十六进制解码;坏序列一律报错,绝不猜。
fn parse_form(body: &[u8]) -> Result<Vec<(String, String)>, &'static str> {
    let body = std::str::from_utf8(body).map_err(|_| "表单体不是 UTF-8")?;
    let mut form = Vec::new();
    for pair in body.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').ok_or("表单对缺少 '='")?;
        form.push((form_urldecode(key)?, form_urldecode(value)?));
    }
    Ok(form)
}

/// 取表单字段(首个同名者;表单字段只有两个,线性找即可)。
fn form_value<'a>(form: &'a [(String, String)], key: &str) -> Option<&'a str> {
    form.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

fn form_urldecode(input: &str) -> Result<String, &'static str> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' => {
                let hex = bytes.get(index + 1..index + 3).ok_or("百分号编码不完整")?;
                let hi = (hex[0] as char).to_digit(16).ok_or("百分号编码非法")?;
                let lo = (hex[1] as char).to_digit(16).ok_or("百分号编码非法")?;
                out.push((hi * 16 + lo) as u8);
                index += 3;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| "解码后不是 UTF-8")
}

// ---------------------------------------------------------------------------
// 渲染(全部经 escape_html;金额/时刻/链值复用 W-22 口径)
// ---------------------------------------------------------------------------

/// 撤销按钮:令牌 + 委托 id 两个隐藏字段,POST 到 `/revoke`。
fn revoke_form(token: &str, delegation_id: &str) -> String {
    format!(
        "<form method=\"post\" action=\"/revoke\" class=\"revoke\">\
         <input type=\"hidden\" name=\"token\" value=\"{}\">\
         <input type=\"hidden\" name=\"delegation\" value=\"{}\">\
         <button type=\"submit\">撤销(kill switch)</button></form>",
        audit_html::escape_html(token),
        audit_html::escape_html(delegation_id),
    )
}

/// 委托当前是否在生效窗口内(以页面生成时刻为「现在」;窗口外如实标注,
/// 不冒充「生效中」)。
fn validity_label(valid_from: u64, valid_until: u64, now: u64) -> &'static str {
    if now < valid_from {
        "未生效"
    } else if now >= valid_until {
        "已过期"
    } else {
        "生效窗口内"
    }
}

fn render_dashboard(wal: &Path, report: &audit_html::AuditReport, token: &str) -> String {
    let now = report.generated_at_unix.unwrap_or(0);
    let mut html = String::with_capacity(8 * 1024 + report.rows.len().min(TAIL_ROWS) * 256);
    html.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n");
    html.push_str("<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    html.push_str(&format!(
        "<meta http-equiv=\"refresh\" content=\"{REFRESH_SECS}\">\n"
    ));
    html.push_str("<title>Wanning 仪表盘</title>\n");
    html.push_str(DASHBOARD_STYLE);
    html.push_str("</head>\n<body>\n<main class=\"wrap\">\n");

    html.push_str("<header>\n<h1>Wanning 闸仪表盘</h1>\n");
    html.push_str(
        "<p class=\"sub\">本地只读 · 判定实时滚动(自动刷新)· 只监听 127.0.0.1 · 零 JS 零外链</p>\n</header>\n",
    );

    html.push_str("<section class=\"meta\" aria-label=\"对账\">\n<ul>\n");
    html.push_str(&format!(
        "<li>审计账本:<code>{}</code></li>\n",
        audit_html::escape_html(&crate::slash(wal))
    ));
    html.push_str(&format!(
        "<li>回放对账(两遍一致):0x{} · 完整性链尾:0x{}</li>\n",
        audit_html::chain_hex(report.replay_state_hash),
        audit_html::chain_hex(report.chain_tail),
    ));
    html.push_str(&format!(
        "<li>审计行 {} · 放行 {} 笔 · 拒绝 {} 笔 · 撤销 {} · 累计放行 {}</li>\n",
        report.rows.len(),
        report.counts.allow,
        report.counts.deny,
        report.counts.revoke,
        audit_html::format_cents(report.allow_amount_cents),
    ));
    html.push_str(
        "<li>完整证据页:<a href=\"/audit\">/audit 审计回放页</a>(逐行时间线 + 逐行链节)</li>\n",
    );
    html.push_str("</ul>\n</section>\n");

    // 预算台账 + 撤销表单(已撤销的只给状态,不再给按钮——kill switch 单向)。
    html.push_str("<section aria-label=\"预算台账\">\n<h2>预算台账</h2>\n");
    html.push_str("<table>\n<thead>\n<tr><th>委托</th><th>授权人</th><th>代理</th><th>上限</th><th>已花</th><th>剩余</th><th>有效期(UTC)</th><th>状态</th><th>操作</th></tr>\n</thead>\n<tbody>\n");
    for delegation in &report.delegations {
        html.push_str("<tr>");
        html.push_str(&format!(
            "<td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{} ~ {}</td><td>{}</td><td>",
            audit_html::escape_html(&delegation.id),
            audit_html::escape_html(&delegation.owner),
            audit_html::escape_html(&delegation.agent),
            audit_html::format_cents(delegation.cap_cents),
            audit_html::format_cents(delegation.spent_cents),
            audit_html::format_cents(delegation.remaining_cents),
            audit_html::format_utc(delegation.valid_from),
            audit_html::format_utc(delegation.valid_until),
            audit_html::escape_html(validity_label(
                delegation.valid_from,
                delegation.valid_until,
                now
            )),
        ));
        if delegation.revoked {
            html.push_str("<span class=\"badge bad\">■ 已撤销</span>");
        } else {
            html.push_str(&revoke_form(token, &delegation.id));
        }
        html.push_str("</td></tr>\n");
    }
    html.push_str("</tbody>\n</table>\n");
    html.push_str(
        "<p class=\"note\">撤销走闸本体:write-ahead 落审计、回放对账、单写者锁。\
         闸进程(wanning-mcp)持锁期间撤销会被拒——先停掉它,撤销后再启动,重启回放账本即刻生效。\
         已撤销的授权不会被复活,重复撤销只会再落一条审计。</p>\n",
    );
    html.push_str("</section>\n");

    // 实时滚动:最新在前,只取尾部;完整时间线在 /audit。
    html.push_str("<section aria-label=\"最近事件\">\n<h2>最近事件(实时滚动)</h2>\n");
    html.push_str("<table>\n<thead>\n<tr><th>行</th><th>时刻(UTC)</th><th>事件</th><th>委托</th><th>金额</th><th>商户 / 类目</th><th>原因 / 备注</th><th>账后累计</th><th>链值</th></tr>\n</thead>\n<tbody>\n");
    for row in report.rows.iter().rev().take(TAIL_ROWS) {
        html.push_str("<tr>");
        html.push_str(&format!(
            "<td>{}</td><td>{}</td>",
            row.line_no,
            audit_html::format_utc(row.record.ts())
        ));
        match &row.record {
            WalRecord::RegisterDelegation { delegation, .. } => {
                html.push_str(&format!(
                    "<td><span class=\"badge neutral\">◆ 注册</span></td><td><code>{}</code></td>\
                     <td>-</td><td>-</td><td>代理 {} · 上限 {}</td><td>-</td><td>0x{}</td>",
                    audit_html::escape_html(&delegation.id),
                    audit_html::escape_html(&delegation.agent),
                    audit_html::format_cents(delegation.budget_cap_cents),
                    audit_html::chain_hex(row.link.value),
                ));
            }
            WalRecord::Revoke { delegation_id, .. } => {
                html.push_str(&format!(
                    "<td><span class=\"badge neutral\">■ 撤销</span></td><td><code>{}</code></td><td>-</td><td>-</td><td>kill switch</td><td>-</td><td>0x{}</td>",
                    audit_html::escape_html(delegation_id),
                    audit_html::chain_hex(row.link.value),
                ));
            }
            WalRecord::Decide {
                decision,
                intent,
                reason,
                budget_after_cents,
                ..
            } => {
                let (badge, reason_label) = match (decision, reason) {
                    (WalDecision::Allow, _) => {
                        ("<span class=\"badge good\">● 放行</span>", "-".to_string())
                    }
                    (WalDecision::Deny, Some(reason)) => (
                        "<span class=\"badge bad\">✕ 拒绝</span>",
                        format!(
                            "{} ({})",
                            audit_html::escape_html(deny_reason_zh(reason)),
                            audit_html::escape_html(&reason_machine_label(reason)),
                        ),
                    ),
                    (WalDecision::Deny, None) => {
                        ("<span class=\"badge bad\">✕ 拒绝</span>", "-".to_string())
                    }
                };
                html.push_str(&format!(
                    "{badge}</td><td><code>{}</code></td><td>{}</td><td>{} / {}</td><td>{}</td><td>{}</td><td>0x{}</td>",
                    audit_html::escape_html(&intent.delegation_id),
                    audit_html::format_cents(intent.amount_cents),
                    audit_html::escape_html(&intent.merchant_id),
                    audit_html::escape_html(&intent.category),
                    reason_label,
                    audit_html::format_cents(*budget_after_cents),
                    audit_html::chain_hex(row.link.value),
                ));
            }
            // W-53a 人在环:③待支付(等人按指纹,确认前零资金流)。
            WalRecord::Pending {
                pending_id,
                intent,
                approved_amount_cents,
                expires_ts,
                ..
            } => {
                html.push_str(&format!(
                    "<td><span class=\"badge neutral\">◇ 待支付</span></td><td><code>{}</code></td>\
                     <td>{}</td><td>{} / {}</td><td>单 <code>{}</code> · 窗口至 {}(等人确认)</td>\
                     <td>-</td><td>0x{}</td>",
                    audit_html::escape_html(&intent.delegation_id),
                    audit_html::format_cents(*approved_amount_cents),
                    audit_html::escape_html(&intent.merchant_id),
                    audit_html::escape_html(&intent.category),
                    audit_html::escape_html(pending_id),
                    audit_html::format_utc(*expires_ts),
                    audit_html::chain_hex(row.link.value),
                ));
            }
            // ④人确认:人的显式动作,支付凭证入账(闸不验支付本身,如实呈现)。
            WalRecord::Confirm {
                pending_id,
                amount_cents,
                proof,
                ..
            } => {
                html.push_str(&format!(
                    "<td><span class=\"badge neutral\">✋ 人确认</span></td><td>单 <code>{}</code></td>\
                     <td>{}</td><td>-</td><td>支付凭证 {}(幂等,一次)</td><td>-</td><td>0x{}</td>",
                    audit_html::escape_html(pending_id),
                    audit_html::format_cents(*amount_cents),
                    audit_html::escape_html(proof),
                    audit_html::chain_hex(row.link.value),
                ));
            }
            // ⑤终态:完成 / TTL 过期作废。
            WalRecord::Terminal {
                pending_id,
                outcome,
                ..
            } => {
                let (badge, note) = match outcome {
                    PendingOutcome::Completed => (
                        "<span class=\"badge good\">● 完成</span>",
                        "人已确认,订单完成(⑤终态)",
                    ),
                    PendingOutcome::ExpiredVoid => (
                        "<span class=\"badge neutral\">⊘ 过期作废</span>",
                        "TTL 过期无人确认,作废(⑤终态)",
                    ),
                };
                html.push_str(&format!(
                    "{badge}</td><td>单 <code>{}</code></td><td>-</td><td>-</td><td>{}</td><td>-</td><td>0x{}</td>",
                    audit_html::escape_html(pending_id),
                    note,
                    audit_html::chain_hex(row.link.value),
                ));
            }
        }
        html.push_str("</tr>\n");
    }
    html.push_str("</tbody>\n</table>\n");
    html.push_str("</section>\n");

    html.push_str("<footer>\n<ul>\n");
    html.push_str("<li>证据以审计原文为准;本页是只读视图,链抓不住「只改最后一行」与「整体截尾」——那两半由所有者侧锚点负责(`wanning anchor-verify`)。</li>\n");
    html.push_str(&format!(
        "<li>页面每 {REFRESH_SECS} 秒自动刷新;零 JS、零外链、零网络调用。</li>\n"
    ));
    html.push_str("</ul>\n</footer>\n");
    html.push_str("</main>\n</body>\n</html>\n");
    html
}

/// reason 的机器可读标签(与 WAL 原文一致,便于对照;恒为蛇形英文,入页前照常
/// 过转义——自由文本的纪律不因「现在恰好安全」打折)。
fn reason_machine_label(reason: &wanning_core::gate::DenyReason) -> String {
    serde_json::to_string(reason)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

/// 账本不可信时的 fail-closed 视图:横幅 + 原始报错,零撤销表单、零台账。
fn render_dashboard_failed(wal: &Path, err: &CoreError) -> String {
    let mut html = String::with_capacity(4 * 1024);
    html.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n");
    html.push_str("<meta charset=\"utf-8\">\n");
    html.push_str(&format!(
        "<meta http-equiv=\"refresh\" content=\"{REFRESH_SECS}\">\n"
    ));
    html.push_str("<title>Wanning 仪表盘(账本不可信)</title>\n");
    html.push_str(DASHBOARD_STYLE);
    html.push_str("</head>\n<body>\n<main class=\"wrap\">\n");
    html.push_str("<header>\n<h1>Wanning 闸仪表盘</h1>\n");
    html.push_str("<p class=\"sub\">本地只读 · 只监听 127.0.0.1 · 零 JS 零外链</p>\n</header>\n");
    html.push_str(
        "<section class=\"failed\" role=\"alert\">\n<h2>账本读取失败(fail-closed)</h2>\n",
    );
    html.push_str(&format!(
        "<p>审计账本 <code>{}</code> 未通过完整性校验:完整性链 / 回放对账有一道不过。\
         闸拒绝展示台账、拒绝一切撤销动作——对不了账的账本,一个写动作都不能给。</p>\n",
        audit_html::escape_html(&crate::slash(wal))
    ));
    html.push_str(&format!(
        "<p class=\"detail\">{}</p>\n",
        audit_html::escape_html(&err.to_string())
    ));
    html.push_str("<p>排查:`wanning audit <账本>` 会给出同样的 fail-closed 报错;证据以 WAL 原文为准。页面每 2 秒自动刷新,账本恢复可信后本横幅自动消失。</p>\n");
    html.push_str("</section>\n</main>\n</body>\n</html>\n");
    html
}

/// 简单错误页(门禁/路由层用;正文不含任何外部输入以外的内容)。
fn error_page(title: &str, detail: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>{}</title>\n{}</head>\n<body>\n<main class=\"wrap\">\n\
         <section class=\"failed\" role=\"alert\">\n<h2>{}</h2>\n<p>{}</p>\n</section>\n\
         </main>\n</body>\n</html>\n",
        audit_html::escape_html(title),
        DASHBOARD_STYLE,
        audit_html::escape_html(title),
        audit_html::escape_html(detail),
    )
}

// ---------------------------------------------------------------------------
// 回写
// ---------------------------------------------------------------------------

fn respond(
    writer: &mut TcpStream,
    status_line: &str,
    extra_headers: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n{extra_headers}\r\n",
        body.len(),
    );
    writer.write_all(head.as_bytes())?;
    writer.write_all(body)?;
    writer.flush()
}

fn respond_html(writer: &mut TcpStream, status_line: &str, body: &str) -> std::io::Result<()> {
    respond(
        writer,
        status_line,
        "",
        "text/html; charset=utf-8",
        body.as_bytes(),
    )
}

fn respond_simple(writer: &mut TcpStream, status_line: &str, detail: &str) -> std::io::Result<()> {
    let page = error_page(status_line, detail);
    respond_html(writer, status_line, &page)
}

fn respond_redirect(writer: &mut TcpStream) -> std::io::Result<()> {
    respond(
        writer,
        "303 See Other",
        "Location: /\r\n",
        "text/html; charset=utf-8",
        b"",
    )
}

/// 仪表盘样式:单文件内联,零外链;深浅双主题跟随系统(`prefers-color-scheme`)。
const DASHBOARD_STYLE: &str = r#"<style>
:root {
  color-scheme: light dark;
  --page: #f9f9f7; --card: #fcfcfb; --ink: #0b0b0b; --ink-2: #52514e;
  --muted: #898781; --line: #e1e0d9; --bad: #b3261e; --good: #1b6e3c;
  --chip: #efeee8;
}
@media (prefers-color-scheme: dark) {
  :root {
    --page: #141414; --card: #1d1d1c; --ink: #e8e6e1; --ink-2: #a5a29b;
    --muted: #77756f; --line: #32312e; --bad: #f2b8b5; --good: #7fd6a0;
    --chip: #262523;
  }
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--page); color: var(--ink);
  font: 15px/1.6 system-ui, "Segoe UI", "Microsoft YaHei", sans-serif; }
.wrap { max-width: 1080px; margin: 0 auto; padding: 24px 20px 48px; }
h1 { font-size: 22px; margin: 0 0 4px; }
h2 { font-size: 15px; margin: 28px 0 10px; color: var(--ink-2); }
.sub { color: var(--muted); margin: 0 0 20px; }
.meta ul, footer ul { margin: 0; padding: 0 0 0 18px; color: var(--ink-2); }
.meta li, footer li { margin: 2px 0; }
code { font-family: ui-monospace, Consolas, monospace; font-size: 13px;
  background: var(--chip); padding: 1px 5px; border-radius: 4px; }
table { width: 100%; border-collapse: collapse; background: var(--card);
  border: 1px solid var(--line); border-radius: 8px; overflow: hidden; }
th, td { text-align: left; padding: 7px 10px; border-bottom: 1px solid var(--line);
  font-variant-numeric: tabular-nums; font-size: 14px; }
th { color: var(--ink-2); font-weight: 600; background: var(--chip); }
tbody tr:last-child td { border-bottom: none; }
.badge { white-space: nowrap; }
.badge.good { color: var(--good); }
.badge.bad { color: var(--bad); }
.badge.neutral { color: var(--ink-2); }
.revoke { display: inline; }
button { font: inherit; font-size: 13px; padding: 3px 12px; border-radius: 6px;
  border: 1px solid var(--bad); color: var(--bad); background: transparent; cursor: pointer; }
button:hover { background: var(--bad); color: var(--page); }
.note { color: var(--muted); font-size: 13px; }
.failed { border: 1px solid var(--bad); border-radius: 8px; padding: 16px 20px;
  background: var(--card); }
.failed h2 { margin-top: 0; color: var(--bad); }
.detail { font-family: ui-monospace, Consolas, monospace; font-size: 13px;
  color: var(--ink-2); word-break: break-all; }
a { color: var(--ink-2); }
</style>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_urldecode_roundtrip_and_strict_failures() {
        assert_eq!(form_urldecode("d+a").unwrap_or_default(), "d a");
        assert_eq!(
            form_urldecode("%E4%B8%AD%E6%96%87%2B1").unwrap_or_default(),
            "中文+1"
        );
        assert!(form_urldecode("%ZZ").is_err(), "非十六进制要拒");
        assert!(form_urldecode("%A").is_err(), "截断的编码要拒");
        assert!(form_urldecode("abc").is_ok(), "裸字符原样通过");
    }

    #[test]
    fn validity_label_covers_three_windows() {
        assert_eq!(validity_label(100, 200, 50), "未生效");
        assert_eq!(validity_label(100, 200, 150), "生效窗口内");
        assert_eq!(validity_label(100, 200, 200), "已过期");
    }

    #[test]
    fn parse_form_strictness() {
        let form = parse_form(b"delegation=d%20a&token=abcd").expect("正常表单");
        assert_eq!(form[0], ("delegation".to_string(), "d a".to_string()));
        assert_eq!(form[1], ("token".to_string(), "abcd".to_string()));
        assert!(parse_form(b"noequals").is_err(), "缺 = 要拒");
        assert!(parse_form(b"delegation=%ZZ&token=x").is_err(), "坏编码要拒");
    }

    #[test]
    fn csrf_token_is_hex_and_varies_per_process_call() {
        let a = new_csrf_token();
        let b = new_csrf_token();
        assert_eq!(a.len(), 32, "128 位 = 32 个十六进制字符");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "两次取样应不同(OS 熵种子)");
    }
}
