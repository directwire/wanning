//! W-51a `wanning init --install` 直写安装的契约测试。
//!
//! 纪律逐条对应任务书 W-51a(`Wanning-oss/W-51-seamless-journey.md`):
//! - claude-code/kimi/trae/workbuddy 四 mcp.json = merge 进 `mcpServers.wanning`,
//!   不动他人条目(merge 前后对他人条目做语义全等断言);
//! - deepseek-harness = `$DSH_HOME/cordis.patch.yml` 合并追加(W-44 纪律:append
//!   勿整文件覆盖;文本块级 merge,其他块逐字节保留);
//! - openclaw/hermes = 产出宿主 CLI 命令行,仅 `--yes` 显式时执行(假宿主脚本抓
//!   argv/stdin 作证;真宿主隔离实测见 P0 文档);
//! - 写前备份 `<file>.wanning.bak`;升级场景打 diff;`--dry-run` 零落盘;
//! - 无 `--install` 的 stdout 行为不变(W-36 契约);
//! - codex = fail-closed 不支持(TOML 主配置做文本合并的风险大于收益,给人工指引)。
//!
//! 零网络、零真实消费、零模型会话;所有临时路径进 `std::env::temp_dir()`,
//! 绝不触碰真实家目录(DSH_HOME/HERMES_HOME 等一律显式传入,不读进程环境)。

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use wanning_init::install::{
    install, read_installed_entry, InstallEnv, InstallError, InstallOptions, InstallState,
};
use wanning_init::{generate, GenerateOptions, Platform, Resolved};

const BIN: &str = env!("CARGO_BIN_EXE_wanning-init");

fn temp_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "w51-install-{}-{}-{}-{}",
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

/// 假的 wanning-mcp 可执行文件(空文件;install 只验「存在且是文件」,不执行它)。
fn dummy_bin(dir: &Path) -> PathBuf {
    let path = dir.join(format!("wanning-mcp{}", std::env::consts::EXE_SUFFIX));
    fs::write(&path, b"").expect("写假 bin");
    path
}

fn resolved(bin: &Path, wal: &Path) -> Resolved {
    Resolved {
        mcp_bin: bin.to_path_buf(),
        wal: wal.to_path_buf(),
    }
}

fn env_for<'a>(cwd: &'a Path, home: &'a Path) -> InstallEnv<'a> {
    InstallEnv {
        cwd,
        home: Some(home),
        dsh_home: None,
        openclaw_state_dir: None,
        hermes_home: None,
        kimi_code_home: None,
        codex_home: None,
        path_env: None,
    }
}

fn options<'a>(
    platform: Platform,
    resolved: &'a Resolved,
    env: &'a InstallEnv,
) -> InstallOptions<'a> {
    InstallOptions {
        platform,
        resolved,
        env,
        dry_run: false,
        yes: false,
        host_bin: None,
    }
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn expect_err(report: Result<wanning_init::install::InstallReport, InstallError>) -> String {
    match report {
        Ok(report) => panic!(
            "应该失败,实际成功: state={:?} actions={:?}",
            report.state, report.actions
        ),
        Err(error) => error.message(),
    }
}

fn expect_entry_err(
    entry: Result<Option<wanning_init::install::InstalledEntry>, InstallError>,
) -> String {
    match entry {
        Ok(Some(entry)) => panic!("应该失败,实际读到条目: {entry:?}"),
        Ok(None) => panic!("应该失败,实际返回 None"),
        Err(error) => error.message(),
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("读 {} 失败: {e}", path.display()))
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("建父目录");
    }
    fs::write(path, content).expect("写临时配置");
}

/// 与 generate() 的产物比对:安装写入的 wanning 条目必须和生成器 stdout 给出的
/// 完全同源(单一事实来源,不允许 install 侧另写一份字段面)。
fn generated_entry(platform: Platform, bin: &Path, wal: &Path) -> Value {
    let artifact = generate(
        platform,
        &GenerateOptions {
            mcp_bin: Some(bin.to_path_buf()),
            wal: Some(wal.to_path_buf()),
        },
    )
    .expect("生成配置");
    let document: Value = serde_json::from_str(&artifact.content).expect("生成内容是 JSON");
    document["mcpServers"]["wanning"].clone()
}

// ── A. 四 mcp.json 平台 ──────────────────────────────────────────────────

#[test]
fn fresh_creates_project_mcp_json() {
    let dir = temp_dir("fresh");
    let bin = dummy_bin(&dir);
    let wal = dir.join("wal.jsonl");
    let env = env_for(&dir, &dir);
    let res = resolved(&bin, &wal);
    let report = install(&options(Platform::ClaudeCode, &res, &env)).expect("安装成功");
    assert_eq!(report.state, InstallState::Fresh);
    assert_eq!(
        report.target.as_deref(),
        Some(dir.join(".mcp.json").as_path())
    );
    assert!(report.backup.is_none(), "全新创建没有旧文件可备份");
    assert!(!report.actions.is_empty());

    let document: Value =
        serde_json::from_str(&read(&dir.join(".mcp.json"))).expect("落盘内容是 JSON");
    assert_eq!(
        document["mcpServers"]["wanning"],
        generated_entry(Platform::ClaudeCode, &bin, &wal)
    );
}

