//! W-51b `wanning doctor`:挂载面体检——「装完 init 之后、第一次开闸之前」的
//! 验证命令,与 `wanning init --install`(W-51a)合成三命令流。
//!
//! 六项检查(任务书 `Wanning-oss/W-51-seamless-journey.md` W-51b):
//! ① wanning-mcp 二进制存在 + 版本(从 PATH 解析,与 wanning init 同一解析口);
//! ② 平台配置条目解析 + 语义校验(command 是真实文件,args 含 --wal/--budget);
//! ③ **真握手**:隔离临时账本拉起配置指向的 wanning-mcp,initialize →
//!    tools/list 完整往返——零模型、零外网、零真实消费;配置里写的账本一个
//!    字节都不碰(单写者锁语义下,体检去抢用户账本的锁既多余又危险);
//! ④ 审计账本所在目录可写(探测文件落了即删);
//! ⑤ 真实消费就绪度:复用 [`wanning_demo::guard`] 的 EnvSnapshot/GuardDenied,
//!    输出与 GuardDenied Display **同源**(原文照印,不另写一份缺项措辞);
//!    只读 env、信息项——护栏齐 ≠ 能花钱,绝不影响体检结论;
//! ⑥ 版本一致性:配置指向的 bin 版本 ≠ 当前 wanning → 提示重跑
//!    `wanning init --platform <名> --install`。
//!
//! 纪律:每项 ❌ 都带 ✗ 修复命令;未安装平台在扫描模式跳过(一个都没装 =
//! 体检不过)、显式 `--platform` 点名时算 ❌;配置形状坏 = 拒绝解读(fail-closed,
//! 绝不静默当未装)。零网络、零真实消费、零模型会话。

use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::json;
use wanning_demo::guard;
use wanning_init::install::{read_installed_entry, InstallEnv, InstallError, InstalledEntry};
use wanning_init::{parse_platform, Platform};

use crate::slash;

const USAGE: &str = "wanning doctor:体检——挂载面六项检查(零模型、零外网、零真实消费)

用法: wanning doctor [--platform <名>]

  --platform <名>   只体检这一个平台;缺省扫描全部八平台(未安装的跳过)
  -h / --help       打印本说明后退出

六项检查:
  ① wanning-mcp 二进制存在 + 版本(从 PATH 解析,与 wanning init 同一解析)
  ② 平台配置条目解析 + 语义校验(command 是真实文件,args 含 --wal/--budget)
  ③ 真握手:隔离临时账本拉起配置指向的 wanning-mcp,initialize → tools/list
     完整往返(绝不碰配置里写的账本)
  ④ 审计账本所在目录可写(探测文件落了即删)
  ⑤ 真实消费就绪度:只读环境变量,列出缺什么(护栏原文照印);信息项
  ⑥ 版本一致性:配置指向的 bin 版本 ≠ 当前 wanning → 提示重跑 init --install

退出码:0 全绿;1 有 ❌(每项 ✗ 后面带修复命令);2 用法错。
";

/// doctor 的错误分层,与 [`crate::CmdError`] 同构(用法错 2 / 运行失败 1)。
pub enum DoctorError {
    Usage(String),
    Failed(String),
}

/// 与 `wanning init` 的平台矩阵同一组名字;扫描模式按这个顺序覆盖。
const PLATFORM_NAMES: &[&str] = &[
    "claude-code",
    "codex",
    "kimi",
    "trae",
    "workbuddy",
    "deepseek-harness",
    "openclaw",
    "hermes",
];

