//! W-43b `wanning ui` 集成测试:真实 HTTP 往返打真服务器(不是 mock handler)。
//!
//! 覆盖面 = 模块文档里立的三道跨站门禁、撤销走闸本体、读路径零持锁、
//! 坏账 fail-closed、表单严格解码、CLI 用法/退出码纪律。所有 WAL 都建在
//! 临时目录(`进程 id + 原子序号 + 纳秒`,W-21 撞名教训),真实家目录零触碰。

use std::io::{BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use wanning_cli::ui::{UiServer, UiStartError};
use wanning_cli::{run_cli, USAGE};
use wanning_core::clock::{Clock, SystemClock};
use wanning_core::delegation::Delegation;
use wanning_core::intent::SpendIntent;
use wanning_core::state::WanningState;

// ── 临时目录与样本账本 ─────────────────────────────────────────────────

fn temp_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "w43b-ui-{}-{}-{}-{}",
        tag,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间早于 Unix 纪元")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

/// 两份委托 + 三笔判定(放行/超额拒/放行)的样本账本。
///
/// d-a:上限 1000 分,nonce 1 放行 400 分,nonce 2 想花 9000 分 → over_budget 拒;
/// d-b:上限 500 分,nonce 1 放行 100 分。nonce 作用域各走各的,互不串号。
fn sample_wal(tag: &str) -> PathBuf {
    let wal = temp_dir(tag).join("wal.jsonl");
    let now = SystemClock.now();
    let mut state = WanningState::live_resuming(&wal).expect("空账可开");
    for (id, cap, scope) in [
        ("d-a", 1_000u64, "agent:ui-test:a"),
        ("d-b", 500, "agent:ui-test:b"),
    ] {
        state
            .register_delegation(Delegation::new(
                id,
                "所有者",
                "ui-agent",
                cap,
                now,
                now + 3_600,
                scope,
            ))
            .expect("注册样本委托");
    }
    let intents = [
        SpendIntent::new("d-a", 1, 400, "mock:shop", "grocery", "ui 测试放行"),
        SpendIntent::new("d-a", 2, 9_000, "mock:shop", "electronics", "ui 测试超额"),
        SpendIntent::new("d-b", 1, 100, "mock:shop", "grocery", "ui 测试第二份委托"),
    ];
    for intent in &intents {
        state.decide(intent).expect("判定落账");
    }
    wal
}

fn wal_lines(wal: &Path) -> Vec<String> {
    std::fs::read_to_string(wal)
        .expect("读账本")
        .lines()
        .map(str::to_string)
        .collect()
}

// ── 真实 HTTP 客户端(原始字节进出,Connection: close) ────────────────

struct HttpResponse {
    status: String,
    headers: String,
    body: String,
}

fn send(addr: SocketAddr, raw: &str) -> HttpResponse {
    let mut stream = TcpStream::connect(addr).expect("连不上仪表盘");
    stream.write_all(raw.as_bytes()).expect("写请求失败");
    let mut raw_response = String::new();
    BufReader::new(stream)
        .read_to_string(&mut raw_response)
        .expect("读响应失败(服务器应主动关闭)");
    let (head, body) = raw_response
        .split_once("\r\n\r\n")
        .unwrap_or((raw_response.as_str(), ""));
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or_default().to_string();
    HttpResponse {
        status,
        headers: lines.collect::<Vec<_>>().join("\r\n"),
        body: body.to_string(),
    }
}

fn get(addr: SocketAddr, path: &str) -> HttpResponse {
    send(
        addr,
        &format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
            addr.port()
        ),
    )
}