#[test]
fn merge_preserves_other_servers_exactly() {
    let dir = temp_dir("merge");
    let bin = dummy_bin(&dir);
    let wal = dir.join("wal.jsonl");
    let others = json!({
        "mcpServers": {
            "other-tool": {
                "command": "node",
                "args": ["server.js", "--port", "3000"],
                "env": {"K": "V"}
            }
        }
    });
    write(
        &dir.join(".mcp.json"),
        &serde_json::to_string_pretty(&others).unwrap(),
    );
    let env = env_for(&dir, &dir);
    let res = resolved(&bin, &wal);
    let report = install(&options(Platform::ClaudeCode, &res, &env)).expect("安装成功");
    assert_eq!(report.state, InstallState::Updated);

    let document: Value = serde_json::from_str(&read(&dir.join(".mcp.json"))).expect("JSON");
    assert_eq!(
        document["mcpServers"]["other-tool"],
        others["mcpServers"]["other-tool"]
    );
    assert_eq!(
        document["mcpServers"]["wanning"],
        generated_entry(Platform::ClaudeCode, &bin, &wal)
    );
}

#[test]
fn upgrade_updates_entry_diff_and_backup() {
    let dir = temp_dir("upgrade");
    let bin = dummy_bin(&dir);
    let wal = dir.join("wal.jsonl");
    let old = json!({
        "mcpServers": {
            "wanning": {
                "type": "stdio",
                "command": "D:/old/wanning-mcp.exe",
                "args": ["--wal", "D:/old/wal.jsonl", "--budget", "500"]
            }
        }
    });
    let path = dir.join(".mcp.json");
    write(&path, &serde_json::to_string_pretty(&old).unwrap());
    let env = env_for(&dir, &dir);
    let res = resolved(&bin, &wal);
    let report = install(&options(Platform::ClaudeCode, &res, &env)).expect("安装成功");
    assert_eq!(report.state, InstallState::Updated);
    assert!(
        report.diff.iter().any(|line| line.starts_with('-'))
            && report.diff.iter().any(|line| line.starts_with('+')),
        "升级场景必须打 diff: {:?}",
        report.diff
    );
    assert!(
        report.diff.iter().any(|line| line.contains("command")),
        "diff 要点名改动的字段: {:?}",
        report.diff
    );

    let backup = report.backup.expect("升级前有备份");
    assert_eq!(backup, dir.join(".mcp.json.wanning.bak"));
    // 备份 = 写前原文件的逐字节内容(此后 path 已被改写成新条目,不能拿它比)。
    assert_eq!(
        read(&backup),
        serde_json::to_string_pretty(&old).unwrap(),
        "备份 = 写前原文件内容"
    );
    // 备份发生在写入前 → 备份里还是旧条目。
    let backup_json: Value = serde_json::from_str(&read(&backup)).expect("备份是 JSON");
    assert_eq!(backup_json, old);

    let document: Value = serde_json::from_str(&read(&path)).expect("JSON");
    assert_eq!(
        document["mcpServers"]["wanning"]["command"],
        json!(slash(&bin)),
        "条目升级为解析出的真实路径"
    );
}

#[test]
fn already_current_is_byte_noop() {
    let dir = temp_dir("current");
    let bin = dummy_bin(&dir);
    let wal = dir.join("wal.jsonl");
    let env = env_for(&dir, &dir);
    let res = resolved(&bin, &wal);
    install(&options(Platform::ClaudeCode, &res, &env)).expect("首次安装");
    let before = fs::read(dir.join(".mcp.json")).expect("读当前内容");
    let report = install(&options(Platform::ClaudeCode, &res, &env)).expect("二次安装");
    assert_eq!(report.state, InstallState::AlreadyCurrent);
    assert!(report.backup.is_none(), "没改动就不该产生备份");
    assert!(
        report.diff.is_empty(),
        "没改动就没有 diff: {:?}",
        report.diff
    );
    let after = fs::read(dir.join(".mcp.json")).expect("读当前内容");
    assert_eq!(before, after, "已装齐 = 逐字节不动");
    assert!(!dir.join(".mcp.json.wanning.bak").exists());
}

#[test]
fn corrupt_json_refused_and_unchanged() {
    let dir = temp_dir("corrupt");
    let bin = dummy_bin(&dir);
    let path = dir.join(".mcp.json");
    write(&path, "{oops 不是 JSON");
    let env = env_for(&dir, &dir);
    let res = resolved(&bin, &dir.join("wal.jsonl"));
    let message = expect_err(install(&options(Platform::ClaudeCode, &res, &env)));
    assert!(message.contains("JSON"), "报错要点名 JSON 解析: {message}");
    assert_eq!(read(&path), "{oops 不是 JSON", "fail-closed 绝不碰坏文件");
}

#[test]
fn non_object_root_refused() {
    let dir = temp_dir("root-array");
    let bin = dummy_bin(&dir);
    let path = dir.join(".mcp.json");
    write(&path, "[1, 2, 3]");
    let env = env_for(&dir, &dir);
    let res = resolved(&bin, &dir.join("wal.jsonl"));
    let message = expect_err(install(&options(Platform::ClaudeCode, &res, &env)));
    assert!(
        message.contains("对象"),
        "报错要点名顶层不是对象: {message}"
    );
    assert_eq!(read(&path), "[1, 2, 3]");
}

