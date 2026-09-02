//! W-36 生成器契约:每平台生成内容与仓内现物/W-17、W-35、W-37 调研逐字段对齐;
//! 写文件必须显式 --out 且绝不覆盖已存在文件;未知平台 fail-closed 列全矩阵。
//! (W-37 破冰:workbuddy 从「拒绝生成待调研」转为支持矩阵,字段按官方
//! MCP-Guide 直核。)零网络、零真实消费。

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use wanning_init::{generate, parse_platform, Platform};

const BIN: &str = env!("CARGO_BIN_EXE_wanning-init");
const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn temp_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "w36-{}-{}-{}-{}",
        tag,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn run_bin(args: &[&str], cwd: &std::path::Path) -> (i32, String, String) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .expect("spawn wanning-init");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ── claude-code / trae:与仓内现物逐字段契约 ─────────────────────────────

#[test]
fn claude_code_output_matches_repo_artifact() {
    let repo: Value = serde_json::from_str(
        &fs::read_to_string(format!("{REPO_ROOT}/.mcp.json")).expect("读现物"),
    )
    .expect("现物是合法 JSON");
    let gen: Value =
        serde_json::from_str(&generate(Platform::ClaudeCode).content).expect("生成内容是合法 JSON");
    assert_eq!(
        gen, repo,
        "claude-code 生成内容必须与仓内 .mcp.json 语义全等"
    );

    let server = &repo["mcpServers"]["wanning"];
    assert_eq!(server["command"], "cargo");
    let args = server["args"].as_array().expect("args 数组");
    assert!(args.contains(&Value::from("--wal")));
    assert!(args
        .iter()
        .any(|a| a == "${CLAUDE_PROJECT_DIR:-.}/target/mcp-demo.wal"));
}

#[test]
fn trae_output_matches_repo_artifact() {
    let repo: Value = serde_json::from_str(
        &fs::read_to_string(format!("{REPO_ROOT}/.trae/mcp.json")).expect("读现物"),
    )
    .expect("现物是合法 JSON");
    let gen: Value =
        serde_json::from_str(&generate(Platform::Trae).content).expect("生成内容是合法 JSON");
    assert_eq!(gen, repo, "trae 生成内容必须与仓内 .trae/mcp.json 语义全等");

    let args = repo["mcpServers"]["wanning"]["args"]
        .as_array()
        .expect("args");
    assert!(args
        .iter()
        .any(|a| a == "${workspaceFolder}/target/mcp-demo.wal"));
}

#[test]
fn json_platforms_keep_comments_out_of_file() {
    // 严格 JSON 没有注释语法:文件内容必须纯净,注释只能走 stdout 的 notes。
    for (platform, tag) in [
        (Platform::ClaudeCode, "claude-code"),
        (Platform::Trae, "trae"),
        (Platform::WorkBuddy, "workbuddy"),
        (Platform::Kimi, "kimi"),
    ] {
        let artifact = generate(platform);
        assert!(
            serde_json::from_str::<Value>(&artifact.content).is_ok(),
            "{tag} 内容必须是可解析 JSON"
        );
        assert!(
            !artifact.content.lines().any(|l| {
                let t = l.trim_start();
                t.starts_with("//") || t.starts_with('#')
            }),
            "{tag} 文件内容不得含注释行(严格 JSON 无注释语法)"
        );
        assert!(
            !artifact.notes.is_empty(),
            "{tag} 的说明行必须在 stdout notes 里给出"
        );
    }
}

// ── codex:W-35 直核字段契约(config.toml 片段) ─────────────────────────