/// MCP stdio 握手用的协议版本(wanning-mcp 的协商实现按 spec 回自己支持的最高版,
/// doctor 提议这个版本,两端口径一致)。
const DOCTOR_PROTOCOL_VERSION: &str = "2025-06-18";

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub fn run(args: &[String]) -> Result<(), DoctorError> {
    let mut platform_input: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--platform" => {
                platform_input = Some(next_value(args, &mut index, "--platform")?);
            }
            other => {
                return Err(DoctorError::Usage(format!(
                    "未知参数 '{other}'(用法:wanning doctor [--platform <名>];--help 看六项检查)"
                )))
            }
        }
        index += 1;
    }

    let cwd = std::env::current_dir()
        .map_err(|e| DoctorError::Failed(format!("解析当前目录失败: {e}")))?;
    // 进程环境只在 CLI 层读(库层纪律与 wanning-init 的 install 面一致)。
    let dsh_home = std::env::var_os("DSH_HOME").map(PathBuf::from);
    let openclaw_state_dir = std::env::var_os("OPENCLAW_STATE_DIR").map(PathBuf::from);
    let hermes_home = std::env::var_os("HERMES_HOME").map(PathBuf::from);
    let kimi_code_home = std::env::var_os("KIMI_CODE_HOME").map(PathBuf::from);
    let codex_home = std::env::var_os("CODEX_HOME").map(PathBuf::from);
    let path_env = std::env::var_os("PATH");
    let env = InstallEnv {
        cwd: &cwd,
        home: None,
        dsh_home: dsh_home.as_deref(),
        openclaw_state_dir: openclaw_state_dir.as_deref(),
        hermes_home: hermes_home.as_deref(),
        kimi_code_home: kimi_code_home.as_deref(),
        codex_home: codex_home.as_deref(),
        path_env: path_env.as_deref(),
    };

    let platforms: Vec<(&str, Platform)> = match &platform_input {
        Some(input) => {
            let platform = parse_platform(input).map_err(|e| DoctorError::Usage(e.message()))?;
            vec![(input.as_str(), platform)]
        }
        None => PLATFORM_NAMES
            .iter()
            .map(|name| (*name, parse_platform(name).expect("矩阵内平台必解析")))
            .collect(),
    };

    println!("Wanning 支付闸体检(wanning doctor)");
    println!();

    let mut failures = check_binary(path_env.as_deref());
    println!();

    let mut installed_any = false;
    for (name, platform) in &platforms {
        println!("── {name} ──");
        println!("② 配置条目解析");
        match read_installed_entry(*platform, &env) {
            Err(e) => {
                // 落点指针没设(如 DSH_HOME 空)= 这个平台根本没在用:扫描模式按
                // 未安装跳过;其余形状错/IO 错 = 配置坏了,fail-closed 绝不当没装。
                if matches!(e, InstallError::TargetUnresolved(_)) && platform_input.is_none() {
                    println!("  ⏭ 未安装(扫描模式跳过;{})", e.message());
                } else {
                    failures += 1;
                    println!("  ❌ {name} 的配置坏了,拒绝解读:{}", e.message());
                    println!("    ✗ 修复:修好该配置文件(或备份后删掉)再重跑 wanning doctor");
                    print_skips();
                }
            }
            Ok(None) => {
                if platform_input.is_some() {
                    failures += 1;
                    println!(
                        "  ❌ {name} 未安装(配置里没有 wanning 条目)。\
                         ✗ 修复:wanning init --platform {name} --install"
                    );
                    print_skips();
                } else {
                    println!(
                        "  ⏭ 未安装(扫描模式跳过;安装:wanning init --platform {name} --install)"
                    );
                }
            }
            Ok(Some(entry)) => {
                installed_any = true;
                match check_entry(&entry) {
                    Ok(semantics) => {
                        println!(
                            "  ✅ {} → command={},--wal {}",
                            slash(&entry.path),
                            entry.command,
                            slash(&semantics.wal)
                        );
                        if semantics.budget.is_none() {
                            println!("    注:未写 --budget,闸用默认预算 1000 分");
                        }
                        println!("③ 真握手(隔离临时账本,零模型零外网零真实消费)");
                        match handshake_check(&entry.command, &entry.args) {
                            Ok(detail) => println!("  ✅ {detail}"),
                            Err(message) => {
                                failures += 1;
                                println!("  ❌ {message}");
                                if !message.contains("✗ 修复") {
                                    println!(
                                        "    ✗ 修复:重跑 `wanning init --platform {name} --install` \
                                         让配置指向健康的 wanning-mcp"
                                    );
                                }
                            }
                        }
                        println!("④ 账本目录可写");
                        match wal_dir_writable(&semantics.wal) {
                            Ok(detail) => println!("  ✅ {detail}"),
                            Err(message) => {
                                failures += 1;
                                println!("  ❌ {message}");
                            }
                        }
                        println!("⑥ 版本一致性");
                        match version_parity(&entry.command, name) {
                            Ok(detail) => println!("  ✅ {detail}"),
                            Err(message) => {
                                failures += 1;
                                println!("  ❌ {message}");
                            }
                        }
                    }
                    Err(message) => {
                        failures += 1;
                        println!("  ❌ {message}");
                        print_skips();
                    }
                }
            }
        }
        println!();
    }

    check_readiness();
    println!();

    if platform_input.is_none() && !installed_any {
        failures += 1;
        println!(
            "体检结论:❌ 八平台一个都没装。✗ 修复:挑你的编码工具跑 \
             `wanning init --platform <名> --install`(矩阵:`wanning init --help`),\
             然后重跑 wanning doctor"
        );
    } else if failures == 0 {
        println!("体检结论:✅ 全绿");
    } else {
        println!(
            "体检结论:❌ {failures} 项不过(上面每项 ✗ 后面是修复命令;修完重跑 wanning doctor)"
        );
    }

    if failures > 0 {
        return Err(DoctorError::Failed(format!("体检 {failures} 项不过")));
    }
    Ok(())
}

