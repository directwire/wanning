//! W-43a 统一 CLI 入口 `wanning` 的产品契约(北极星:不用 Rust 的新用户,从进
//! 仓库到闸口在跑 ≤10 分钟,全程照 README 抄命令)。
//!
//! 锁四件事:①子命令分发面(init/audit/demo/anchor-verify,`ui` 随 W-43b 加入)
//! 与退出码纪律(帮助 0 / 用法错 2 / 运行失败 1);②init 从 PATH 解析 wanning-mcp
//! ——`cargo install` 装完就能 `wanning init`,失败时报错给安装指引;③audit 读
//! 默认账本(~/.wanning/wal.jsonl)并 fail-closed(坏账拒读);④端到端:干净目录
//! + 隔离家目录,照 README 抄 `wanning init`,把**生成配置里的 command/args 原样
//!   拉起**,两笔判定(预算内放行 / 超额拒绝)落默认账本。
//!
//!
//! 退出码约定:0 = 成功;2 = 用法错误(参数缺失/未知子命令);1 = 运行失败
//! (护栏拒/坏账/找不到账本)。真实消费护栏 W-07 在 `wanning demo` 直通路径上
//! 原样生效(--dry-run false 照拒)。

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use wanning_mcp::McpServer;

const WAN: &str = env!("CARGO_BIN_EXE_wanning");
/// cargo test --workspace 会先构建全部成员 bin;单跑本 crate 时需先
/// `cargo build -p wanning-mcp`(端到端测试依赖这个二进制存在)。
const TARGET_DEBUG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/debug");

fn temp_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "w43-cli-{}-{}-{}-{}",
        tag,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间早于 Unix 纪元")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