#[test]
fn mcp_servers_not_object_refused() {
    let dir = temp_dir("servers-number");
    let bin = dummy_bin(&dir);
    let path = dir.join(".mcp.json");
    write(&path, r#"{"mcpServers": 5}"#);
    let env = env_for(&dir, &dir);
    let res = resolved(&bin, &dir.join("wal.jsonl"));
    let message = expect_err(install(&options(Platform::ClaudeCode, &res, &env)));
    assert!(
        message.contains("mcpServers"),
        "报错要点名 mcpServers 形状: {message}"
    );
    assert_eq!(read(&path), r#"{"mcpServers": 5}"#);
}

#[test]
fn dry_run_changes_nothing() {
    // 升级场景:有他人条目 + 旧 wanning 条目 → dry-run 什么都不写。
    let dir = temp_dir("dry-upgrade");
    let bin = dummy_bin(&dir);
    let path = dir.join(".mcp.json");
    let old = r#"{
  "mcpServers": {
    "other-tool": {"command": "node"},
    "wanning": {"command": "D:/old/wanning-mcp.exe", "args": ["--wal", "D:/old/wal.jsonl"]}
  }
}"#;
    write(&path, old);
    let env = env_for(&dir, &dir);
    let res = resolved(&bin, &dir.join("wal.jsonl"));
    let mut opts = options(Platform::ClaudeCode, &res, &env);
    opts.dry_run = true;
    let report = install(&opts).expect("dry-run 成功");
    assert_eq!(report.state, InstallState::DryRun);
    assert!(report.target.is_none(), "dry-run 不落盘");
    assert_eq!(read(&path), old, "文件逐字节原样");
    assert!(
        !dir.join(".mcp.json.wanning.bak").exists(),
        "dry-run 不产生备份"
    );
    assert!(
        !report.actions.is_empty(),
        "dry-run 打印将做的全部动作: {:?}",
        report.actions
    );

    // 全新场景:dry-run 不创建文件。
    let fresh = temp_dir("dry-fresh");
    let bin2 = dummy_bin(&fresh);
    let env2 = env_for(&fresh, &fresh);
    let res2 = resolved(&bin2, &fresh.join("wal.jsonl"));
    let mut opts2 = options(Platform::Trae, &res2, &env2);
    opts2.dry_run = true;
    let report2 = install(&opts2).expect("dry-run 成功");
    assert_eq!(report2.state, InstallState::DryRun);
    assert!(!fresh.join(".trae").exists(), "dry-run 连目录都不建");
}

#[test]
fn project_paths_per_platform() {
    for (platform, relative) in [
        (Platform::Trae, ".trae/mcp.json"),
        (Platform::Kimi, ".kimi-code/mcp.json"),
        (Platform::WorkBuddy, ".workbuddy/mcp.json"),
    ] {
        let dir = temp_dir(&format!("path-{}", relative.replace('/', "-")));
        let bin = dummy_bin(&dir);
        let env = env_for(&dir, &dir);
        let res = resolved(&bin, &dir.join("wal.jsonl"));
        let report = install(&options(platform, &res, &env)).expect("安装成功");
        assert_eq!(report.state, InstallState::Fresh);
        assert_eq!(
            report.target.as_deref(),
            Some(dir.join(relative).as_path()),
            "{relative} 落在项目级目录(父目录自动创建)"
        );
        let document: Value = serde_json::from_str(&read(&dir.join(relative))).expect("JSON");
        assert_eq!(
            document["mcpServers"]["wanning"],
            generated_entry(platform, &bin, &dir.join("wal.jsonl"))
        );
    }
}

#[test]
fn install_entry_matches_generate_output() {
    // install 的条目与 generate() 的 stdout 产物同源:逐字段全等(含 type: stdio)。
    let dir = temp_dir("same-source");
    let bin = dummy_bin(&dir);
    let wal = dir.join("wal.jsonl");
    let env = env_for(&dir, &dir);
    let res = resolved(&bin, &wal);
    install(&options(Platform::ClaudeCode, &res, &env)).expect("安装成功");
    let document: Value = serde_json::from_str(&read(&dir.join(".mcp.json"))).expect("JSON");
    assert_eq!(
        document["mcpServers"]["wanning"],
        generated_entry(Platform::ClaudeCode, &bin, &wal)
    );
    assert_eq!(
        document["mcpServers"]["wanning"]["type"],
        json!("stdio"),
        "claude-code 需要 type: stdio(W-19 实测同款)"
    );
}

// ── B. deepseek-harness(Cordis patch 文本块级 merge) ────────────────────

fn dsh_options<'a>(resolved: &'a Resolved, env: &'a InstallEnv) -> InstallOptions<'a> {
    InstallOptions {
        platform: Platform::DeepSeekHarness,
        resolved,
        env,
        dry_run: false,
        yes: false,
        host_bin: None,
    }
}

#[test]
fn dsh_fresh_creates_patch_file() {
    let dir = temp_dir("dsh-fresh");
    let bin = dummy_bin(&dir);
    let dsh_home = dir.join("dshhome");
    let env = InstallEnv {
        dsh_home: Some(&dsh_home),
        ..env_for(&dir, &dir)
    };
    let res = resolved(&bin, &dir.join("wal.jsonl"));
    let report = install(&dsh_options(&res, &env)).expect("安装成功");
    assert_eq!(report.state, InstallState::Fresh);
    let target = dsh_home.join("cordis.patch.yml");
    assert_eq!(report.target.as_deref(), Some(target.as_path()));
    let content = read(&target);
    assert!(content.contains("- insert:"), "patch entry 形态: {content}");
    assert!(content.contains("id: wanning-gate"));
    assert!(content.contains("serverName: wanning"));
}

#[test]
fn dsh_appends_into_existing_patch_preserving_bytes() {
    let dir = temp_dir("dsh-append");
    let bin = dummy_bin(&dir);
    let dsh_home = dir.join("dshhome");
    let target = dsh_home.join("cordis.patch.yml");
    let existing = concat!(
        "# my own patch layer\n",
        "- insert:\n",
        "    - id: other-plugin\n",
        "      name: '@other/plugin'\n",
        "      config:\n",
        "        serverName: other\n",
    );
    write(&target, existing);
    let env = InstallEnv {
        dsh_home: Some(&dsh_home),
        ..env_for(&dir, &dir)
    };
    let res = resolved(&bin, &dir.join("wal.jsonl"));
    let report = install(&dsh_options(&res, &env)).expect("安装成功");
    assert_eq!(report.state, InstallState::Updated);
    let content = read(&target);
    assert!(
        content.contains(existing.trim_end()),
        "他人块逐字节保留: {content}"
    );
    assert!(
        content.contains("id: wanning-gate"),
        "wanning 块合并追加: {content}"
    );
    assert!(
        content.find("other-plugin").unwrap() < content.find("wanning-gate").unwrap(),
        "追加在文件尾,不动他人块位置"
    );
    assert!(report.backup.is_some(), "写前有备份");
}

#[test]
fn dsh_replaces_existing_wanning_block_with_diff() {
    let dir = temp_dir("dsh-replace");
    let bin = dummy_bin(&dir);
    let dsh_home = dir.join("dshhome");
    let target = dsh_home.join("cordis.patch.yml");
    let existing = concat!(
        "- insert:\n",
        "    - id: other-plugin\n",
        "      name: '@other/plugin'\n",
        "      config:\n",
        "        serverName: other\n",
        "- insert:\n",
        "    - id: wanning-gate\n",
        "      name: '@deepseek-ai/dsh-mcp-client'\n",
        "      config:\n",
        "        serverName: wanning\n",
        "        transport: stdio\n",
        "        command: D:/old/wanning-mcp.exe\n",
        "        args: [\"--wal\", \"D:/old/wal.jsonl\", \"--budget\", \"500\"]\n",
        "        env: {}\n",
    );
    write(&target, existing);
    let env = InstallEnv {
        dsh_home: Some(&dsh_home),
        ..env_for(&dir, &dir)
    };
    let res = resolved(&bin, &dir.join("wal.jsonl"));
    let report = install(&dsh_options(&res, &env)).expect("安装成功");
    assert_eq!(report.state, InstallState::Updated);
    assert!(!report.diff.is_empty(), "升级场景打 diff");
    let content = read(&target);
    assert!(content.contains("other-plugin"), "他人块保留");
    assert!(!content.contains("D:/old"), "旧块被替换: {content}");
    assert!(content.contains("id: wanning-gate"));
    assert!(content.contains(&slash(&bin)), "新 command 写进块内");
    assert!(content.contains(&slash(&dir.join("wal.jsonl"))));
    assert!(
        content.find("other-plugin").unwrap() < content.find("wanning-gate").unwrap(),
        "替换只发生在原 wanning 块位置,他人块不动"
    );
}

#[test]
fn dsh_non_list_top_level_refused() {
    let dir = temp_dir("dsh-bad");
    let bin = dummy_bin(&dir);
    let dsh_home = dir.join("dshhome");
    let target = dsh_home.join("cordis.patch.yml");
    write(&target, "not-a-list: true\n");
    let env = InstallEnv {
        dsh_home: Some(&dsh_home),
        ..env_for(&dir, &dir)
    };
    let res = resolved(&bin, &dir.join("wal.jsonl"));
    let message = expect_err(install(&dsh_options(&res, &env)));
    assert!(
        message.contains("insert 列表") || message.contains("列表"),
        "报错要点名顶层不是 insert 列表: {message}"
    );
    assert_eq!(read(&target), "not-a-list: true\n", "fail-closed 不碰文件");
}

#[test]
fn dsh_without_home_fails_closed() {
    let dir = temp_dir("dsh-nohome");
    let bin = dummy_bin(&dir);
    let env = InstallEnv {
        dsh_home: None,
        ..env_for(&dir, &dir)
    };
    let res = resolved(&bin, &dir.join("wal.jsonl"));
    let message = expect_err(install(&dsh_options(&res, &env)));
    assert!(
        message.contains("DSH_HOME"),
        "不猜落点,报错点名 DSH_HOME: {message}"
    );
}

// ── C. openclaw / hermes(宿主 CLI;--yes 才执行) ────────────────────────

/// 假宿主脚本:把 argv 逐行 + stdin 首行 + ARGV_END 写进盘上固定文件(路径烤进
/// 脚本,不经环境变量传,避免测试进程 env 竞态),再以给定退出码退出。
#[cfg(windows)]
fn fake_host(dir: &Path, name: &str, exit_code: i32) -> PathBuf {
    let captured = dir.join(format!("{name}-captured.txt"));
    let script = format!(
        concat!(
            "@echo off\r\n",
            "setlocal enabledelayedexpansion\r\n",
            "set \"OUT={}\"\r\n",
            ":loop\r\n",
            "if \"%~1\"==\"\" goto args_done\r\n",
            "echo %~1>>\"%OUT%\"\r\n",
            "shift\r\n",
            "goto loop\r\n",
            ":args_done\r\n",
            "set /p FAKE_STDIN=\r\n",
            "if defined FAKE_STDIN echo STDIN=!FAKE_STDIN!>>\"%OUT%\"\r\n",
            "echo ARGV_END>>\"%OUT%\"\r\n",
            "exit /b {}\r\n",
        ),
        captured.display(),
        exit_code
    );
    let path = dir.join(format!("{name}.cmd"));
    fs::write(&path, script).expect("写假宿主");
    path
}

#[cfg(unix)]
fn fake_host(dir: &Path, name: &str, exit_code: i32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let captured = dir.join(format!("{name}-captured.txt"));
    let script = format!(
        concat!(
            "#!/bin/sh\n",
            "out='{}'\n",
            "for a in \"$@\"; do printf '%s\\n' \"$a\" >> \"$out\"; done\n",
            "line=$(head -n 1 2>/dev/null)\n",
            "if [ -n \"$line\" ]; then printf 'STDIN=%s\\n' \"$line\" >> \"$out\"; fi\n",
            "printf 'ARGV_END\\n' >> \"$out\"\n",
            "exit {}\n",
        ),
        captured.display(),
        exit_code
    );
    let path = dir.join(name);
    fs::write(&path, script).expect("写假宿主");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

fn captured(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}-captured.txt"))
}

fn host_options<'a>(
    platform: Platform,
    resolved: &'a Resolved,
    env: &'a InstallEnv,
    host_bin: &'a Path,
    yes: bool,
) -> InstallOptions<'a> {
    InstallOptions {
        platform,
        resolved,
        env,
        dry_run: false,
        yes,
        host_bin: Some(host_bin),
    }
}