fn print_skips() {
    for title in [
        "③ 真握手(隔离临时账本,零模型零外网零真实消费)",
        "④ 账本目录可写",
        "⑥ 版本一致性",
    ] {
        println!("{title}");
        println!("  ⏭ 跳过(② 未过,前置依赖缺失)");
    }
}

fn next_value(args: &[String], index: &mut usize, flag: &str) -> Result<String, DoctorError> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| DoctorError::Usage(format!("{flag} 缺少取值(用 --help 看用法)")))
}

// ── ① 二进制 + 版本 ──────────────────────────────────────────────────────

fn check_binary(path_env: Option<&OsStr>) -> usize {
    println!("① wanning-mcp 二进制(PATH 解析,与 wanning init 同一解析)");
    match wanning_init::resolve_bin(None, path_env) {
        Ok(bin) => match probe_version(&bin) {
            Ok(text) => {
                println!("  ✅ {} — {text}", slash(&bin));
                0
            }
            Err(message) => {
                println!(
                    "  ❌ {} 存在,但跑不出 --version({message})。\
                     ✗ 修复:cargo install wanning-cli wanning-mcp 后重跑 init --install",
                    slash(&bin)
                );
                1
            }
        },
        Err(e) => {
            println!("  ❌ PATH 上找不到可用的 wanning-mcp:");
            for line in e.message().lines() {
                println!("    {line}");
            }
            1
        }
    }
}

/// `--version` 短路在 wanning-mcp 的参数解析之前:版本探测不要求 --wal。
fn probe_version(program: &Path) -> Result<String, String> {
    let output = Command::new(program)
        .arg("--version")
        .output()
        .map_err(|e| format!("无法启动({e})"))?;
    if !output.status.success() {
        return Err(format!("--version 退出码 {:?}", output.status.code()));
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return Err("--version 没有输出".to_string());
    }
    Ok(text)
}

// ── ② 条目语义校验 ──────────────────────────────────────────────────────

struct EntrySemantics {
    wal: PathBuf,
    budget: Option<String>,
}

fn check_entry(entry: &InstalledEntry) -> Result<EntrySemantics, String> {
    if !Path::new(&entry.command).is_file() {
        return Err(format!(
            "配置里的 command 指向 {},不是真实文件。\
             ✗ 修复:重跑 `wanning init --platform <名> --install`,或手工把 command \
             改成 wanning-mcp 的真实路径",
            entry.command
        ));
    }
    let mut wal: Option<PathBuf> = None;
    let mut budget: Option<String> = None;
    let mut index = 0;
    while index < entry.args.len() {
        match entry.args[index].as_str() {
            "--wal" => {
                let Some(value) = entry.args.get(index + 1) else {
                    return Err(
                        "--wal 缺取值。✗ 修复:wanning init --platform <名> --install 重装"
                            .to_string(),
                    );
                };
                wal = Some(PathBuf::from(value));
                index += 2;
                continue;
            }
            "--budget" | "--cap-cents" => {
                let Some(value) = entry.args.get(index + 1) else {
                    return Err(format!(
                        "{flag} 缺取值。✗ 修复:wanning init --platform <名> --install 重装",
                        flag = entry.args[index]
                    ));
                };
                if value.parse::<u64>().is_err() {
                    return Err(format!(
                        "--budget 不是非负整数(分): {value}。✗ 修复:改配置里的 --budget 取值"
                    ));
                }
                budget = Some(value.clone());
                index += 2;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    let Some(wal) = wal else {
        return Err("配置条目缺 --wal <账本路径>(没有审计的闸不服务)。\
             ✗ 修复:wanning init --platform <名> --install 重装"
            .to_string());
    };
    Ok(EntrySemantics { wal, budget })
}

// ── ③ 真握手(隔离临时账本) ──────────────────────────────────────────────

fn handshake_check(program: &str, args: &[String]) -> Result<String, String> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let temp_dir = std::env::temp_dir().join("wanning-doctor");
    let _ = fs::create_dir_all(&temp_dir);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    // 名字带 pid+序号+纳秒(W-21 教训):并行体检/测试绝不撞同一把单写者锁。
    let temp_wal = temp_dir.join(format!(
        "handshake-{}-{}-{}.jsonl",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
        nanos
    ));
    let spawn_args = args_without_wal(args);
    let mut spawn_args = spawn_args;
    spawn_args.push("--wal".to_string());
    spawn_args.push(slash(&temp_wal));

    let mut command = Command::new(program);
    command
        .args(&spawn_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| {
        format!("拉起 {program} 失败: {e}。✗ 修复:核对 ② 里 command 指向的二进制还能不能跑")
    })?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel::<std::io::Result<String>>();
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    let _ = tx.send(Err(std::io::Error::other(
                        "server 提前关闭了 stdout(没按 MCP 协议应答)",
                    )));
                    break;
                }
                Ok(_) => {
                    if tx.send(Ok(line.clone())).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    break;
                }
            }
        }
    });

    let steps = run_handshake_steps(&mut stdin, &rx);
    let stderr_text = shutdown(child, stdin);
    let _ = reader.join();
    cleanup_temp_wal(&temp_wal);
    match steps {
        Ok(summary) => Ok(summary),
        Err(message) => {
            if stderr_text.trim().is_empty() {
                Err(message)
            } else {
                Err(format!("{message};server stderr: {}", stderr_text.trim()))
            }
        }
    }
}