/// 跑 `wanning <args…>`:默认剥离 WANNING_HOME(默认路径测试显式给隔离值,
/// 绝不碰真实家目录),`envs` 追加环境变量。返回 (exit code, stdout, stderr)。
fn run(args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let mut command = Command::new(WAN);
    command
        .args(args)
        .stdin(Stdio::null())
        .env_remove("WANNING_HOME");
    for (key, value) in envs {
        command.env(key, value);
    }
    let out = command.output().expect("spawn wanning");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// PATH 上放一个空的临时目录(init 在其上找不到 wanning-mcp,必须 fail-closed)。
fn empty_path_dir() -> PathBuf {
    temp_dir("empty-path")
}

/// 假的 wanning-mcp 可执行文件(空文件;init 只验「存在且是文件」,不执行它)。
fn dummy_bin(dir: &Path) -> PathBuf {
    let path = dir.join(format!("wanning-mcp{}", std::env::consts::EXE_SUFFIX));
    fs::write(&path, b"").expect("写假 bin");
    path
}

/// 用进程内 `McpServer` 造一份真实审计账本(默认预算 1000 分;`decisions` 笔
/// 10 分放行)。判定笔数可调:audit 的篡改测试要 ≥2 笔才有「中间行」可改
/// ——W-21 已知边界:只改**尾行**完整性链本地验不住(无后继行引用),
/// 那一半由锚点层负责(见 anchor-verify 测试)。
fn build_wal(tag: &str, decisions: usize) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let wal = temp_dir("wal").join(format!(
        "{}-{}-{}.jsonl",
        tag,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    let mut server = McpServer::new_full(&wal, 1_000, 24, 10).expect("启动应成功");
    server
        .handle_line(
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","clientInfo":{"name":"w43-cli-tests","version":"0.0.0"}}}"#,
        )
        .expect("initialize 必有响应");
    server.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    for nonce in 1..=decisions as u64 {
        let response = server
            .handle_line(
                &json!({
                    "jsonrpc":"2.0","id":nonce,"method":"tools/call",
                    "params":{"name":"wanning_gate_evaluate","arguments":{
                        "delegation_id":"demo-d1","nonce":nonce,"amount_cents":10,
                        "merchant_id":"jd:shop-1","category":"grocery","memo":"W-43 audit 汇总"}}
                })
                .to_string(),
            )
            .expect("tools/call 必有响应");
        let value: Value = serde_json::from_str(&response).expect("响应是合法 JSON");
        assert_eq!(value["result"]["structuredContent"]["decision"], "allow");
    }
    drop(server);
    wal
}

// ── 分发面与退出码纪律 ─────────────────────────────────────────────────

#[test]
fn no_args_is_a_usage_error_listing_subcommands() {
    let (code, stdout, stderr) = run(&[], &[]);
    assert_eq!(code, 2, "无参数 = 用法错误:{stdout}{stderr}");
    let combined = format!("{stdout}{stderr}");
    for sub in ["init", "audit", "demo", "anchor-verify"] {
        assert!(combined.contains(sub), "用法要列 {sub}:{combined}");
    }
}

#[test]
fn unknown_subcommand_is_a_usage_error_listing_subcommands() {
    let (code, stdout, stderr) = run(&["definitely-not-a-subcommand"], &[]);
    assert_eq!(code, 2, "未知子命令 = 用法错误:{stdout}{stderr}");
    let combined = format!("{stdout}{stderr}");
    for sub in ["init", "audit", "demo", "anchor-verify"] {
        assert!(combined.contains(sub), "用法要列 {sub}:{combined}");
    }
}

#[test]
fn help_flag_exits_zero() {
    for args in [&["--help"][..], &["help"][..]] {
        let (code, stdout, _) = run(args, &[]);
        assert_eq!(code, 0, "--help 应成功:{stdout}");
        assert!(stdout.contains("wanning"), "{stdout}");
        assert!(stdout.contains("audit"), "{stdout}");
    }
}

#[test]
fn version_flag_exits_zero() {
    let (code, stdout, _) = run(&["--version"], &[]);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")), "{stdout}");
}

// ── init:PATH 解析 + fail-closed 指引 ─────────────────────────────────

#[test]
fn init_writes_generated_config_matching_the_library() {
    let bin_dir = temp_dir("init-bin");
    let bin = dummy_bin(&bin_dir);
    let wal = temp_dir("init-wal").join("audit.jsonl");
    let dir = temp_dir("init-cwd");
    let out = dir.join("mcp.json");
    let (code, stdout, stderr) = run(
        &[
            "init",
            "--platform",
            "kimi",
            "--bin",
            &bin.to_string_lossy(),
            "--wal",
            &wal.to_string_lossy(),
            "--out",
            &out.to_string_lossy(),
        ],
        &[],
    );
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");

    let written = fs::read_to_string(&out).expect("落盘文件");
    let expected = wanning_init::generate(
        wanning_init::Platform::Kimi,
        &wanning_init::GenerateOptions {
            mcp_bin: Some(bin),
            wal: Some(wal),
        },
    )
    .expect("生成应成功")
    .content;
    assert_eq!(written, expected, "落盘内容必须与库面生成逐字节一致");
    assert!(
        stdout.contains("重启"),
        "first-run 引导要打在 stdout:{stdout}"
    );
}

#[test]
fn init_fails_closed_with_install_guidance_when_binary_missing() {
    // PATH 上没有 wanning-mcp:绝不猜一个命令,报错点名缺什么并给安装指引。
    let empty = empty_path_dir();
    let path = std::env::join_paths([&empty]).expect("拼 PATH");
    let (code, stdout, stderr) = run(
        &["init", "--platform", "kimi"],
        &[("PATH", &path.to_string_lossy())],
    );
    assert_ne!(code, 0, "找不到 wanning-mcp 必须 fail-closed");
    let combined = format!("{stdout}{stderr}");
    assert!(combined.contains("wanning-mcp"), "{combined}");
    assert!(
        combined.contains("cargo install"),
        "报错要给安装指引:{combined}"
    );
}

#[test]
fn init_resolves_wanning_mcp_from_path() {
    // cargo install 装完 wanning-mcp 后,`wanning init` 直接从 PATH 找到并写实路径。
    let bin_dir = temp_dir("path-bin");
    let bin = dummy_bin(&bin_dir);
    let path = std::env::join_paths([&bin_dir]).expect("拼 PATH");
    let (code, stdout, stderr) = run(
        &["init", "--platform", "kimi"],
        &[("PATH", &path.to_string_lossy())],
    );
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains(&bin.to_string_lossy().replace('\\', "/")),
        "生成配置应写实解析出的 wanning-mcp 路径:{stdout}"
    );
}

// ── audit:读账本汇总 + fail-closed ────────────────────────────────────

#[test]
fn audit_prints_summary_for_a_built_wal() {
    let wal = build_wal("audit-summary", 1);
    let (code, stdout, stderr) = run(&["audit", &wal.to_string_lossy()], &[]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("行数: 2"), "注册 + 一笔判定:{stdout}");
    assert!(stdout.contains("判定: allow 1 / deny 0"), "{stdout}");
    assert!(stdout.contains("链尾: 0x"), "链尾要给十六进制:{stdout}");
    assert!(stdout.contains("demo-d1"), "委托要报 id:{stdout}");
    assert!(stdout.contains("剩 990 分"), "1000 - 10 = 990:{stdout}");
}

#[test]
fn audit_without_wal_arg_fails_closed_pointing_at_default_path() {
    // 不给路径:读默认账本 ~/.wanning/wal.jsonl;账本还不存在 → fail-closed,
    // 报错点名找的是哪个文件(测试用 WANNING_HOME 隔离,绝不碰真实家目录)。
    let home = temp_dir("audit-default-home");
    let home_s = home.to_string_lossy().into_owned();
    let (code, stdout, stderr) = run(&["audit"], &[("WANNING_HOME", home_s.as_str())]);
    assert_ne!(code, 0, "没有账本必须 fail-closed");
    let combined = format!("{stdout}{stderr}");
    let expected = home.join(".wanning").join("wal.jsonl");
    assert!(
        combined.contains(&expected.to_string_lossy().replace('\\', "/")),
        "报错要点名默认账本路径:{combined}"
    );
}

#[test]
fn audit_out_writes_html_report() {
    let wal = build_wal("audit-html", 1);
    let dir = temp_dir("audit-out");
    let html = dir.join("report.html");
    let (code, stdout, stderr) = run(
        &[
            "audit",
            &wal.to_string_lossy(),
            "--out",
            &html.to_string_lossy(),
        ],
        &[],
    );
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let written = fs::read_to_string(&html).expect("HTML 已落盘");
    assert!(
        written.contains("<html") || written.contains("Wanning"),
        "导出的是审计回放页"
    );
}

#[test]
fn audit_refuses_a_tampered_wal() {
    // 坏账拒读:完整性链断裂 → exit 1,绝不把对不上账的汇总当结论打出来。
    let wal = build_wal("audit-tampered", 2);
    let tampered = temp_dir("audit-tampered-copy").join("tampered.jsonl");
    fs::copy(&wal, &tampered).expect("复制账本");
    let content = fs::read_to_string(&tampered).expect("读回");
    let broken = content.replacen("grocery", "tampered", 1);
    assert_ne!(content, broken, "篡改要真的改到内容");
    fs::write(&tampered, broken).expect("写坏账副本");
    let (code, stdout, stderr) = run(&["audit", &tampered.to_string_lossy()], &[]);
    assert_ne!(code, 0, "坏账必须拒读:stdout={stdout}");
    assert!(!stderr.is_empty(), "要有报错:{stderr}");
}

// ── demo:直通 wanning-demo,真实消费护栏 W-07 原样生效 ────────────────

#[test]
fn demo_delegates_to_the_offline_scenario() {
    let (code, stdout, stderr) = run(&["demo", "--scenario", "smoke"], &[]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("离线场景 smoke"),
        "场景输出原样透传:{stdout}"
    );
}

#[test]
fn demo_real_spend_guard_still_fails_closed() {
    // --dry-run false = 真实消费路径:护栏 W-07 原样生效(本机无密钥 → 拒)。
    let (code, stdout, stderr) = run(
        &[
            "demo",
            "--scenario",
            "four-selling-points",
            "--dry-run",
            "false",
        ],
        &[],
    );
    assert_ne!(code, 0, "护栏必须拦下真实消费路径");
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("真实消费路径保持关闭"),
        "护栏要给清晰报错:{combined}"
    );
}