#[test]
fn codex_install_unsupported_with_guidance() {
    let dir = temp_dir("codex");
    let bin = dummy_bin(&dir);
    let env = env_for(&dir, &dir);
    let res = resolved(&bin, &dir.join("wal.jsonl"));
    let message = expect_err(install(&options(Platform::Codex, &res, &env)));
    assert!(
        message.contains("codex") && message.contains("--out"),
        "报错要点名 codex 并给 --out 人工指引: {message}"
    );
    assert!(!dir.join("config.toml").exists(), "绝不乱写主配置");
}

#[test]
fn openclaw_without_yes_prints_only() {
    let dir = temp_dir("oc-print");
    let bin = dummy_bin(&dir);
    let env = env_for(&dir, &dir);
    let res = resolved(&bin, &dir.join("wal.jsonl"));
    let report = install(&options(Platform::OpenClaw, &res, &env)).expect("打印成功");
    assert_eq!(report.state, InstallState::HostPrinted);
    let printed = report.printed.expect("打印宿主命令行");
    assert!(printed.starts_with("openclaw mcp set wanning"), "{printed}");
    assert!(report.target.is_none(), "宿主 CLI 平台不落配置文件");
}

#[test]
fn openclaw_yes_executes_host_with_payload() {
    let dir = temp_dir("oc-exec");
    let bin = dummy_bin(&dir);
    let env = env_for(&dir, &dir);
    let res = resolved(&bin, &dir.join("wal.jsonl"));
    let fake = fake_host(&dir, "fake-openclaw", 0);
    let report =
        install(&host_options(Platform::OpenClaw, &res, &env, &fake, true)).expect("执行成功");
    assert_eq!(report.state, InstallState::HostExecuted);

    let lines = read(&captured(&dir, "fake-openclaw"));
    let mut parts: Vec<&str> = lines.lines().collect();
    assert_eq!(parts.pop(), Some("ARGV_END"), "argv 抓取完整: {lines}");
    assert!(
        !parts.iter().any(|line| line.starts_with("STDIN=")),
        "openclaw 不喂 stdin(确认是 hermes 的事)"
    );
    assert_eq!(
        parts[..3],
        ["mcp", "set", "wanning"],
        "子命令与 server 名: {lines}"
    );
    // argv 共 4 段:openclaw mcp set wanning '<payload>'(W-45 真宿主 2026.5.22
    // 验证的形态是权威,首版测试写 parts[4] 是数错下标)。
    //
    // 假宿主是 .cmd:抓到的 payload 是 std 对批处理宿主的 BatBadBut 转义层
    // (内层 `"` 被翻倍成 `""`,整段仍是一个 argv,不碎)。真宿主是 npm shim →
    // `%*` 原样转发 → node 的 CRT 解析层把引号段内的 `""` 还原成 `"`(W-45
    // 真宿主实测 payload 原样入库),所以在测试里做这层唯一的无损还原再比对。
    // 本仓生成的 payload 是紧凑 JSON,原文不含相邻 `""`,还原无歧义。
    let captured_payload = parts[3].replace("\"\"", "\"");
    let payload: Value = serde_json::from_str(&captured_payload).unwrap_or_else(|e| {
        panic!(
            "payload 解析失败 {e}; 原始抓取:
{lines}"
        );
    });
    assert_eq!(
        payload,
        json!({
            "command": slash(&bin),
            "args": ["--wal", slash(&dir.join("wal.jsonl")), "--budget", "1000"]
        })
    );
    // 打印给用户的命令行与实际执行的 payload 同源。
    let printed = report.printed.expect("打印宿主命令行");
    let quoted = printed
        .strip_prefix("openclaw mcp set wanning '")
        .and_then(|rest| rest.strip_suffix("'\n"))
        .expect("命令行形态");
    assert_eq!(quoted, captured_payload, "打印与执行的 payload 逐字节同源");
}