/// 配置 args 里原样保留一切,只把 `--wal <旧账本>` 对摘掉(换成隔离临时账本);
/// 没有 --wal 也行(② 已保证有)。
fn args_without_wal(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--wal" {
            index += 2;
            continue;
        }
        out.push(args[index].clone());
        index += 1;
    }
    out
}

fn run_handshake_steps(
    stdin: &mut ChildStdin,
    rx: &mpsc::Receiver<std::io::Result<String>>,
) -> Result<String, String> {
    send(
        stdin,
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": DOCTOR_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "wanning-doctor", "version": env!("CARGO_PKG_VERSION")}
            }
        }),
    )?;
    let init_line = read_response(rx)?;
    let init: serde_json::Value = serde_json::from_str(init_line.trim())
        .map_err(|e| format!("initialize 响应不是 JSON: {e}"))?;
    let reported = init["result"]["protocolVersion"]
        .as_str()
        .unwrap_or_default();
    if reported != DOCTOR_PROTOCOL_VERSION {
        return Err(format!(
            "initialize 协商出协议版本 {reported:?},期望 {DOCTOR_PROTOCOL_VERSION}"
        ));
    }
    send(
        stdin,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )?;
    send(
        stdin,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )?;
    let tools_line = read_response(rx)?;
    let tools: serde_json::Value = serde_json::from_str(tools_line.trim())
        .map_err(|e| format!("tools/list 响应不是 JSON: {e}"))?;
    let names: Vec<String> = tools["result"]["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for name in ["wanning_gate_evaluate", "wanning_audit_tail"] {
        if !names.iter().any(|actual| actual == name) {
            return Err(format!("工具面缺 {name}(实际:{names:?})"));
        }
    }
    Ok(format!(
        "initialize + tools/list 往返成功:{} 个工具({})",
        names.len(),
        names.join(" / ")
    ))
}

fn send(stdin: &mut ChildStdin, message: &serde_json::Value) -> Result<(), String> {
    writeln!(stdin, "{message}").map_err(|e| format!("写 stdin 失败(server 可能已死): {e}"))?;
    stdin.flush().map_err(|e| format!("flush stdin 失败: {e}"))
}

fn read_response(rx: &mpsc::Receiver<std::io::Result<String>>) -> Result<String, String> {
    match rx.recv_timeout(HANDSHAKE_TIMEOUT) {
        Ok(Ok(line)) => Ok(line),
        Ok(Err(e)) => Err(format!("读不到 server 响应: {e}")),
        Err(_) => Err(format!(
            "等待响应超时({}s):配置指向的程序没有按 MCP 协议应答",
            HANDSHAKE_TIMEOUT.as_secs()
        )),
    }
}

/// 收尾:先关 stdin(server 见 EOF 自退),kill 只是兜底;随后回收进程并读走
/// stderr 供失败诊断(stdout 已被 reader 线程接管,wait_with_output 只收 stderr)。
/// kill 在已退出的进程上是无害错误。
fn shutdown(mut child: Child, stdin: ChildStdin) -> String {
    drop(stdin);
    let _ = child.kill();
    match child.wait_with_output() {
        Ok(output) => String::from_utf8_lossy(&output.stderr).into_owned(),
        Err(_) => String::new(),
    }
}

/// 清掉体检自己的临时账本与残留单写者锁(kill 路径下 WalLock 的 Drop 不会跑,
/// 锁文件会留在盘上——只清自己名字下的,绝不猜别人的)。
fn cleanup_temp_wal(wal: &Path) {
    let _ = fs::remove_file(wal);
    let _ = fs::remove_file(wanning_core::wal::single_writer_lock_path(wal));
}

