//! W-51b `wanning doctor` 的产品契约:挂载面六项检查(零模型、零外网、零真实消费)。
//!
//! 锁六件事(任务书 `Wanning-oss/W-51-seamless-journey.md` W-51b):
//! ① wanning-mcp 二进制存在 + 版本(PATH 解析,与 wanning init 同一解析);
//! ② 平台配置条目解析 + 语义校验(command 指向真实文件,args 含 --wal/--budget);
//! ③ 真握手:隔离临时账本拉起配置指向的 wanning-mcp,initialize → tools/list
//!    完整往返(零模型零外网零真实消费;配置里写的账本一个字节都不碰);
//! ④ 审计账本所在目录可写;
//! ⑤ 真实消费就绪度:复用 guard.rs 的 EnvSnapshot/GuardDenied 原文(只读 env,
//!    信息项,不影响体检结论);
//! ⑥ 版本一致性:配置指向的 bin 版本 ≠ 当前 wanning → 提示重跑
//!    `wanning init --platform <名> --install`。
//!
//! 用法面:`wanning doctor` 扫描八平台(未安装的跳过;一个都没装 = 退出码 1),
//! `wanning doctor --platform <名>` 只体检这一个(未安装 = ❌)。
//!
//! 测试隔离:全部 env(DSH/HERMES/OPENCLAW/CODEX/KIMI/WANNING_*)显式剥离再按需
//! 注入,绝不触碰真实家目录与真实配置;握手用隔离临时账本,配置账本文件保持不存在。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

const WAN: &str = env!("CARGO_BIN_EXE_wanning");
/// cargo test --workspace 会先构建全部成员 bin;单跑本 crate 时需先
/// `cargo build -p wanning-mcp`(真握手/版本一致性检查依赖真实二进制)。
const TARGET_DEBUG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/debug");