#[test]
fn hermes_yes_feeds_confirm_and_args() {
    let dir = temp_dir("hm-exec");
    let bin = dummy_bin(&dir);
    let env = env_for(&dir, &dir);
    let res = resolved(&bin, &dir.join("wal.jsonl"));
    let fake = fake_host(&dir, "fake-hermes", 0);
    let report =
        install(&host_options(Platform::Hermes, &res, &env, &fake, true)).expect("执行成功");
    assert_eq!(report.state, InstallState::HostExecuted);

    let lines = read(&captured(&dir, "fake-hermes"));
    let mut parts: Vec<&str> = lines.lines().collect();
    assert_eq!(parts.pop(), Some("ARGV_END"));
    assert!(
        parts.contains(&"STDIN=y"),
        "非 TTY 确认提示要喂 y(W-45 教训): {lines}"
    );
    // 脚本先抓 argv 再读 stdin,抓取文件里 STDIN 行排在 argv 后,比对 argv 前滤掉。
    let argv: Vec<&str> = parts
        .into_iter()
        .filter(|line| !line.starts_with("STDIN="))
        .collect();
    let expected = vec![
        "mcp".to_string(),
        "add".to_string(),
        "wanning".to_string(),
        "--command".to_string(),
        slash(&bin),
        "--args".to_string(),
        "--wal".to_string(),
        slash(&dir.join("wal.jsonl")),
        "--budget".to_string(),
        "1000".to_string(),
    ];
    assert_eq!(argv, expected, "argv 与生成器命令行同源: {lines}");
}

