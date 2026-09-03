//! W-36 生成器契约 + W-43a 产品化改版:每平台生成内容与仓内现物/调研的**字段面**
//! 对齐;W-43a 起配置去占位符——`wanning-mcp` 可执行文件与审计 WAL 路径在生成时
//! 解析成**真实绝对路径**直写进配置(新用户拿到就能用,不必手改 `{{WAL_PATH}}`);
//! 默认预算策略 `--budget` 显式写进 args(保守默认,可改)。
//! 写文件必须显式 --out 且绝不覆盖已存在文件;未知平台 fail-closed 列全矩阵。
//! (字段权威:claude-code/trae = 仓内现物;codex = W-35;kimi = W-40 本机实测;
//! workbuddy = W-37 直核官方 MCP-Guide;deepseek-harness = W-44 任务书 + dsh
//! 0.1.0-rc.7 包内 README。)零网络、零真实消费。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use wanning_init::{
    first_run_notes, generate, parse_platform, GenerateOptions, InitError, Platform,
};

const BIN: &str = env!("CARGO_BIN_EXE_wanning-init");
const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn temp_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "w43-init-{}-{}-{}-{}",
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

/// 假的 wanning-mcp 可执行文件(空文件;生成器只验「存在且是文件」,不执行它)。
fn dummy_bin(dir: &Path) -> PathBuf {
    let path = dir.join(format!("wanning-mcp{}", std::env::consts::EXE_SUFFIX));
    fs::write(&path, b"").expect("写假 bin");
    path
}

fn run_bin(args: &[&str], cwd: &Path) -> (i32, String, String) {
    run_bin_env(args, cwd, &[])
}