fn temp_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "w51-doctor-{}-{}-{}-{}",
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

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn real_mcp() -> PathBuf {
    let path = Path::new(TARGET_DEBUG).join(format!("wanning-mcp{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.is_file(),
        "cargo test --workspace 会构建全部 bin;单跑本 crate 先 cargo build -p wanning-mcp:{}",
        path.display()
    );
    path
}

/// 假宿主/假 bin:只应答 `--version`(打印给定版本行后按给定码退出),其余
/// 一律静默退出 0 —— 真握手检查会因此拿到「server 提前关闭输出」而 ❌。
#[cfg(windows)]
fn fake_version_bin(dir: &Path, version_line: &str, exit_code: i32) -> PathBuf {
    let script = format!(
        concat!(
            "@echo off\r\n",
            "if \"%~1\"==\"--version\" (\r\n",
            "  echo {version_line}\r\n",
            "  exit /b {exit_code}\r\n",
            ")\r\n",
            "exit /b 0\r\n",
        ),
        version_line = version_line,
        exit_code = exit_code
    );
    let path = dir.join("wanning-mcp.cmd");
    fs::write(&path, script).expect("写假 bin");
    path
}

#[cfg(unix)]
fn fake_version_bin(dir: &Path, version_line: &str, exit_code: i32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        concat!(
            "#!/bin/sh\n",
            "if [ \"$1\" = \"--version\" ]; then\n",
            "  echo '{version_line}'\n",
            "  exit {exit_code}\n",
            "fi\n",
            "exit 0\n",
        ),
        version_line = version_line,
        exit_code = exit_code
    );
    let path = dir.join("wanning-mcp");
    fs::write(&path, script).expect("写假 bin");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// 把目录列表作为完整 PATH(不叠加真实 PATH:体检结果必须与宿主机安装状态无关)。
fn path_env(dirs: &[&Path]) -> String {
    std::env::join_paths(dirs.iter().copied())
        .expect("join PATH")
        .to_string_lossy()
        .into_owned()
}

/// 跑 `wanning doctor …`:剥离一切会泄露真实配置/密钥的 env,再按需注入。
fn run(args: &[&str], cwd: &Path, envs: &[(&str, &str)]) -> (i32, String, String) {
    let mut command = Command::new(WAN);
    command.args(args).current_dir(cwd).stdin(Stdio::null());
    for key in [
        "WANNING_HOME",
        "DSH_HOME",
        "HERMES_HOME",
        "OPENCLAW_STATE_DIR",
        "CODEX_HOME",
        "KIMI_CODE_HOME",
        "WANNING_ALLOW_REAL_SPEND",
        "WANNING_GLM_KEY",
        "WANNING_JD_APP_KEY",
        "WANNING_JD_APP_SECRET",
        "WANNING_JD_ACCESS_TOKEN",
    ] {
        command.env_remove(key);
    }
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

/// 手写一份 .mcp.json(用户改过配置的形态:command 指到任意路径)。
fn write_mcp_json(project: &Path, command: &Path, wal: &Path) {
    let document = json!({
        "mcpServers": {
            "wanning": {
                "command": slash(command),
                "args": ["--wal", slash(wal), "--budget", "1000"]
            }
        }
    });
    fs::write(
        project.join(".mcp.json"),
        serde_json::to_string_pretty(&document).unwrap(),
    )
    .expect("写 .mcp.json");
}

const ALL_PLATFORMS: &[&str] = &[
    "claude-code",
    "codex",
    "kimi",
    "trae",
    "workbuddy",
    "deepseek-harness",
    "openclaw",
    "hermes",
];

// ── 用法面与退出码纪律 ─────────────────────────────────────────────────

#[test]
fn doctor_usage_and_exit_codes() {
    let project = temp_dir("usage");
    // --help 退出 0 并说明六项检查。
    let (code, stdout, _) = run(&["doctor", "--help"], &project, &[]);
    assert_eq!(code, 0, "{stdout}");
    for marker in ["①", "②", "③", "④", "⑤", "⑥"] {
        assert!(stdout.contains(marker), "--help 要说明六项检查:{stdout}");
    }
    // 未知平台 = 用法错(退出码 2),报错列全矩阵。
    let (code, _, stderr) = run(&["doctor", "--platform", "nonsense"], &project, &[]);
    assert_eq!(code, 2, "未知平台 = 用法错: {stderr}");
    for name in ALL_PLATFORMS {
        assert!(stderr.contains(name), "报错要列 {name}: {stderr}");
    }
    // --platform 缺取值 = 用法错。
    let (code, _, stderr) = run(&["doctor", "--platform"], &project, &[]);
    assert_eq!(code, 2, "{stderr}");
}

#[test]
fn doctor_listed_in_top_level_help() {
    let project = temp_dir("top-help");
    let (code, stdout, _) = run(&["--help"], &project, &[]);
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.contains("doctor"),
        "顶层 --help 要列 doctor:{stdout}"
    );
}

// ── 扫描模式:一个都没装 = fail 并给安装命令 ────────────────────────────

#[test]
fn scan_with_nothing_installed_fails_with_install_guidance() {
    let project = temp_dir("scan-none");
    let empty = temp_dir("empty-path");
    let (code, stdout, _) = run(
        &["doctor"],
        &project,
        &[("PATH", path_env(&[&empty]).as_str())],
    );
    assert_eq!(code, 1, "一个平台都没装 = 体检不过:{stdout}");
    assert!(
        stdout.contains("一个都没装") || stdout.contains("没装"),
        "{stdout}"
    );
    assert!(
        stdout.contains("wanning init --platform"),
        "修复命令要点名 init --install: {stdout}"
    );
    for name in ALL_PLATFORMS {
        assert!(stdout.contains(name), "扫描要覆盖 {name}: {stdout}");
    }
    // ① 也不过(PATH 是空目录):给 cargo install 指引。
    assert!(stdout.contains("cargo install"), "{stdout}");
}

#[test]
fn single_platform_uninstalled_is_a_fail_with_install_command() {
    let project = temp_dir("single-none");
    let empty = temp_dir("empty-path2");
    let (code, stdout, _) = run(
        &["doctor", "--platform", "trae"],
        &project,
        &[("PATH", path_env(&[&empty]).as_str())],
    );
    assert_eq!(code, 1, "显式点名未装平台 = ❌:{stdout}");
    assert!(stdout.contains("trae"), "{stdout}");
    assert!(
        stdout.contains("wanning init --platform trae --install"),
        "修复命令要带平台名: {stdout}"
    );
}

// ── ① 二进制检查 ─────────────────────────────────────────────────────

#[test]
fn check1_reports_binary_and_version_from_path() {
    let project = temp_dir("check1");
    let real = real_mcp();
    let (code, stdout, _) = run(
        &["doctor", "--platform", "claude-code"],
        &project,
        &[(
            "PATH",
            path_env(&[real.parent().expect("bin 目录")]).as_str(),
        )],
    );
    // claude-code 未装仍是 ❌,但 ① 的二进制检查必须 ✅ 并报出版本。
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("✅"), "① 应为绿:{stdout}");
    assert!(stdout.contains("wanning-mcp"), "{stdout}");
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "① 要报出真实版本: {stdout}"
    );
}

// ── 三命令流端到端(init --install → doctor 全绿)─────────────────────