#[test]
fn host_nonzero_exit_is_fail_closed() {
    let dir = temp_dir("oc-fail");
    let bin = dummy_bin(&dir);
    let env = env_for(&dir, &dir);
    let res = resolved(&bin, &dir.join("wal.jsonl"));
    let fake = fake_host(&dir, "fake-openclaw", 3);
    let message = expect_err(install(&host_options(
        Platform::OpenClaw,
        &res,
        &env,
        &fake,
        true,
    )));
    assert!(
        message.contains("3") || message.contains("失败"),
        "宿主退出码要进报错: {message}"
    );
}

#[test]
fn host_not_found_fails_closed() {
    let dir = temp_dir("oc-nopath");
    let bin = dummy_bin(&dir);
    let env = InstallEnv {
        path_env: Some(OsStr::new("")),
        ..env_for(&dir, &dir)
    };
    let res = resolved(&bin, &dir.join("wal.jsonl"));
    let message = expect_err(install(&host_options(
        Platform::OpenClaw,
        &res,
        &env,
        Path::new("no-such-host"),
        true,
    )));
    assert!(message.contains("宿主"), "报错要点名宿主 CLI: {message}");
}

// ── D. read_installed_entry(doctor 复用的读取面) ────────────────────────

#[test]
fn reads_back_installed_entry() {
    let dir = temp_dir("read-back");
    let bin = dummy_bin(&dir);
    let wal = dir.join("wal.jsonl");
    let env = env_for(&dir, &dir);
    let res = resolved(&bin, &wal);
    install(&options(Platform::ClaudeCode, &res, &env)).expect("安装成功");
    let entry = read_installed_entry(Platform::ClaudeCode, &env)
        .expect("读取成功")
        .expect("已装应有条目");
    assert_eq!(entry.command, slash(&bin));
    assert_eq!(
        entry.args,
        vec![
            "--wal".to_string(),
            slash(&wal),
            "--budget".to_string(),
            "1000".to_string()
        ]
    );
}

#[test]
fn none_when_not_installed() {
    let dir = temp_dir("read-none");
    let env = env_for(&dir, &dir);
    for platform in [
        Platform::ClaudeCode,
        Platform::Trae,
        Platform::Kimi,
        Platform::WorkBuddy,
    ] {
        assert!(
            read_installed_entry(platform, &env)
                .expect("读取成功")
                .is_none(),
            "{platform:?} 未装应返回 None"
        );
    }
}

#[test]
fn corrupt_config_is_error() {
    let dir = temp_dir("read-corrupt");
    write(&dir.join(".mcp.json"), "{oops");
    let env = env_for(&dir, &dir);
    let message = expect_entry_err(read_installed_entry(Platform::ClaudeCode, &env));
    assert!(message.contains("JSON"), "{message}");
}

#[test]
fn parses_real_hermes_shape() {
    // 形状逐字取自 W-45 真宿主写盘(target/w45/hermes-home/config.yaml 的
    // mcp_servers 段;块列表 args + 单引号标量)。
    let dir = temp_dir("hermes-shape");
    let content = concat!(
        "mcp_servers:\n",
        "  wanning:\n",
        "    command: D:/Desktop_Projects/Wanning/target/debug/wanning-mcp.exe\n",
        "    args:\n",
        "      - --wal\n",
        "      - D:/Desktop_Projects/Wanning/target/w45/hm-wal.jsonl\n",
        "      - --budget\n",
        "      - '1000'\n",
        "    enabled: true\n",
        "\n",
        "# ── Security ──────────────────────────────────────────────────────────\n",
    );
    write(&dir.join("config.yaml"), content);
    let env = InstallEnv {
        hermes_home: Some(&dir),
        ..env_for(&dir, &dir)
    };
    let entry = read_installed_entry(Platform::Hermes, &env)
        .expect("读取成功")
        .expect("已装条目");
    assert_eq!(
        entry.command,
        "D:/Desktop_Projects/Wanning/target/debug/wanning-mcp.exe"
    );
    assert_eq!(
        entry.args,
        vec![
            "--wal",
            "D:/Desktop_Projects/Wanning/target/w45/hm-wal.jsonl",
            "--budget",
            "1000"
        ]
    );
}