/// `envs` 追加进子进程环境(测试用 WANNING_HOME 隔离默认路径,绝不碰真实家目录)。
fn run_bin_env(args: &[&str], cwd: &Path, envs: &[(&str, &str)]) -> (i32, String, String) {
    let mut command = Command::new(BIN);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env_remove("WANNING_HOME");
    for (key, value) in envs {
        command.env(key, value);
    }
    let out = command.output().expect("spawn wanning-init");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// 显式 --bin + 显式 --wal 的标准生成选项(测试全走显式路径,不依赖本机 PATH)。
fn opts(bin: &Path, wal: &Path) -> GenerateOptions {
    GenerateOptions {
        mcp_bin: Some(bin.to_path_buf()),
        wal: Some(wal.to_path_buf()),
    }
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

// ── claude-code / trae:字段面对齐仓内现物(W-43a 起路径写实) ─────────────

#[test]
fn claude_code_output_matches_repo_field_face() {
    let repo: Value = serde_json::from_str(
        &fs::read_to_string(format!("{REPO_ROOT}/docs/examples/claude-code.mcp.json")).expect("读现物"),
    )
    .expect("现物是合法 JSON");
    let bin_dir = temp_dir("cc-bin");
    let bin = dummy_bin(&bin_dir);
    let wal = temp_dir("cc-wal").join("audit.jsonl");
    let artifact =
        generate(Platform::ClaudeCode, &opts(&bin, &wal)).expect("claude-code 生成应成功");
    let value: Value = serde_json::from_str(&artifact.content).expect("生成内容是合法 JSON");

    // 现物是字段面权威:顶层 mcpServers + type: stdio(W-19 实测同款)。
    let server = &value["mcpServers"]["wanning"];
    assert_eq!(
        server["type"], "stdio",
        "claude-code 现物带 type:stdio(W-19 实测),不得丢:{value}"
    );
    assert_eq!(
        repo["mcpServers"]["wanning"]["type"], "stdio",
        "现物前提变了要连本测试一起改"
    );

    // 产品化:命令 = 解析出的 wanning-mcp 绝对路径(正斜杠),不再是 cargo run。
    assert_eq!(
        server["command"],
        Value::from(slash(&bin)),
        "命令必须是解析出的 wanning-mcp 绝对路径: {value}"
    );
    let args = server["args"].as_array().expect("args 数组");
    assert!(args.contains(&Value::from("--wal")));
    assert!(
        args.iter().any(|a| a == &Value::from(slash(&wal))),
        "WAL 路径直写实路径(去占位符): {args:?}"
    );
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--budget" && w[1] == "1000"),
        "默认预算策略显式进 args(保守默认,用户可改): {args:?}"
    );

    // 去占位符:不再有 {{...}} 与平台路径变量(路径已解析成绝对路径)。
    assert!(
        !artifact.content.contains("${") && !artifact.content.contains("{{"),
        "W-43a 起配置去占位符,直写实路径: {artifact:?}"
    );
}

#[test]
fn trae_output_matches_repo_field_face() {
    let repo: Value = serde_json::from_str(
        &fs::read_to_string(format!("{REPO_ROOT}/docs/examples/trae.mcp.json")).expect("读现物"),
    )
    .expect("现物是合法 JSON");
    let bin_dir = temp_dir("trae-bin");
    let bin = dummy_bin(&bin_dir);
    let wal = temp_dir("trae-wal").join("audit.jsonl");
    let artifact = generate(Platform::Trae, &opts(&bin, &wal)).expect("trae 生成应成功");
    let value: Value = serde_json::from_str(&artifact.content).expect("生成内容是合法 JSON");

    // Trae 现物字段面无 type(官方文档直核,W-17);命令路径不得含空格。
    let server = &value["mcpServers"]["wanning"];
    assert!(
        server.get("type").is_none(),
        "trae 官方字段面无 type,不得臆加:{value}"
    );
    assert!(
        repo["mcpServers"]["wanning"].get("type").is_none(),
        "现物前提变了要连本测试一起改"
    );
    assert_eq!(server["command"], Value::from(slash(&bin)), "{value}");
    let args = server["args"].as_array().expect("args 数组");
    assert!(args.iter().any(|a| a == &Value::from(slash(&wal))));
    assert!(!artifact.content.contains("${") && !artifact.content.contains("{{"));
}

#[test]
fn json_platforms_keep_comments_out_of_file() {
    // 严格 JSON 没有注释语法:文件内容必须纯净,注释只能走 stdout 的 notes。
    let bin_dir = temp_dir("json-bin");
    let bin = dummy_bin(&bin_dir);
    let wal = temp_dir("json-wal").join("audit.jsonl");
    for (platform, tag) in [
        (Platform::ClaudeCode, "claude-code"),
        (Platform::Trae, "trae"),
        (Platform::WorkBuddy, "workbuddy"),
        (Platform::Kimi, "kimi"),
    ] {
        let artifact = generate(platform, &opts(&bin, &wal)).expect("生成应成功");
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

// ── codex:W-35 直核字段契约(config.toml 片段,路径写实) ─────────────────

#[test]
fn codex_snippet_matches_w35_research() {
    let bin_dir = temp_dir("codex-bin");
    let bin = dummy_bin(&bin_dir);
    let wal = temp_dir("codex-wal").join("audit.jsonl");
    let artifact = generate(Platform::Codex, &opts(&bin, &wal)).expect("生成应成功");
    let content = &artifact.content;
    let bin_s = slash(&bin);
    let wal_s = slash(&wal);

    assert!(content.contains("[mcp_servers.wanning]"), "{content}");
    assert!(
        content.contains(&format!("command = '{bin_s}'")),
        "codex 无路径变量 → 命令写实绝对路径: {content}"
    );
    assert!(
        content.contains(&format!(
            "args = [\"--wal\", '{wal_s}', \"--budget\", \"1000\"]"
        )),
        "args 写实 + 默认预算: {content}"
    );
    // W-35 直核:codex 配置没有路径变量 → 绝不出现 ${} 变量,也不再留占位符
    assert!(
        !content.contains("${") && !content.contains("{{"),
        "codex 无路径变量,不得生成变量或占位符: {content}"
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

// ── kimi:W-40 本机实测契约(kimi-code 0.39.1,mcp.json 形态,路径写实) ────
//
// 字段权威 = W-40 隔离 KIMI_CODE_HOME 实验(取证在档,W-40 节):
// 本机 kimi 0.39.1 实测无 `kimi mcp` 子命令;官方挂法 = `$KIMI_CODE_HOME/mcp.json`
// (用户级)或 `<repo>/.kimi-code/mcp.json`(项目级);mcpServers/command/args,
// 无 type 字段、无 ${...} 变量(W-17 直核的 `kimi mcp add` 属 legacy kimi-cli 挂法)。

#[test]
fn kimi_output_matches_w40_experiment() {
    let bin_dir = temp_dir("kimi-bin");
    let bin = dummy_bin(&bin_dir);
    let wal = temp_dir("kimi-wal").join("audit.jsonl");
    let artifact = generate(Platform::Kimi, &opts(&bin, &wal)).expect("生成应成功");
    let value: Value =
        serde_json::from_str(&artifact.content).expect("kimi 内容是合法 JSON(mcp.json 形态)");
    let server = &value["mcpServers"]["wanning"];
    assert_eq!(
        server["command"],
        Value::from(slash(&bin)),
        "kimi 无路径变量(W-40 官方文档直核)→ 命令写实绝对路径: {value}"
    );
    let args = server["args"].as_array().expect("args 数组");
    assert!(args.contains(&Value::from("--wal")), "{args:?}");
    assert!(
        args.iter().any(|a| a == &Value::from(slash(&wal))),
        "WAL 路径写实(官方未提及 ${{...}} 变量): {args:?}"
    );
    // W-40 实验现物与官方示例均无 type 字段(stdio 由 command 字段隐含),不得臆加
    assert!(
        server.get("type").is_none(),
        "kimi mcp.json 无 type 字段,不得臆加: {value}"
    );
    assert!(
        !artifact.content.contains("${") && !artifact.content.contains("{{"),
        "kimi 无路径变量证据,不得生成变量或占位符: {artifact:?}"
    );
    // notes 要给两种写入位置 + 项目级 workspace trust 提示(W-40 官方文档直核)
    let notes = artifact.notes.join("\n");
    assert!(notes.contains("KIMI_CODE_HOME"), "{notes}");
    assert!(notes.contains("trust"), "{notes}");
    assert!(notes.contains("W-40"), "{notes}");
    assert!(notes.contains("单写者锁"), "{notes}");

    // CLI 端到端:exit 0,stdout 带 mcpServers
    let dir = temp_dir("kimi-cli");
    let (code, stdout, _) = run_bin(
        &[
            "--platform",
            "kimi",
            "--bin",
            &bin.to_string_lossy(),
            "--wal",
            &wal.to_string_lossy(),
        ],
        &dir,
    );
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("mcpServers"), "{stdout}");
}

// ── workbuddy:W-37 调研破冰后加入矩阵(字段按官方 MCP-Guide 直核) ──────

#[test]
fn workbuddy_output_matches_w37_research() {
    // 字段权威 = workbuddy.cn 官方 MCP-Guide(mcpServers → command/args/env,
    // 无 type 字段;docs/research/workbuddy.md 第二节)。
    let bin_dir = temp_dir("wb-bin");
    let bin = dummy_bin(&bin_dir);
    let wal = temp_dir("wb-wal").join("audit.jsonl");
    let artifact = generate(Platform::WorkBuddy, &opts(&bin, &wal)).expect("生成应成功");
    let value: Value = serde_json::from_str(&artifact.content).expect("workbuddy 内容是合法 JSON");
    let server = &value["mcpServers"]["wanning"];
    assert_eq!(server["command"], Value::from(slash(&bin)), "{value}");
    let args = server["args"].as_array().expect("args 数组");
    assert!(args.contains(&Value::from("--wal")), "{args:?}");
    assert!(
        args.iter().any(|a| a == &Value::from(slash(&wal))),
        "WAL 路径写实(官方未提及变量): {args:?}"
    );
    // 官方示例字段面没有 type —— 与 claude-code 现物(带 type:stdio)是刻意差异
    assert!(
        server.get("type").is_none(),
        "workbuddy 官方示例无 type 字段,不得臆加: {value}"
    );
    assert!(
        !artifact.content.contains("${") && !artifact.content.contains("{{"),
        "workbuddy 无路径变量证据,不得生成变量或占位符: {artifact:?}"
    );
    assert!(artifact.notes.iter().any(|n| n.contains("待实测")));
    assert!(artifact.notes.iter().any(|n| n.contains("workbuddy")));

    // CLI 端到端:exit 0,stdout 带 mcpServers
    let dir = temp_dir("workbuddy-cli");
    let (code, stdout, _) = run_bin(
        &[
            "--platform",
            "workbuddy",
            "--bin",
            &bin.to_string_lossy(),
            "--wal",
            &wal.to_string_lossy(),
        ],
        &dir,
    );
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("mcpServers"), "{stdout}");
}

// ── deepseek-harness:W-44 任务书直核的 Cordis overlay patch 格式 ─────────
//
// 字段权威 = W-44 任务书(官方 docs/user/guide/mcp-memory.md 通用格式)+ 本机
// dsh 0.1.0-rc.7 包内 @deepseek-ai/dsh-mcp-client README(字段表:serverName
// `[A-Za-z0-9_-]{1,32}` / transport stdio|streamable-http / command / args / env /
// cwd;工具命名 mcp__<serverName>__<rawName>)。真实二进制取证(零网络零会话):
// `dsh --profile headless --dump-config --patch <生成文件>` exit 0,W-44 节。
// 会话级端到端待所有者(dsh 会话 = 模型会话 + 网络,红线 2)。

#[test]
fn deepseek_harness_output_matches_w44_research() {
    let bin_dir = temp_dir("dsh-bin");
    let bin = dummy_bin(&bin_dir);
    let wal = temp_dir("dsh-wal").join("audit.jsonl");
    let artifact = generate(Platform::DeepSeekHarness, &opts(&bin, &wal)).expect("生成应成功");
    let content = &artifact.content;
    let wal_s = slash(&wal);

    // patch entry 形态:顶层 YAML 数组,insert 列表(dsh 官方 patch 语义)
    assert!(
        content.lines().any(|l| l.trim_end() == "- insert:"),
        "必须是 insert 列表形态的 patch entry: {content}"
    );
    assert!(content.contains("id: wanning-gate"), "{content}");
    assert!(
        content.contains("name: '@deepseek-ai/dsh-mcp-client'"),
        "桥接插件包名按官方: {content}"
    );
    let config = content.split("config:").nth(1).expect("config: 块存在");
    assert!(config.contains("serverName: wanning"), "{content}");
    assert!(config.contains("transport: stdio"), "{content}");
    assert!(
        config.contains(&format!("command: {}", slash(&bin))),
        "dsh 不在 Wanning 仓内启动 → 命令写实绝对路径: {content}"
    );
    assert!(
        config.contains(&format!(
            "args: [\"--wal\", \"{wal_s}\", \"--budget\", \"1000\"]"
        )),
        "args 写实 + 默认预算: {content}"
    );
    assert!(config.contains("env: {}"), "{content}");
    assert!(
        config.contains("cwd: !!js process.cwd()"),
        "js-tag 按官方示例原样生成,不自创形态: {content}"
    );
    // dsh 文档未提及 ${...} 路径变量 → 绝不生成变量或占位符
    assert!(
        !content.contains("${") && !content.contains("{{"),
        "dsh 无路径变量证据,不得生成变量或占位符: {content}"
    );

    // notes:启用两路 + 合并追加纪律 + env 剥离 + 工具现身名 + 破坏性变更警示
    let notes = artifact.notes.join("\n");
    assert!(notes.contains("--patch"), "{notes}");
    assert!(notes.contains("cordis.patch.yml"), "{notes}");
    assert!(
        notes.contains("合并追加"),
        "持久化=合并追加,绝不整文件覆盖: {notes}"
    );
    assert!(
        notes.contains("DSH_*"),
        "env 剥离行为必须写进 notes(scrubbedParentEnv): {notes}"
    );
    assert!(
        notes.contains("mcp__wanning__wanning_gate_evaluate"),
        "工具现身名: {notes}"
    );
    assert!(
        notes.contains("破坏性变更"),
        "developer preview 破坏性变更警示: {notes}"
    );
    assert!(notes.contains("单写者锁"), "{notes}");
    assert!(notes.contains("0.1.0-rc"), "本机实测版本要在档: {notes}");

    // CLI 端到端:exit 0,stdout 带 patch entry
    let dir = temp_dir("dsh-cli");
    let (code, stdout, _) = run_bin(
        &[
            "--platform",
            "deepseek-harness",
            "--bin",
            &bin.to_string_lossy(),
            "--wal",
            &wal.to_string_lossy(),
        ],
        &dir,
    );
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("- insert:"), "{stdout}");
}

// ── W-45:openclaw / hermes(本机 2026.5.22 / hermes v0.19.1 直核 + 隔离实测) ──
//
// 字段权威 = 本机真宿主直核(W-45 隔离实测,取证在档(W-45 节)):
// - OpenClaw 2026.5.22 (a374c3a):`openclaw mcp set wanning '{"command":…,"args":[…]}'
//   落 $OPENCLAW_STATE_DIR/openclaw.json 的 mcp.servers.<name> = {command, args}
//   (隔离 env 实测落盘原文互证;官方 docs.openclaw.ai/mcp 直核 stdio 字段
//   command/args/env/cwd + env 安全过滤拦 NODE_OPTIONS 等);
// - hermes v0.19.1:`hermes mcp add wanning --command <bin> --args <args…>`
//   discovery-first 真连发现 2 工具,落 $HERMES_HOME/config.yaml 的
//   mcp_servers.<name> = {command, args, enabled: true};`hermes mcp test` 真连
//   141ms;`hermes -z -t wanning` + 本地 mock LLM → allow 400 落 WAL,二次会话
//   同 nonce → replay 拒(链连续)。

#[test]
fn openclaw_output_matches_w45_research() {
    let bin_dir = temp_dir("oc-bin");
    let bin = dummy_bin(&bin_dir);
    let wal = temp_dir("oc-wal").join("audit.jsonl");
    let artifact = generate(Platform::OpenClaw, &opts(&bin, &wal)).expect("生成应成功");
    let content = &artifact.content;

    // 产出 = 宿主 CLI 写入命令行(openclaw.json 由宿主自己管理,含 commands/
    // messages/agents 等骨架段——生成器绝不整文件覆盖,只出 set 这一条命令)。
    assert!(
        content.starts_with("openclaw mcp set wanning "),
        "产出必须是 openclaw mcp set 命令行: {content}"
    );
    assert!(
        content.contains(&format!("\"command\":\"{}\"", slash(&bin))),
        "command 字段写实绝对路径(正斜杠): {content}"
    );
    assert!(
        content.contains(&format!(
            "[\"--wal\",\"{}\",\"--budget\",\"1000\"]",
            slash(&wal)
        )),
        "args 数组逐字段与 W-45 隔离实测落盘一致: {content}"
    );
    assert!(
        !content.contains('\\'),
        "配置内容里不得出现反斜杠(转义雷):{content}"
    );
    let notes = artifact.notes.join("\n");
    assert!(
        notes.contains("mcp.servers"),
        "notes 要点名配置落点 mcp.servers(openclaw.json): {notes}"
    );
    assert!(
        notes.contains("gateway"),
        "notes 要诚实标注工具现身级验证待 gateway+模型会话(本轮只验配置面): {notes}"
    );
}

#[test]
fn hermes_output_matches_w45_research() {
    let bin_dir = temp_dir("hm-bin");
    let bin = dummy_bin(&bin_dir);
    let wal = temp_dir("hm-wal").join("audit.jsonl");
    let artifact = generate(Platform::Hermes, &opts(&bin, &wal)).expect("生成应成功");
    let content = &artifact.content;

    // 产出 = 宿主 CLI 挂载命令(discovery-first:add 即真连发现工具,挂载即验证)。
    assert!(
        content.contains("hermes mcp add wanning --command"),
        "产出必须是 hermes mcp add 命令行: {content}"
    );
    assert!(
        content.contains(&format!("--command {}", slash(&bin))),
        "command 写实绝对路径(正斜杠): {content}"
    );
    assert!(
        content.contains(&format!("--args --wal {} --budget 1000", slash(&wal))),
        "--args 尾接 wanning-mcp 参数(实测形态,--args 必须是最后一个选项): {content}"
    );
    let notes = artifact.notes.join("\n");
    assert!(
        notes.contains("mcp_servers"),
        "notes 要给 config.yaml 落点(mcp_servers 段): {notes}"
    );
    assert!(
        notes.contains("mcp__wanning__"),
        "notes 要给工具现身名(W-45 实测 deferred catalog): {notes}"
    );
    assert!(
        notes.contains("tool_call"),
        "notes 要说明 hermes 的 tool_call 间接调用层(直接调 mcp__ 名会报 does not exist): {notes}"
    );

    // CLI 端到端:两个平台的 exit 0 + stdout 带宿主 CLI 命令(真 spawn W-45 矩阵)
    for (name, marker) in [
        ("openclaw", "openclaw mcp set wanning"),
        ("hermes", "hermes mcp add wanning"),
    ] {
        let dir = temp_dir("w45-cli");
        let (code, stdout, _) = run_bin(
            &[
                "--platform",
                name,
                "--bin",
                &bin.to_string_lossy(),
                "--wal",
                &wal.to_string_lossy(),
            ],
            &dir,
        );
        assert_eq!(code, 0, "{name}: {stdout}");
        assert!(stdout.contains(marker), "{name}: {stdout}");
    }
}

// ── 未知平台:fail-closed 列全矩阵 ─────────────────────────────────────

#[test]
fn unknown_platform_fails_closed_listing_matrix() {
    let err = parse_platform("nope").expect_err("未知平台必须拒绝");
    let message = err.message();
    for name in [
        "claude-code",
        "codex",
        "kimi",
        "trae",
        "workbuddy",
        "deepseek-harness",
        "openclaw",
        "hermes",
    ] {
        assert!(message.contains(name), "矩阵缺 {name}: {message}");
    }

    let dir = temp_dir("unknown");
    let (code, stdout, stderr) = run_bin(&["--platform", "nope"], &dir);
    let combined = format!("{stdout}{stderr}");
    assert_ne!(code, 0);
    assert!(combined.contains("workbuddy"), "{combined}");
}

// ── 路径解析:--bin / --wal / 默认 ~/.wanning ───────────────────────────

#[test]
fn explicit_bin_must_be_an_existing_file() {
    let dir = temp_dir("bin-check");
    let missing = dir.join("nope.exe");
    match generate(
        Platform::Kimi,
        &GenerateOptions {
            mcp_bin: Some(missing),
            wal: Some(dir.join("a.jsonl")),
        },
    ) {
        Err(InitError::McpBinaryInvalid(message)) => {
            assert!(message.contains("wanning-mcp"), "{message}");
        }
        other => panic!("显式 --bin 指向不存在的文件必须 fail-closed:{other:?}"),
    }
}

#[test]
fn resolve_bin_searches_path_and_reports_searched_dirs() {
    use std::ffi::OsString;
    use wanning_init::resolve_bin;

    let dir = temp_dir("path-search");
    let bin = dummy_bin(&dir);

    // PATH 上有 → 找到
    let fake_path = std::env::join_paths([dir.clone()]).expect("拼 PATH");
    let found = resolve_bin(None, Some(fake_path.as_os_str())).expect("PATH 上应有 wanning-mcp");
    assert_eq!(found, bin, "PATH 搜索应命中 {bin:?}");

    // PATH 为空 → fail-closed,报错列出搜过的目录并给安装指引
    let empty = OsString::new();
    match resolve_bin(None, Some(empty.as_os_str())) {
        Err(InitError::McpBinaryNotFound { searched }) => {
            assert!(searched.is_empty(), "空 PATH 应没有可搜目录:{searched:?}");
            let message = InitError::McpBinaryNotFound { searched }.message();
            assert!(
                message.contains("cargo install"),
                "报错要给安装指引:{message}"
            );
        }
        other => panic!("空 PATH 必须 fail-closed:{other:?}"),
    }

    // PATH 上有目录但没有 wanning-mcp → 同样 fail-closed 且列出该目录
    let unrelated = temp_dir("path-empty-dir");
    let fake_path = std::env::join_paths([unrelated.clone()]).expect("拼 PATH");
    match resolve_bin(None, Some(fake_path.as_os_str())) {
        Err(InitError::McpBinaryNotFound { searched }) => {
            assert_eq!(searched, vec![unrelated], "报错要列出搜过的目录");
        }
        other => panic!("PATH 上没有 wanning-mcp 必须 fail-closed:{other:?}"),
    }
}

#[test]
fn generated_wal_path_is_absolute_and_forward_slashed() {
    // 显式相对 --wal → 相对当前目录转绝对;反斜杠 → 统一正斜杠(配置文件里
    // Windows 反斜杠在 JSON/YAML/TOML 里都要转义,正斜杠 Windows 也认)。
    let bin_dir = temp_dir("slash-bin");
    let bin = dummy_bin(&bin_dir);
    let cwd = temp_dir("slash-cwd");
    let wal = cwd.join("nested").join("audit.jsonl");
    let artifact = generate(
        Platform::Kimi,
        &GenerateOptions {
            mcp_bin: Some(bin),
            wal: Some(wal.clone()),
        },
    )
    .expect("生成应成功");
    assert!(
        artifact.content.contains(&slash(&wal)),
        "WAL 路径必须是绝对路径且正斜杠:{} in {}",
        slash(&wal),
        artifact.content
    );
    assert!(
        !artifact.content.contains('\\'),
        "配置内容里不得出现反斜杠(转义雷):{artifact:?}"
    );
}

#[test]
fn default_wal_lands_under_wanning_home_without_placeholder() {
    // 不给 --wal:默认 = $WANNING_HOME/.wanning/wal.jsonl(测试用隔离家目录,
    // 绝不碰真实家目录;默认路径在子进程里解析,测试进程环境零改动)。
    // 生成内容直写实路径,零占位符。
    let bin_dir = temp_dir("default-bin");
    let bin = dummy_bin(&bin_dir);
    let home = temp_dir("default-home");
    let expected = home.join(".wanning").join("wal.jsonl");
    let home_s = home.to_string_lossy().into_owned();
    let dir = temp_dir("default-cwd");
    let (code, stdout, stderr) = run_bin_env(
        &["--platform", "kimi", "--bin", &bin.to_string_lossy()],
        &dir,
        &[("WANNING_HOME", home_s.as_str())],
    );
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains(&slash(&expected)),
        "默认 WAL 应落在 $WANNING_HOME/.wanning/wal.jsonl:{} in {}",
        slash(&expected),
        stdout
    );
    assert!(!stdout.contains("{{"), "零占位符:{stdout}");
}

// ── Trae fail-closed:命令路径含空格 = 官方明示不能含空格 ────────────────

#[test]
fn trae_fails_closed_when_command_path_contains_whitespace() {
    // Trae 官方文档:command「不能含空格」(W-17 直核)。路径含空格 → 拒绝生成,
    // 绝不产出一份装不上的配置。
    let dir = temp_dir("trae space dir");
    let bin = dummy_bin(&dir);
    let wal = temp_dir("trae-space-wal").join("audit.jsonl");
    let err = generate(Platform::Trae, &opts(&bin, &wal)).expect_err("含空格路径必须拒绝");
    let message = err.message();
    assert!(message.contains("空格"), "{message}");
    assert!(
        message.contains("trae") || message.contains("Trae"),
        "{message}"
    );
}

// ── first-run 三行引导 ─────────────────────────────────────────────────

#[test]
fn first_run_notes_are_three_lines() {
    let notes = first_run_notes();
    assert_eq!(notes.len(), 3, "任务书:first-run 三行引导。实际:{notes:?}");
    assert!(notes.iter().all(|n| !n.trim().is_empty()));
    let joined = notes.join("\n");
    assert!(joined.contains("重启"), "第一步要提醒重启工具:{joined}");
    assert!(
        joined.contains("mcp__wanning__wanning_gate_evaluate"),
        "要告诉用户工具长什么样:{joined}"
    );
    assert!(
        joined.contains("over_budget"),
        "第三步要教用户怎么验证闸在工作(试超额应被拒):{joined}"
    );
}

// ── 写文件纪律:默认只打印;--out 显式且绝不覆盖 ────────────────────────

#[test]
fn stdout_default_creates_no_file() {
    let dir = temp_dir("stdout-only");
    let bin_dir = temp_dir("stdout-bin");
    let bin = dummy_bin(&bin_dir);
    let wal = temp_dir("stdout-wal").join("audit.jsonl");
    let (code, stdout, _) = run_bin(
        &[
            "--platform",
            "codex",
            "--bin",
            &bin.to_string_lossy(),
            "--wal",
            &wal.to_string_lossy(),
        ],
        &dir,
    );
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
    let bin_dir = temp_dir("no-overwrite-bin");
    let bin = dummy_bin(&bin_dir);
    let target = dir.join("mcp.json");
    const SENTINEL: &str = "SENTINEL-EXISTING-CONFIG-DO-NOT-TOUCH";
    fs::write(&target, SENTINEL).expect("预置已存在文件");

    // --out 缺路径参数:用法错误,非零退出
    let (code, _, _) = run_bin(
        &[
            "--platform",
            "claude-code",
            "--bin",
            &bin.to_string_lossy(),
            "--out",
        ],
        &dir,
    );
    assert_ne!(code, 0, "--out 缺值必须报用法错误");

    // 已存在文件:拒绝覆盖,原样保留
    let (code, _, stderr) = run_bin(
        &[
            "--platform",
            "claude-code",
            "--bin",
            &bin.to_string_lossy(),
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
    let bin_dir = temp_dir("out-writes-bin");
    let bin = dummy_bin(&bin_dir);
    let wal = temp_dir("out-writes-wal").join("audit.jsonl");
    let target = dir.join("wanning-codex.toml");
    let (code, stdout, stderr) = run_bin(
        &[
            "--platform",
            "codex",
            "--bin",
            &bin.to_string_lossy(),
            "--wal",
            &wal.to_string_lossy(),
            "--out",
            target.to_str().unwrap(),
        ],
        &dir,
    );
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");

    let written = fs::read_to_string(&target).expect("读回");
    let expected = generate(Platform::Codex, &opts(&bin, &wal))
        .expect("生成应成功")
        .content;
    assert_eq!(written, expected, "落盘内容必须与生成内容逐字节一致");
    assert!(stdout.contains("Wanning"), "说明行要打在 stdout:{stdout}");
    assert!(
        stdout.contains("重启"),
        "first-run 引导要打在 stdout:{stdout}"
    );
}

#[test]
fn missing_flag_value_is_a_usage_error() {
    let dir = temp_dir("flag-value");
    for flag in ["--bin", "--wal", "--out"] {
        let (code, _, stderr) = run_bin(&["--platform", "kimi", flag], &dir);
        assert_ne!(code, 0, "{flag} 缺值必须非零退出");
        assert!(stderr.contains(flag), "{flag} 报错要点名:{stderr}");
    }
}