#[test]
fn demo_unknown_scenario_is_a_usage_failure() {
    let (code, _stdout, stderr) = run(&["demo", "--scenario", "nope"], &[]);
    assert_ne!(code, 0, "{stderr}");
    assert!(
        stderr.contains("可用场景") || stderr.contains("nope"),
        "{stderr}"
    );
}

// ── anchor-verify:第三方零密钥验签,直通同一语义 ──────────────────────

#[test]
fn anchor_verify_accepts_a_signed_anchor_and_rejects_a_tampered_wal() {
    let wal = build_wal("anchor-verify", 1);
    let dir = temp_dir("anchor");
    let seed_path = dir.join("seed.hex");
    fs::write(&seed_path, "ab".repeat(32)).expect("写测试种子(32 字节,测试专用)");

    let anchor_path = dir.join("anchor.json");
    let file = wanning_demo::anchor_v2::sign_v2(
        &wal,
        &wanning_demo::anchor_v2::Ed25519Seed::from_hex_file(&seed_path).expect("种子文件合法"),
        1_800_000_000,
        &anchor_path,
    )
    .expect("签出锚点(账本非空)");

    // 第三方零密钥验签:exit 0,回执带锚点链尾。
    let (code, stdout, stderr) = run(
        &[
            "anchor-verify",
            "--anchor",
            &anchor_path.to_string_lossy(),
            "--wal",
            &wal.to_string_lossy(),
        ],
        &[],
    );
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains(&file.chain_tail_hex),
        "回执要带与锚点一致的链尾:{stdout}"
    );

    // 篡改账本 → 验签 fail-closed。
    let tampered = dir.join("tampered.jsonl");
    let content = fs::read_to_string(&wal).expect("读账本");
    fs::write(&tampered, content.replacen("grocery", "tampered", 1)).expect("写坏账副本");
    let (code, _stdout, stderr) = run(
        &[
            "anchor-verify",
            "--anchor",
            &anchor_path.to_string_lossy(),
            "--wal",
            &tampered.to_string_lossy(),
        ],
        &[],
    );
    assert_ne!(code, 0, "篡改必须现形:{stderr}");
}