#[test]
fn parses_real_openclaw_shape() {
    // 形状逐字取自 W-45/W-47 真宿主写盘(mcp.servers.<name> = {command, args})。
    let dir = temp_dir("openclaw-shape");
    write(
        &dir.join("openclaw.json"),
        r#"{
  "meta": {"lastStartedVersion": "2026.5.22"},
  "mcp": {
    "servers": {
      "wanning": {
        "command": "D:/x/wanning-mcp.exe",
        "args": ["--wal", "D:/y/wal.jsonl", "--budget", "1000"]
      }
    }
  }
}"#,
    );
    let env = InstallEnv {
        openclaw_state_dir: Some(&dir),
        ..env_for(&dir, &dir)
    };
    let entry = read_installed_entry(Platform::OpenClaw, &env)
        .expect("读取成功")
        .expect("已装条目");
    assert_eq!(entry.command, "D:/x/wanning-mcp.exe");
    assert_eq!(
        entry.args,
        vec!["--wal", "D:/y/wal.jsonl", "--budget", "1000"]
    );
}

#[test]
fn parses_codex_fragment() {
    // 形状 = W-36 生成器片段追加进 config.toml(codex 主配置是 TOML 文本面)。
    let dir = temp_dir("codex-shape");
    let content = concat!(
        "model = \"gpt-5\"\n",
        "\n",
        "[mcp_servers.wanning]\n",
        "command = 'D:/x/wanning-mcp.exe'\n",
        "args = [\"--wal\", \"D:/y/wal.jsonl\", \"--budget\", \"1000\"]\n",
        "\n",
        "[other_section]\n",
        "key = \"value\"\n",
    );
    write(&dir.join("config.toml"), content);
    let env = InstallEnv {
        codex_home: Some(&dir),
        ..env_for(&dir, &dir)
    };
    let entry = read_installed_entry(Platform::Codex, &env)
        .expect("读取成功")
        .expect("已装条目");
    assert_eq!(entry.command, "D:/x/wanning-mcp.exe");
    assert_eq!(
        entry.args,
        vec!["--wal", "D:/y/wal.jsonl", "--budget", "1000"]
    );
}

#[test]
fn parses_dsh_block() {
    let dir = temp_dir("dsh-shape");
    let bin = dummy_bin(&dir);
    let dsh_home = dir.join("dshhome");
    let env = InstallEnv {
        dsh_home: Some(&dsh_home),
        ..env_for(&dir, &dir)
    };
    let res = resolved(&bin, &dir.join("wal.jsonl"));
    install(&dsh_options(&res, &env)).expect("安装成功");
    let entry = read_installed_entry(Platform::DeepSeekHarness, &env)
        .expect("读取成功")
        .expect("已装条目");
    assert_eq!(entry.command, slash(&bin));
    assert_eq!(
        entry.args,
        vec![
            "--wal".to_string(),
            slash(&dir.join("wal.jsonl")),
            "--budget".to_string(),
            "1000".to_string()
        ]
    );
}

// ── E. CLI 面(--install / --dry-run / --yes / --host-bin) ───────────────

fn run_bin(args: &[&str], cwd: &Path) -> (i32, String, String) {
    run_bin_env(args, cwd, &[])
}