fn post_form(addr: SocketAddr, body: &str, origin: Option<&str>) -> HttpResponse {
    let mut request = format!(
        "POST /revoke HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\
         Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n",
        addr.port(),
        body.len()
    );
    if let Some(origin) = origin {
        request.push_str(&format!("Origin: {origin}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    request.push_str(body);
    send(addr, &request)
}

/// 从仪表盘页抽本进程令牌(每个撤销表单的隐藏字段里都有)。
fn extract_token(page: &str) -> String {
    const MARKER: &str = "name=\"token\" value=\"";
    let start = page.find(MARKER).expect("页面应含撤销表单令牌") + MARKER.len();
    let end = page[start..].find('"').expect("令牌值应有闭合引号");
    page[start..start + end].to_string()
}

fn start(wal: &Path) -> UiServer {
    UiServer::start(wal).expect("仪表盘应能启动")
}

// ── 仪表盘展示 ────────────────────────────────────────────────────────

#[test]
fn dashboard_shows_budgets_tail_and_revoke_forms_with_zero_js() {
    let wal = sample_wal("dash");
    let server = start(&wal);
    let page = get(server.addr(), "/");

    assert_eq!(
        page.status, "HTTP/1.1 200 OK",
        "仪表盘应可达:{}",
        page.status
    );
    // 预算台账:两份委托、金额分→元纯整数(¥4.00 已花 / ¥10.00 上限)。
    for fragment in ["d-a", "d-b", "¥4.00", "¥10.00", "¥5.00", "¥1.00"] {
        assert!(page.body.contains(fragment), "台账应有 {fragment}");
    }
    // 对账节:回放 hash 与链尾都是 16 位十六进制。
    assert!(page.body.contains("回放对账(两遍一致):0x"), "{}", page.body);
    assert!(page.body.contains("完整性链尾:0x"));
    assert!(page.body.contains("/audit 审计回放页"), "应链到证据页");
    // 实时滚动:放行与拒绝徽章、拒绝原因中英双标。
    for fragment in ["● 放行", "✕ 拒绝", "超出预算上限", "over_budget"] {
        assert!(page.body.contains(fragment), "滚动条应有 {fragment}");
    }
    // 撤销表单:两份未撤销委托各一个,带本进程令牌。
    assert_eq!(
        page.body.matches("action=\"/revoke\"").count(),
        2,
        "两份委托各一个撤销表单"
    );
    let token = extract_token(&page.body);
    assert_eq!(token.len(), 32, "令牌 = 128 位十六进制");

    // 零 JS 零外链:一个 <script> 都没有,一个 http:// 外链都不引。
    assert!(!page.body.contains("<script"), "页面零 JS");
    assert!(!page.body.contains("http://"), "页面零外链:{}", {
        let hit = page.body.find("http://").map(|i| &page.body[i..i + 60]);
        format!("{hit:?}")
    });
    // 零 JS 的自动刷新 = meta refresh。
    assert!(
        page.body
            .contains("<meta http-equiv=\"refresh\" content=\"2\">"),
        "自动刷新走 meta refresh"
    );
    // 响应头纪律:不缓存、不嗅探。
    assert!(page.headers.contains("Cache-Control: no-store"));
    assert!(page.headers.contains("X-Content-Type-Options: nosniff"));
}

#[test]
fn audit_page_and_routing_behaviour() {
    let wal = sample_wal("routes");
    let server = start(&wal);
    let audit = get(server.addr(), "/audit");
    assert_eq!(audit.status, "HTTP/1.1 200 OK");
    assert!(audit.body.contains("Wanning 审计回放"), "{}", audit.body);
    assert!(audit.body.contains("完整性链"), "回放页应含逐行链节");
    assert!(!audit.body.contains("<script"), "回放页同样零 JS");

    let missing = get(server.addr(), "/no-such-page");
    assert_eq!(missing.status, "HTTP/1.1 404 Not Found");
    let wrong_method = send(
        server.addr(),
        &format!(
            "DELETE / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
            server.addr().port()
        ),
    );
    assert_eq!(wrong_method.status, "HTTP/1.1 405 Method Not Allowed");
}

// ── 撤销走闸本体 ──────────────────────────────────────────────────────

#[test]
fn revoke_via_dashboard_goes_through_gate_and_lands_in_wal() {
    let wal = sample_wal("revoke");
    let lines_before = wal_lines(&wal).len();
    let server = start(&wal);
    let token = extract_token(&get(server.addr(), "/").body);

    let response = post_form(
        server.addr(),
        &format!("delegation=d-a&token={token}"),
        None,
    );
    assert_eq!(
        response.status, "HTTP/1.1 303 See Other",
        "{}",
        response.body
    );
    assert!(response.headers.contains("Location: /"), "PRG 回仪表盘");

    // 落审计:WAL 多恰一行 kind=revoke,引用被撤的委托 id。
    let lines = wal_lines(&wal);
    assert_eq!(lines.len(), lines_before + 1, "撤销应恰落一行审计");
    let last = lines.last().expect("有尾行");
    assert!(last.contains("\"kind\":\"revoke\""), "{last}");
    assert!(last.contains("d-a"), "{last}");

    // 刷新后的仪表盘:d-a 只剩已撤销徽章(无表单),d-b 照常有表单。
    let page = get(server.addr(), "/").body;
    assert!(page.contains("■ 已撤销"), "{}", page);
    assert_eq!(
        page.matches("action=\"/revoke\"").count(),
        1,
        "只剩 d-b 一个撤销表单"
    );
}

#[test]
fn revoke_with_missing_or_wrong_token_is_403_and_leaves_wal_untouched() {
    let wal = sample_wal("csrf-token");
    let before = wal_lines(&wal);
    let server = start(&wal);
    let token = extract_token(&get(server.addr(), "/").body);

    let no_token = post_form(server.addr(), "delegation=d-a", None);
    assert_eq!(
        no_token.status, "HTTP/1.1 403 Forbidden",
        "{}",
        no_token.body
    );
    let wrong_token = post_form(
        server.addr(),
        &format!("delegation=d-a&token={}-forged", &token[..16]),
        None,
    );
    assert_eq!(wrong_token.status, "HTTP/1.1 403 Forbidden");
    assert!(
        wrong_token.body.contains("跨站伪造"),
        "{}",
        wrong_token.body
    );

    assert_eq!(wal_lines(&wal), before, "被拒的撤销零审计噪音");
}

#[test]
fn cross_site_origin_and_foreign_host_are_refused() {
    let wal = sample_wal("cross-site");
    let before = wal_lines(&wal);
    let server = start(&wal);
    let token = extract_token(&get(server.addr(), "/").body);

    // 门禁二:浏览器跨站 POST 一定带 Origin,异源直接拒(令牌对了也不行)。
    let cross_origin = post_form(
        server.addr(),
        &format!("delegation=d-a&token={token}"),
        Some("http://evil.example"),
    );
    assert_eq!(cross_origin.status, "HTTP/1.1 403 Forbidden");
    assert!(cross_origin.body.contains("Origin 跨站"));

    // 门禁一:DNS rebinding 把攻击者域名指到 127.0.0.1 时 Host 是攻击者域名。
    let rebinding = send(
        server.addr(),
        &format!(
            "GET / HTTP/1.1\r\nHost: evil.example:{}\r\nConnection: close\r\n\r\n",
            server.addr().port()
        ),
    );
    assert_eq!(rebinding.status, "HTTP/1.1 403 Forbidden");
    assert!(rebinding.body.contains("Host 不是本机回环地址"));

    assert_eq!(wal_lines(&wal), before, "门禁拦截零审计噪音");
}

#[test]
fn unknown_delegation_is_404_with_zero_audit_noise() {
    let wal = sample_wal("unknown-delegation");
    let before = wal_lines(&wal);
    let server = start(&wal);
    let token = extract_token(&get(server.addr(), "/").body);

    let response = post_form(
        server.addr(),
        &format!("delegation=ghost&token={token}"),
        None,
    );
    assert_eq!(
        response.status, "HTTP/1.1 404 Not Found",
        "{}",
        response.body
    );
    assert!(response.body.contains("ghost"));
    assert_eq!(wal_lines(&wal), before, "嵌入方 bug 不留痕");
}

// ── 锁冲突:读不挡,写拒绝 ────────────────────────────────────────────

#[test]
fn reads_work_while_writer_lock_is_held_but_revoke_is_409() {
    let wal = sample_wal("lock");
    let before = wal_lines(&wal).len();
    // 进程内持有单写者锁,扮演另一个写进程(如 wanning-mcp)。
    let holder = WanningState::live_resuming(&wal).expect("持锁");
    let server = start(&wal);
    let token = extract_token(&get(server.addr(), "/").body);

    // 读路径零持锁:锁被别人拿着,仪表盘照常完整呈现。
    let page = get(server.addr(), "/");
    assert_eq!(page.status, "HTTP/1.1 200 OK");
    assert!(page.body.contains("action=\"/revoke\""), "照常显示表单");

    // 写路径被拒:409,绝不排队——撤销永不作用于活闸进程的内存态。
    let response = post_form(
        server.addr(),
        &format!("delegation=d-a&token={token}"),
        None,
    );
    assert_eq!(
        response.status, "HTTP/1.1 409 Conflict",
        "{}",
        response.body
    );
    assert!(response.body.contains("单写者锁"));
    assert_eq!(wal_lines(&wal).len(), before, "锁冲突期间账本零变化");

    // 锁释放后:同一令牌撤销照常走通。
    drop(holder);
    let response = post_form(
        server.addr(),
        &format!("delegation=d-a&token={token}"),
        None,
    );
    assert_eq!(
        response.status, "HTTP/1.1 303 See Other",
        "{}",
        response.body
    );
}

// ── 坏账 fail-closed ─────────────────────────────────────────────────

#[test]
fn tampered_ledger_dashboard_is_fail_closed_and_revoke_refused() {
    let wal = sample_wal("tamper");
    // 改中间行(行 3 的放行金额):完整性链当场断——尾行盲区归锚点管,不归这里。
    let lines = wal_lines(&wal);
    let mut tampered = lines.clone();
    tampered[2] = tampered[2].replace("\"amount_cents\":400", "\"amount_cents\":450");
    assert_ne!(tampered[2], lines[2], "篡改要真的改到字节");
    std::fs::write(&wal, tampered.join("\n") + "\n").expect("写坏账");

    let server = start(&wal);
    // 横幅页:200 可达,但台账/表单一概不给。
    let page = get(server.addr(), "/");
    assert_eq!(page.status, "HTTP/1.1 200 OK");
    assert!(page.body.contains("账本读取失败"), "{}", page.body);
    assert!(page.body.contains("role=\"alert\""));
    assert!(
        !page.body.contains("action=\"/revoke\""),
        "对不了账的账本一个写动作都不给"
    );
    assert!(
        !page.body.contains("<table"),
        "台账表格也不给(报错文本引用账本原文属诊断证据,不算展示台账)"
    );

    // 直接 POST(拿得到令牌也绕不过):live_resuming 对账失败 → 500 拒绝。
    let token = server.token().to_string();
    let response = post_form(
        server.addr(),
        &format!("delegation=d-a&token={token}"),
        None,
    );
    assert_eq!(
        response.status, "HTTP/1.1 500 Internal Server Error",
        "{}",
        response.body
    );
    assert!(response.body.contains("对账失败"));
    // 账本原样(被拒的撤销不追加任何行)。
    assert_eq!(wal_lines(&wal), tampered, "坏账期间零写入");
}

// ── 表单解码(严格 urlencoded) ───────────────────────────────────────

#[test]
fn urlencoded_delegation_id_roundtrips_and_malformed_encoding_is_rejected() {
    let wal = temp_dir("urlenc").join("wal.jsonl");
    let now = SystemClock.now();
    let mut state = WanningState::live_resuming(&wal).expect("空账可开");
    state
        .register_delegation(Delegation::new(
            "d 空格+中文",
            "所有者",
            "ui-agent",
            1_000,
            now,
            now + 3_600,
            "agent:ui-test:c",
        ))
        .expect("注册含空格与中文的委托");
    drop(state);

    let server = start(&wal);
    let token = extract_token(&get(server.addr(), "/").body);
    let lines_before = wal_lines(&wal).len();

    // `+`=空格、%2B=加号、中文按 UTF-8 百分号编码——解码后必须逐字符等于原 id。
    let response = post_form(
        server.addr(),
        &format!("delegation=d+%E7%A9%BA%E6%A0%BC%2B%E4%B8%AD%E6%96%87&token={token}"),
        None,
    );
    assert_eq!(
        response.status, "HTTP/1.1 303 See Other",
        "{}",
        response.body
    );
    let lines = wal_lines(&wal);
    assert_eq!(lines.len(), lines_before + 1);
    assert!(
        lines
            .last()
            .expect("有尾行")
            .contains("\"delegation_id\":\"d 空格+中文\""),
        "撤销行引用解码后的原 id:{:?}",
        lines.last()
    );

    // 坏编码(`%ZZ`)与缺 `=` 一律 400,绝不猜。
    let bad = post_form(
        server.addr(),
        &format!("delegation=%ZZ&token={token}"),
        None,
    );
    assert_eq!(bad.status, "HTTP/1.1 400 Bad Request", "{}", bad.body);
    let no_equals = post_form(server.addr(), "justatoken", None);
    assert_eq!(no_equals.status, "HTTP/1.1 400 Bad Request");
    assert_eq!(wal_lines(&wal).len(), lines_before + 1, "坏表单零落账");
}

// ── 绑定面与生命周期 ─────────────────────────────────────────────────

#[test]
fn ui_binds_loopback_only_with_random_port_by_default() {
    let wal = sample_wal("bind");
    let server = start(&wal);
    let addr = server.addr();
    assert!(addr.ip().is_loopback(), "只许回环:{addr}");
    assert_ne!(addr.port(), 0, "随机端口应解析成真实端口");
    assert_eq!(server.wal_path(), wal.as_path());
    drop(server); // Drop 应停服并收线程,不悬挂。
}

// ── CLI 面:参数解析与退出码 ─────────────────────────────────────────

#[test]
fn ui_arg_errors_carry_disciplined_exit_codes() {
    // 未知参数 = 用法错(2);账本不存在 = 运行失败(1);坏端口 = 用法错(2)。
    let missing = temp_dir("missing").join("nope.jsonl");
    let cases: Vec<(Vec<String>, u8, &str)> = vec![
        (vec!["ui".into(), "--bogus".into()], 2, "未知参数要报用法错"),
        (
            vec!["ui".into(), "--wal".into(), missing.display().to_string()],
            1,
            "账本不存在要 fail-closed 拒启",
        ),
        (
            vec!["ui".into(), "--port".into(), "99999".into()],
            2,
            "端口超 u16 要报用法错",
        ),
    ];
    for (args, expected, why) in cases {
        let code = run_cli(&args);
        assert_eq!(code, ExitCode::from(expected), "{why}: args={args:?}");
    }
    // 库面错误分层:同参数直接拿枚举。
    let error =
        wanning_cli::ui::start_from_args(&["--wal".to_string(), missing.display().to_string()])
            .err()
            .expect("不存在的账本要拒启");
    assert!(matches!(error, UiStartError::Failed(_)), "{error:?}");
    let usage = UiStartError::Usage("x".to_string());
    assert_eq!(usage, UiStartError::Usage("x".to_string()), "错误可比较");
}

#[test]
fn ui_help_prints_usage_via_real_binary() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_wanning"))
        .args(["ui", "--help"])
        .output()
        .expect("拉起 wanning bin");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("仪表盘"), "{stdout}");
    assert!(stdout.contains("127.0.0.1"), "{stdout}");
    // --help 文案与总 USAGE 同源(单一出处,不漂移)。
    assert!(USAGE.contains("wanning ui"));
}

#[test]
fn ui_refuses_missing_wal_via_real_binary_with_exit_1() {
    let missing = temp_dir("bin-missing").join("nope.jsonl");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_wanning"))
        .args(["ui", "--wal", &missing.display().to_string()])
        .output()
        .expect("拉起 wanning bin");
    assert_eq!(
        output.status.code(),
        Some(1),
        "账本不存在 = 运行失败(1):{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("审计账本不存在"), "{stderr}");
    assert!(!missing.exists(), "拒启绝不悄悄建账本");
}

#[test]
fn usage_error_exits_two_consistently() {
    let code = run_cli(&["ui".into(), "--port".into()]);
    assert_eq!(code, ExitCode::from(2), "旗标缺取值 = 用法错");
}