#[test]
fn codex_snippet_matches_w35_research() {
    let artifact = generate(Platform::Codex);
    let content = &artifact.content;

    assert!(content.contains("[mcp_servers.wanning]"), "{content}");
    assert!(content.contains("command = '{{WANNING_BIN}}'"), "{content}");
    assert!(
        content.contains(r#"args = ["--wal", '{{WAL_PATH}}']"#),
        "{content}"
    );
    // W-35 直核:codex 配置没有路径变量 → 绝对路径占位,绝不出现 ${} 变量
    assert!(
        !content.contains("${"),
        "codex 无路径变量,不得生成 ${{...}}: {content}"
    );
    // 可选加固字段(W-35 config-reference):required = true 与闸 fail-closed 同构
    assert!(content.contains("required = true"), "{content}");
    // 注释语法 = TOML 的 # 行,且必须带单写者锁语义提示
    assert!(content.lines().next().unwrap_or("").starts_with("# "));
    assert!(
        content.contains("单写者锁"),
        "注释要讲同挂一份 WAL 的并发语义"
    );
}

// ── kimi:W-40 本机实测契约(kimi-code 0.39.1,mcp.json 形态) ────────────
//
// 字段权威 = W-40 隔离 KIMI_CODE_HOME 实验(docs/tasks/P0-demo-closedloop.md W-40 节):
// 本机 kimi 0.39.1 实测无 `kimi mcp` 子命令;官方挂法 = `$KIMI_CODE_HOME/mcp.json`
// (用户级)或 `<repo>/.kimi-code/mcp.json`(项目级);W-40 实验现物
// (mcpServers/command/args,无 type 字段)被真 kimi 二进制接受并完成 MCP 往返
// (allow/replay/over_budget 三判定落 WAL)。W-17 直核的 `kimi mcp add` 属 legacy
// kimi-cli 挂法(老板机器 ~/.kimi → ~/.kimi-code 迁移痕迹佐证),不再生成。

#[test]
fn kimi_output_matches_w40_experiment() {
    let artifact = generate(Platform::Kimi);
    let value: Value =
        serde_json::from_str(&artifact.content).expect("kimi 内容是合法 JSON(mcp.json 形态)");
    let server = &value["mcpServers"]["wanning"];
    assert_eq!(
        server["command"], "{{WANNING_BIN}}",
        "kimi 无路径变量(W-40 官方文档直核)→ 命令用绝对路径占位符: {value}"
    );
    let args = server["args"].as_array().expect("args 数组");
    assert!(args.contains(&Value::from("--wal")), "{args:?}");
    assert!(
        args.iter().any(|a| a == "{{WAL_PATH}}"),
        "WAL 路径占位符(官方未提及 ${{...}} 变量 → 绝对路径手改): {args:?}"
    );
    // W-40 实验现物与官方示例均无 type 字段(stdio 由 command 字段隐含),不得臆加
    assert!(
        server.get("type").is_none(),
        "kimi mcp.json 无 type 字段,不得臆加: {value}"
    );
    assert!(
        !artifact.content.contains("${"),
        "kimi 无路径变量证据,不得生成 ${{...}}: {artifact:?}"
    );
    // notes 要给两种写入位置 + 项目级 workspace trust 提示(W-40 官方文档直核)
    let notes = artifact.notes.join("\n");
    assert!(notes.contains("KIMI_CODE_HOME"), "{notes}");
    assert!(notes.contains("trust"), "{notes}");
    assert!(notes.contains("W-40"), "{notes}");
    assert!(notes.contains("单写者锁"), "{notes}");

    // CLI 端到端:exit 0,stdout 带 mcpServers
    let dir = temp_dir("kimi-cli");
    let (code, stdout, _) = run_bin(&["--platform", "kimi"], &dir);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("mcpServers"), "{stdout}");
}

// ── workbuddy:W-37 调研破冰后加入矩阵(字段按官方 MCP-Guide 直核) ──────