fn run_bin_env(args: &[&str], cwd: &Path, envs: &[(&str, &str)]) -> (i32, String, String) {
    let output = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env_remove("WANNING_HOME")
        .env_remove("DSH_HOME")
        .env_remove("HERMES_HOME")
        .env_remove("OPENCLAW_STATE_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("KIMI_CODE_HOME")
        .envs(envs.iter().copied())
        .output()
        .expect("spawn wanning-init");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn cli_install_reports_and_writes() {
    let dir = temp_dir("cli-install");
    let bin = dummy_bin(&dir);
    let (code, stdout, stderr) = run_bin(
        &[
            "--platform",
            "claude-code",
            "--install",
            "--bin",
            &slash(&bin),
            "--wal",
            &slash(&dir.join("wal.jsonl")),
        ],
        &dir,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("安装报告"), "stdout: {stdout}");
    assert!(stdout.contains(".mcp.json"), "报告要点名落点: {stdout}");
    assert!(dir.join(".mcp.json").exists());
}

#[test]
fn cli_install_dry_run_writes_nothing() {
    let dir = temp_dir("cli-dry");
    let bin = dummy_bin(&dir);
    let (code, stdout, stderr) = run_bin(
        &[
            "--platform",
            "claude-code",
            "--install",
            "--dry-run",
            "--bin",
            &slash(&bin),
            "--wal",
            &slash(&dir.join("wal.jsonl")),
        ],
        &dir,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("dry-run") || stdout.contains("不落盘"),
        "stdout: {stdout}"
    );
    assert!(!dir.join(".mcp.json").exists());
}

#[test]
fn cli_install_out_conflict_is_usage_error() {
    let dir = temp_dir("cli-conflict");
    let bin = dummy_bin(&dir);
    let (code, _, stderr) = run_bin(
        &[
            "--platform",
            "claude-code",
            "--install",
            "--out",
            "x.json",
            "--bin",
            &slash(&bin),
            "--wal",
            &slash(&dir.join("wal.jsonl")),
        ],
        &dir,
    );
    assert_eq!(code, 2, "用法错 = 退出码 2, stderr: {stderr}");
    assert!(stderr.contains("--out"), "报错要点名冲突旗标: {stderr}");
}

#[test]
fn cli_yes_alone_is_usage_error() {
    let dir = temp_dir("cli-yes-alone");
    let bin = dummy_bin(&dir);
    let (code, _, stderr) = run_bin(
        &[
            "--platform",
            "openclaw",
            "--yes",
            "--bin",
            &slash(&bin),
            "--wal",
            &slash(&dir.join("wal.jsonl")),
        ],
        &dir,
    );
    assert_eq!(code, 2, "--yes 不带 --install = 用法错, stderr: {stderr}");
    assert!(stderr.contains("--yes"), "{stderr}");

    let (code, _, stderr) = run_bin(
        &[
            "--platform",
            "claude-code",
            "--dry-run",
            "--bin",
            &slash(&bin),
            "--wal",
            &slash(&dir.join("wal.jsonl")),
        ],
        &dir,
    );
    assert_eq!(
        code, 2,
        "--dry-run 不带 --install = 用法错, stderr: {stderr}"
    );
    assert!(stderr.contains("--dry-run"), "{stderr}");
}

#[test]
fn cli_unsupported_platform_exit_1() {
    let dir = temp_dir("cli-codex");
    let bin = dummy_bin(&dir);
    let (code, _, stderr) = run_bin(
        &[
            "--platform",
            "codex",
            "--install",
            "--bin",
            &slash(&bin),
            "--wal",
            &slash(&dir.join("wal.jsonl")),
        ],
        &dir,
    );
    assert_eq!(code, 1, "不支持 = 运行失败(不是用法错), stderr: {stderr}");
    assert!(stderr.contains("--out"), "给人工指引: {stderr}");
}

#[test]
fn cli_no_install_stdout_unchanged() {
    let dir = temp_dir("cli-no-install");
    let bin = dummy_bin(&dir);
    let (code, stdout, stderr) = run_bin(
        &[
            "--platform",
            "claude-code",
            "--bin",
            &slash(&bin),
            "--wal",
            &slash(&dir.join("wal.jsonl")),
        ],
        &dir,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.starts_with("# Wanning 支付闸 — 配置生成完成"),
        "W-36 契约:开头仍是生成头, stdout: {stdout}"
    );
    assert!(stdout.contains("\"mcpServers\""), "仍然打印配置内容");
    assert!(
        !stdout.contains("已写入"),
        "无 --install 不该出现「已写入」"
    );
    assert!(!dir.join(".mcp.json").exists(), "无 --install 零文件副作用");
}

#[test]
fn cli_openclaw_yes_runs_fake_host() {
    let dir = temp_dir("cli-oc-yes");
    let bin = dummy_bin(&dir);
    let fake = fake_host(&dir, "fake-openclaw", 0);
    let (code, stdout, stderr) = run_bin(
        &[
            "--platform",
            "openclaw",
            "--install",
            "--yes",
            "--host-bin",
            &slash(&fake),
            "--bin",
            &slash(&bin),
            "--wal",
            &slash(&dir.join("wal.jsonl")),
        ],
        &dir,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("openclaw mcp set wanning"),
        "stdout: {stdout}"
    );
    let lines = read(&captured(&dir, "fake-openclaw"));
    assert!(
        lines.lines().any(|line| line == "wanning"),
        "真执行了宿主 CLI: {lines}"
    );
}

#[test]
fn cli_openclaw_yes_host_failure_exit_1() {
    let dir = temp_dir("cli-oc-fail");
    let bin = dummy_bin(&dir);
    let fake = fake_host(&dir, "fake-openclaw", 7);
    let (code, _, stderr) = run_bin(
        &[
            "--platform",
            "openclaw",
            "--install",
            "--yes",
            "--host-bin",
            &slash(&fake),
            "--bin",
            &slash(&bin),
            "--wal",
            &slash(&dir.join("wal.jsonl")),
        ],
        &dir,
    );
    assert_eq!(code, 1, "宿主失败 = 运行失败, stderr: {stderr}");
    assert!(stderr.contains("7"), "退出码要进报错: {stderr}");
}

#[test]
fn cli_dsh_install_via_env() {
    let dir = temp_dir("cli-dsh");
    let bin = dummy_bin(&dir);
    let dsh_home = dir.join("dshhome");
    let (code, stdout, stderr) = run_bin_env(
        &[
            "--platform",
            "deepseek-harness",
            "--install",
            "--bin",
            &slash(&bin),
            "--wal",
            &slash(&dir.join("wal.jsonl")),
        ],
        &dir,
        &[("DSH_HOME", &slash(&dsh_home))],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(dsh_home.join("cordis.patch.yml").exists());
    assert!(stdout.contains("cordis.patch.yml"), "stdout: {stdout}");
}