// ── 北极星端到端:干净目录 + 隔离家目录,init → 配置 → 闸判定 ──────────

#[test]
fn new_user_path_init_to_gate_decision() {
    // 照 README 抄命令:`wanning init --platform claude-code`(PATH 上已有
    // wanning-mcp)→ 把**生成配置里的 command/args 原样拉起**(= wanning-mcp,
    // 默认预算 1000 分)→ 一笔预算内放行、一笔超额拒绝,判定落默认账本
    // ~/.wanning/wal.jsonl。全程零占位符、零手改配置。
    let mcp_bin =
        PathBuf::from(TARGET_DEBUG).join(format!("wanning-mcp{}", std::env::consts::EXE_SUFFIX));
    assert!(
        mcp_bin.is_file(),
        "需要先构建 wanning-mcp(cargo test --workspace 会先构建全部 bin):{}",
        mcp_bin.display()
    );

    let home = temp_dir("e2e-home");
    let cwd = temp_dir("e2e-cwd");
    let out = cwd.join("mcp.json");
    let home_s = home.to_string_lossy().into_owned();
    let path = std::env::join_paths([PathBuf::from(TARGET_DEBUG)]).expect("拼 PATH");

    let (code, stdout, stderr) = run(
        &[
            "init",
            "--platform",
            "claude-code",
            "--out",
            &out.to_string_lossy(),
        ],
        &[
            ("WANNING_HOME", home_s.as_str()),
            ("PATH", &path.to_string_lossy()),
        ],
    );
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");

    let config: Value = serde_json::from_str(&fs::read_to_string(&out).expect("读生成配置"))
        .expect("配置是合法 JSON");
    let server = &config["mcpServers"]["wanning"];
    let command = server["command"]
        .as_str()
        .expect("command 字段")
        .to_string();
    let args: Vec<String> = server["args"]
        .as_array()
        .expect("args 数组")
        .iter()
        .map(|v| v.as_str().expect("字符串参数").to_string())
        .collect();
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--budget" && w[1] == "1000"),
        "默认预算策略随配置走:{args:?}"
    );

    // 配置里的 command/args 原样拉起(= 用户工具将要 spawn 的同一条命令)。
    let mut child = Command::new(&command)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("按生成配置拉起闸进程");
    // ChildStdout 只实现 Read 不实现 BufRead(read_line 不可用),按行读先包一层
    // BufReader;stdin/stdout take 出来持有,免得与收尾 kill/wait 的借用打架。
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    let decision = |stdin: &mut ChildStdin,
                    stdout: &mut BufReader<ChildStdout>,
                    id,
                    nonce: u64,
                    amount: u64| {
        let request = json!({
            "jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":"wanning_gate_evaluate","arguments":{
                "delegation_id":"demo-d1","nonce":nonce,"amount_cents":amount,
                "merchant_id":"jd:shop-1","category":"grocery","memo":"W-43 北极星 e2e"}}
        })
        .to_string();
        writeln!(stdin, "{request}").expect("写请求");
        stdin.flush().expect("flush");
        let mut line = String::new();
        stdout.read_line(&mut line).expect("读响应");
        serde_json::from_str::<Value>(line.trim()).expect("响应是合法 JSON")
    };
    // 握手(initialize + initialized 通知)。
    {
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":0,"method":"initialize","params":{{"protocolVersion":"2025-06-18","clientInfo":{{"name":"w43-e2e","version":"0.0.0"}}}}}}"#
        )
        .expect("写握手");
        stdin.flush().expect("flush");
        let mut line = String::new();
        stdout.read_line(&mut line).expect("读握手响应");
        let value: Value = serde_json::from_str(line.trim()).expect("握手响应是合法 JSON");
        assert_eq!(value["result"]["protocolVersion"], "2025-06-18");
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )
        .expect("写通知");
        stdin.flush().expect("flush");
    }

    let allow = decision(&mut stdin, &mut stdout, 1, 1, 400);
    assert_eq!(
        allow["result"]["structuredContent"]["decision"], "allow",
        "{allow}"
    );
    assert_eq!(
        allow["result"]["structuredContent"]["budget_after_cents"], 400,
        "budget_after = 累计已花(400 分):{allow}"
    );
    let deny = decision(&mut stdin, &mut stdout, 2, 2, 100_000);
    assert_eq!(
        deny["result"]["structuredContent"]["decision"], "deny",
        "{deny}"
    );
    assert_eq!(
        deny["result"]["structuredContent"]["reason"], "over_budget",
        "{deny}"
    );
    child.kill().expect("收尾杀进程");
    child.wait().expect("收尾回收");

    // 判定落默认账本(自动建目录)。
    let wal = home.join(".wanning").join("wal.jsonl");
    assert!(wal.is_file(), "判定应落默认账本 {}", wal.display());
    let lines = fs::read_to_string(&wal).expect("读账本").lines().count();
    assert!(lines >= 3, "注册 + 两笔判定至少 3 行,实际 {lines}");
}