#[test]
fn workbuddy_output_matches_w37_research() {
    // 字段权威 = workbuddy.cn 官方 MCP-Guide(mcpServers → command/args/env,
    // 无 type 字段;docs/research/workbuddy.md 第二节)。
    let artifact = generate(Platform::WorkBuddy);
    let value: Value = serde_json::from_str(&artifact.content).expect("workbuddy 内容是合法 JSON");
    let server = &value["mcpServers"]["wanning"];
    assert_eq!(server["command"], "cargo", "{value}");
    let args = server["args"].as_array().expect("args 数组");
    assert!(args.contains(&Value::from("--wal")), "{args:?}");
    assert!(
        args.iter().any(|a| a == "{{WAL_PATH}}"),
        "WAL 路径占位符(官方未提及变量 → 绝对路径手改): {args:?}"
    );
    // 官方示例字段面没有 type —— 与 claude-code 现物(带 type:stdio)是刻意差异
    assert!(
        server.get("type").is_none(),
        "workbuddy 官方示例无 type 字段,不得臆加: {value}"
    );
    // 官方文档未提及 ${...} 变量扩展 → 不得生成
    assert!(
        !artifact.content.contains("${"),
        "workbuddy 无路径变量证据,不得生成 ${{...}}: {artifact:?}"
    );
    assert!(artifact.notes.iter().any(|n| n.contains("待实测")));
    assert!(artifact.notes.iter().any(|n| n.contains("workbuddy")));

    // CLI 端到端:exit 0,stdout 带 mcpServers
    let dir = temp_dir("workbuddy-cli");
    let (code, stdout, _) = run_bin(&["--platform", "workbuddy"], &dir);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("mcpServers"), "{stdout}");
}

// ── 未知平台:fail-closed 列全矩阵 ─────────────────────────────────────

#[test]
fn unknown_platform_fails_closed_listing_matrix() {
    let err = parse_platform("nope").expect_err("未知平台必须拒绝");
    let message = err.message();
    for name in ["claude-code", "codex", "kimi", "trae", "workbuddy"] {
        assert!(message.contains(name), "矩阵缺 {name}: {message}");
    }

    let dir = temp_dir("unknown");
    let (code, stdout, stderr) = run_bin(&["--platform", "nope"], &dir);
    let combined = format!("{stdout}{stderr}");
    assert_ne!(code, 0);
    assert!(combined.contains("workbuddy"), "{combined}");
}

// ── 写文件纪律:默认只打印;--out 显式且绝不覆盖 ────────────────────────

#[test]
fn stdout_default_creates_no_file() {
    let dir = temp_dir("stdout-only");
    let (code, stdout, _) = run_bin(&["--platform", "codex"], &dir);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("[mcp_servers.wanning]"), "{stdout}");
    let created: Vec<_> = fs::read_dir(&dir).expect("列目录").collect();
    assert!(
        created.is_empty(),
        "未给 --out 时不得写任何文件:{created:?}"
    );
}

#[test]
fn out_refuses_to_overwrite_existing_file() {
    let dir = temp_dir("no-overwrite");
    let target = dir.join("mcp.json");
    const SENTINEL: &str = "SENTINEL-EXISTING-CONFIG-DO-NOT-TOUCH";
    fs::write(&target, SENTINEL).expect("预置已存在文件");

    // --out 缺路径参数:用法错误,非零退出
    let (code, _, _) = run_bin(&["--platform", "claude-code", "--out"], &dir);
    assert_ne!(code, 0, "--out 缺值必须报用法错误");

    // 已存在文件:拒绝覆盖,原样保留
    let (code, _, stderr) = run_bin(
        &[
            "--platform",
            "claude-code",
            "--out",
            target.to_str().unwrap(),
        ],
        &dir,
    );
    assert_ne!(code, 0, "已存在文件必须拒绝覆盖");
    assert!(
        stderr.contains("已存在") || stderr.contains("覆盖"),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(&target).expect("读回"),
        SENTINEL,
        "已存在文件必须原样保留,零写入"
    );
}

#[test]
fn out_writes_exact_content_and_prints_notes() {
    let dir = temp_dir("out-writes");
    let target = dir.join("wanning-codex.toml");
    let (code, stdout, stderr) = run_bin(
        &["--platform", "codex", "--out", target.to_str().unwrap()],
        &dir,
    );
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");

    let written = fs::read_to_string(&target).expect("读回");
    assert_eq!(
        written,
        generate(Platform::Codex).content,
        "落盘内容必须与生成内容逐字节一致"
    );
    assert!(stdout.contains("Wanning"), "说明行要打在 stdout:{stdout}");
}