#[test]
fn three_command_flow_end_to_end_green() {
    let project = temp_dir("e2e");
    let real = real_mcp();
    let wal = project.join("wal.jsonl");
    let path = path_env(&[real.parent().expect("bin 目录")]);
    let envs = [("PATH", path.as_str())];

    // 第 1 命令:wanning init --platform claude-code --install(W-51a)。
    let (code, _, stderr) = run(
        &[
            "init",
            "--platform",
            "claude-code",
            "--install",
            "--bin",
            &slash(&real),
            "--wal",
            &slash(&wal),
        ],
        &project,
        &envs,
    );
    assert_eq!(code, 0, "init --install 应成功: {stderr}");
    assert!(project.join(".mcp.json").is_file());
    assert!(!wal.exists(), "init 不该创建账本文件(账本由闸创建)");

    // 第 2 命令:wanning doctor --platform claude-code → 全绿(除 ⑤ 信息项)。
    let (code, stdout, stderr) = run(&["doctor", "--platform", "claude-code"], &project, &envs);
    assert_eq!(code, 0, "体检应全绿: {stdout}{stderr}");
    assert!(stdout.contains("✅"), "{stdout}");
    assert!(!stdout.contains("❌"), "全绿输出不该有 ❌: {stdout}");
    // ③ 真握手报出工具面。
    assert!(
        stdout.contains("wanning_gate_evaluate") && stdout.contains("wanning_audit_tail"),
        "握手要报出两个工具: {stdout}"
    );
    // ⑤ 是信息项:缺护栏 env 照列,但不影响退出码(上面已断言 0)。
    assert!(
        stdout.contains("WANNING_ALLOW_REAL_SPEND"),
        "⑤ 要列出缺什么(GuardDenied 原文): {stdout}"
    );
    // ⑥ 版本一致。
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "⑥ 要报版本一致性: {stdout}"
    );

    // 握手隔离:配置里写的账本文件直到体检结束都不存在(握手用的是临时账本)。
    assert!(
        !wal.exists(),
        "doctor 绝不能在配置账本上落字节(握手必须用隔离临时账本)"
    );
}

// ── ⑤ 护栏 env 已齐的形态 ────────────────────────────────────────────

#[test]
fn check5_guard_env_complete_shows_ready_line() {
    let project = temp_dir("guard-ok");
    let real = real_mcp();
    let wal = project.join("wal.jsonl");
    let path = path_env(&[real.parent().expect("bin 目录")]);
    let envs = [
        ("PATH", path.as_str()),
        ("WANNING_ALLOW_REAL_SPEND", "1"),
        ("WANNING_GLM_KEY", "glm-test"),
        ("WANNING_JD_APP_KEY", "jd-key"),
        ("WANNING_JD_APP_SECRET", "jd-secret"),
        ("WANNING_JD_ACCESS_TOKEN", "jd-token"),
    ];
    let (code, _, stderr) = run(
        &[
            "init",
            "--platform",
            "claude-code",
            "--install",
            "--bin",
            &slash(&real),
            "--wal",
            &slash(&wal),
        ],
        &project,
        &envs,
    );
    assert_eq!(code, 0, "{stderr}");
    let (code, stdout, _) = run(&["doctor", "--platform", "claude-code"], &project, &envs);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("已齐"), "⑤ 护栏齐了要有就绪行: {stdout}");
    assert!(
        stdout.contains("未接线") || stdout.contains("不等于能花钱"),
        "⑤ 护栏齐 ≠ 能花钱,要诚实标注: {stdout}"
    );
}

// ── ⑥ 版本陈旧 + ③ 坏二进制 ─────────────────────────────────────────

#[test]
fn stale_bin_flags_rerun_init_install() {
    let project = temp_dir("stale");
    let fake = fake_version_bin(&project, "wanning-mcp 0.0.9-ancient", 0);
    let wal = project.join("wal.jsonl");
    write_mcp_json(&project, &fake, &wal);
    let (code, stdout, _) = run(
        &["doctor", "--platform", "claude-code"],
        &project,
        &[("PATH", path_env(&[&project]).as_str())],
    );
    assert_eq!(code, 1, "旧二进制 = 体检不过:{stdout}");
    // ⑥ 点名版本不一致,修复命令带平台名。
    assert!(
        stdout.contains("0.0.9-ancient"),
        "⑥ 要报出配置 bin 自报的版本: {stdout}"
    );
    assert!(
        stdout.contains("wanning init --platform claude-code --install"),
        "⑥ 修复命令 = 重跑 init --install: {stdout}"
    );
    // ③ 假 bin 不会说 MCP 协议 → 真握手必须 ❌(fail-closed,绝不假装绿)。
    assert!(stdout.contains("❌"), "③ 真握手对假 bin 要红: {stdout}");
}

// ── 扫描模式:装了一个,其余跳过 ─────────────────────────────────────

#[test]
fn scan_mode_reports_all_platforms_and_skips_uninstalled() {
    let project = temp_dir("scan-partial");
    let real = real_mcp();
    let wal = project.join("wal.jsonl");
    let path = path_env(&[real.parent().expect("bin 目录")]);
    let envs = [("PATH", path.as_str())];
    let (code, _, stderr) = run(
        &[
            "init",
            "--platform",
            "claude-code",
            "--install",
            "--bin",
            &slash(&real),
            "--wal",
            &slash(&wal),
        ],
        &project,
        &envs,
    );
    assert_eq!(code, 0, "{stderr}");

    let (code, stdout, _) = run(&["doctor"], &project, &envs);
    assert_eq!(code, 0, "装了一个且全绿 = 体检通过:{stdout}");
    for name in ALL_PLATFORMS {
        assert!(stdout.contains(name), "扫描要覆盖 {name}: {stdout}");
    }
    assert!(stdout.contains("⏭"), "未安装的应标跳过: {stdout}");
    assert!(stdout.contains("✅"), "已装平台应全绿: {stdout}");
}