// ── ④ 账本目录可写 ──────────────────────────────────────────────────────

fn wal_dir_writable(wal: &Path) -> Result<String, String> {
    let parent = match wal.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => std::env::current_dir().map_err(|e| format!("解析当前目录失败: {e}"))?,
    };
    let mut note = String::new();
    if !parent.exists() {
        // 与闸的启动行为一致(W-43a:Wal::open 拿锁前自动建父目录)——补建本身
        // 就是「这个位置能不能落账本」的真实答案,注明即可。
        fs::create_dir_all(&parent).map_err(|e| {
            format!(
                "账本目录 {} 不存在且建不出来: {e}。\
                 ✗ 修复:检查配置里 --wal 的父目录路径与权限",
                slash(&parent)
            )
        })?;
        note = "(目录此前不存在,已按闸的启动行为补建)".to_string();
    }
    static PROBE: AtomicU64 = AtomicU64::new(0);
    let probe = parent.join(format!(
        ".wanning-doctor-probe-{}-{}",
        std::process::id(),
        PROBE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&probe, b"probe").map_err(|e| {
        format!(
            "账本目录 {} 不可写: {e}。✗ 修复:给该目录写权限,或把配置里的 --wal 改到可写位置",
            slash(&parent)
        )
    })?;
    let _ = fs::remove_file(&probe);
    Ok(format!("{} 可写{note}", slash(&parent)))
}

// ── ⑤ 真实消费就绪度(信息项) ────────────────────────────────────────────

fn check_readiness() {
    println!("⑤ 真实消费就绪度(只读环境变量;信息项,不影响体检结论)");
    let snapshot = guard::EnvSnapshot::from_process_env();
    match guard::check_real_spend(&snapshot) {
        Ok(_) => println!(
            "  ℹ 护栏 env 条件已齐(WANNING_ALLOW_REAL_SPEND=1 + 4 个密钥都在)。\
             真实通道仍未接线——护栏过 ≠ 能花钱,真实下单仍是所有者动作。"
        ),
        Err(denied) => {
            // GuardDenied Display 原文照印(同源,不另写一份缺项措辞)。
            let denied_text = denied.to_string();
            let mut lines = denied_text.lines();
            if let Some(first) = lines.next() {
                println!("  ℹ {first}");
            }
            for line in lines {
                println!("    {line}");
            }
            println!("    真实通道尚未接线(账户未开通);护栏齐 ≠ 能花钱。");
        }
    }
}

// ── ⑥ 版本一致性 ────────────────────────────────────────────────────────

fn version_parity(entry_command: &str, platform_name: &str) -> Result<String, String> {
    let current = env!("CARGO_PKG_VERSION");
    let reported = probe_version(Path::new(entry_command)).map_err(|e| {
        format!(
            "配置指向的 {entry_command} 跑不出 --version({e})。\
             ✗ 修复:重跑 `wanning init --platform {platform_name} --install` 让配置指到健康的二进制"
        )
    })?;
    if reported.contains(current) {
        Ok(format!(
            "配置指向的 bin 自报「{reported}」,与当前 wanning({current})一致"
        ))
    } else {
        Err(format!(
            "配置指向的 bin 自报「{reported}」,当前 wanning 是 {current} —— 配置里是旧二进制。\
             ✗ 修复:重跑 `wanning init --platform {platform_name} --install` 让配置指到新二进制"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_matrix_covers_all_eight_and_stays_in_sync_with_init() {
        // 扫描模式吃这份名单;与 wanning-init 的 parse_platform 脱节 = 立刻红。
        assert_eq!(PLATFORM_NAMES.len(), 8);
        for name in PLATFORM_NAMES {
            assert!(
                parse_platform(name).is_ok(),
                "doctor 的平台名单与 wanning-init 脱节: {name}"
            );
        }
    }

    #[test]
    fn args_without_wal_keeps_everything_else() {
        let args: Vec<String> = [
            "--budget",
            "1000",
            "--wal",
            "X:/old.jsonl",
            "--max-spends",
            "5",
        ]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect();
        assert_eq!(
            args_without_wal(&args),
            vec![
                "--budget".to_string(),
                "1000".to_string(),
                "--max-spends".to_string(),
                "5".to_string()
            ]
        );
        // 没值/在尾巴上的 --wal 也不崩(② 保证正常情况有值,这里只验鲁棒)。
        assert_eq!(
            args_without_wal(&["--wal".to_string()]),
            Vec::<String>::new()
        );
    }
}
