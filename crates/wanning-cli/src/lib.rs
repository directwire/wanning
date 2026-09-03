//! wanning-cli:统一 CLI 入口 `wanning`(W-43a 产品化)。
//!
//! 北极星(任务书 `W-43-production-ready.md`):**一个不用 Rust 的新用户,从进仓库
//! 到闸口在跑 ≤10 分钟,全程照 README 抄命令**。此前入口散在 `wanning-demo` /
//! `wanning-init` / `wanning-anchor-verify` 三个 bin 名下,用户得先知道哪个是哪个;
//! 本 crate 收成单一入口:
//!
//! - `wanning init`:给编码工具生成 MCP 配置(委托 [`wanning_init::run_cli`],
//!   六平台写实路径零占位符,失败给安装指引);
//! - `wanning audit`:读审计账本汇总(行数/判定/链尾/预算台账),`--out` 同时导出
//!   HTML 回放页(复用 W-22 [`wanning_demo::audit_html`],坏账 fail-closed 绝不产出);
//! - `wanning demo`:离线演示场景(委托 [`wanning_demo::cli::run`] 同一段实现,
//!   真实消费护栏 W-07 原样生效——「统一入口」不等于绕过任何一道门);
//! - `wanning anchor-verify`:第三方零密钥验锚点(ed25519 v2,W-31)。
//! - `wanning ui`:本地只读仪表盘(W-43b,127.0.0.1 随机端口不监听外网;预算
//!   台账 + 判定实时滚动 + 一键撤销走闸本体;详见 [`ui`] 模块文档)。
//! - `wanning doctor`:挂载面体检(W-51b,与 `wanning init --install` 合成三命令
//!   流;真握手 + 账本可写 + 版本一致性 + 缺项清单;详见 [`doctor`] 模块文档)。
//!
//! 旧 bin 名(`wanning-demo` / `wanning-init` / `wanning-anchor-verify`)保留一个
//! 发行周期作 alias,全部走同一段 lib 实现,不会漂移成两套行为。
//!
//! 退出码纪律:0 成功;2 = 用法错(未知子命令/参数缺失);1 = 运行失败
//! (护栏拒/坏账/找不到账本)。零网络、零真实消费。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wanning_core::clock::{Clock, SystemClock};
use wanning_demo::anchor_v2;
use wanning_demo::audit_html;

pub mod doctor;
pub mod ui;

pub const USAGE: &str = "wanning —— Wanning 支付闸(意图层授权;闸今天就能用,通道等你插钥匙)

用法: wanning <子命令> [参数]

  wanning init --platform <名> [--bin <路径>] [--wal <路径>] [--out <文件> | --install]
      给编码工具生成 MCP 配置(claude-code / codex / kimi / trae / workbuddy /
      deepseek-harness / openclaw / hermes),写实路径零占位符,装完即用;
      `--install` 直写宿主配置的正确位置(merge 只动 wanning 条目,写前备份,
      `--dry-run` 零落盘预览;codex 拒装给人工指引);详情 `wanning init --help`
  wanning audit [<账本路径>] [--out <report.html>]
      读审计账本汇总(行数 / 判定 / 链尾 / 预算台账);不给路径读默认账本
      ~/.wanning/wal.jsonl;--out 同时导出自包含 HTML 回放页(坏账绝不产出)
  wanning demo --scenario <name> [--dry-run true|false]
      离线演示场景(全本地 mock,零真实消费);其余模式 `wanning demo --help`
  wanning ui [--wal <账本>] [--port <端口>]
      本地只读仪表盘(127.0.0.1,默认随机端口,不监听外网):预算余量 / 判定实时
      滚动 / 一键撤销(走闸本体,落审计);页面零 JS,自动刷新
  wanning anchor-verify --anchor <anchor.json> --wal <audit.jsonl> [--expect-key <64位hex>]
      第三方零密钥验锚点(ed25519 v2;公钥随锚点走,无需任何密钥文件)
  wanning doctor [--platform <名>]
      挂载面体检(装完 init 之后、第一次开闸之前跑):wanning-mcp 二进制 + 配置条目
      语义 + 真握手(隔离临时账本,零模型零外网零真实消费)+ 账本目录可写 + 真实
      消费就绪度清单 + 版本一致性;每项 ❌ 带 ✗ 修复命令
  wanning --version    版本
  wanning --help       本帮助

退出码:0 成功;2 用法错(未知子命令 / 参数缺失);1 运行失败(护栏拒 / 坏账 / 找不到账本)。
";

/// CLI 错误分层:用法错(退出码 2)与运行失败(退出码 1)。
enum CmdError {
    Usage(String),
    Failed(String),
}

/// 统一入口主体(`src/main.rs` 只是薄壳)。
pub fn run_cli(args: &[String]) -> ExitCode {
    let Some((command, rest)) = args.split_first() else {
        eprintln!("缺少子命令。\n{USAGE}");
        return ExitCode::from(2);
    };
    match command.as_str() {
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        "--version" | "-V" => {
            println!("wanning {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "init" => wanning_init::run_cli("wanning init", rest),
        "audit" => finish(audit_cmd(rest)),
        "demo" => finish(wanning_demo::cli::run(rest).map_err(CmdError::Failed)),
        "anchor-verify" => finish(anchor_verify_cmd(rest)),
        "ui" => finish(ui::run(rest).map_err(|e| match e {
            ui::UiStartError::Usage(message) => CmdError::Usage(message),
            ui::UiStartError::Failed(message) => CmdError::Failed(message),
        })),
        "doctor" => finish(doctor::run(rest).map_err(|e| match e {
            doctor::DoctorError::Usage(message) => CmdError::Usage(message),
            doctor::DoctorError::Failed(message) => CmdError::Failed(message),
        })),
        other => {
            eprintln!("未知子命令 '{other}'。\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn finish(result: Result<(), CmdError>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(CmdError::Usage(message)) => {
            eprintln!("wanning: {message}");
            ExitCode::from(2)
        }
        Err(CmdError::Failed(message)) => {
            eprintln!("wanning: {message}");
            ExitCode::FAILURE
        }
    }
}

// ── audit:读账本汇总 + --out 导出回放页 ─────────────────────────────────

fn audit_cmd(args: &[String]) -> Result<(), CmdError> {
    let mut wal: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--out" => out = Some(next_path(args, &mut index, "--out")?),
            // 位置参数与 --wal 同义(位置是 README 里抄的那条路;旗标给脚本用)。
            "--wal" => wal = Some(next_path(args, &mut index, "--wal")?),
            other => {
                if wal.is_some() {
                    return Err(CmdError::Usage(
                        "账本路径只给一个(位置参数或 --wal,二选一)".to_string(),
                    ));
                }
                if other.starts_with('-') {
                    return Err(CmdError::Usage(format!(
                        "未知参数 '{other}'(用法:wanning audit [<账本路径>] [--out <html>])"
                    )));
                }
                wal = Some(PathBuf::from(other));
            }
        }
        index += 1;
    }

    // 不给路径 = 产品默认账本(~/.wanning/wal.jsonl);家目录解析不出 = fail-closed,
    // 绝不猜一个落点。
    let wal = match wal {
        Some(wal) => wal,
        None => wanning_core::paths::default_wal_path().ok_or_else(|| {
            CmdError::Failed(
                "解析不出默认账本路径(WANNING_HOME / USERPROFILE / HOME 都没有)。\
                 用 `wanning audit <账本路径>` 显式给一个"
                    .to_string(),
            )
        })?,
    };
    if !wal.exists() {
        return Err(CmdError::Failed(format!(
            "审计账本不存在:{}(闸还没跑过任何判定?先 `wanning init` 挂上闸,或显式给账本路径)",
            slash(&wal)
        )));
    }

    // fail-closed 先于一切输出:验完整性链 + 回放对账两遍(build_report / export_audit
    // 内部做),任何一步不过,汇总与回放页一个字节都不产出。
    let report = match out {
        Some(out) => {
            let report = audit_html::export_audit(&wal, &out, Some(SystemClock.now()))
                .map_err(|e| CmdError::Failed(e.to_string()))?;
            println!(
                "审计回放页已导出:{}(零 JS 零外链,file:// 离线可开)",
                out.display()
            );
            report
        }
        None => audit_html::build_report(&wal)
            .map_err(|e| CmdError::Failed(format!("审计账本读取失败(fail-closed): {e}")))?,
    };
    print_summary(&report);
    Ok(())
}

fn print_summary(report: &audit_html::AuditReport) {
    println!(
        "审计账本:{}(完整性链逐行验证通过,回放对账两遍 hash 一致)",
        slash(Path::new(&report.wal_display))
    );
    println!("行数: {}", report.rows.len());
    println!(
        "判定: allow {} / deny {}(撤销 {})",
        report.counts.allow, report.counts.deny, report.counts.revoke
    );
    println!("回放对账:0x{:016x}", report.replay_state_hash);
    println!("链尾: 0x{:016x}", report.chain_tail);
    for delegation in &report.delegations {
        println!(
            "委托 {}:上限 {} 分,已花 {} 分,剩 {} 分{}",
            delegation.id,
            delegation.cap_cents,
            delegation.spent_cents,
            delegation.remaining_cents,
            if delegation.revoked {
                "(已撤销)"
            } else {
                ""
            }
        );
    }
    println!("证据以审计原文为准;逐行时间线:`wanning audit <账本> --out <report.html>`。");
}

// ── anchor-verify:第三方零密钥验签(ed25519 v2,W-31) ───────────────────

fn anchor_verify_cmd(args: &[String]) -> Result<(), CmdError> {
    let mut anchor: Option<PathBuf> = None;
    let mut wal: Option<PathBuf> = None;
    let mut expect_key: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--anchor" => anchor = Some(next_path(args, &mut index, "--anchor")?),
            "--wal" => wal = Some(next_path(args, &mut index, "--wal")?),
            "--expect-key" => {
                expect_key = Some(next_value(args, &mut index, "--expect-key")?.to_string())
            }
            other => {
                return Err(CmdError::Usage(format!(
                    "未知参数 '{other}'(用法:wanning anchor-verify --anchor <锚点> \
                     --wal <账本> [--expect-key <64位hex>])"
                )))
            }
        }
        index += 1;
    }
    let anchor = anchor.ok_or_else(|| CmdError::Usage("缺少 --anchor <锚点文件>".to_string()))?;
    let wal = wal.ok_or_else(|| CmdError::Usage("缺少 --wal <审计账本>".to_string()))?;

    // 验证顺序即 fail-closed 顺序:版本/schema → 公钥钉定 → ed25519 签名 →
    // WAL 完整性链 → 前缀逐字段比对(库面 [`wanning_demo::anchor_v2::verify_v2`])。
    match anchor_v2::verify_v2(&wal, &anchor, expect_key.as_deref()) {
        Ok(outcome) => {
            println!("锚点验证通过(v2,ed25519):{}", anchor.display());
            println!("  公钥(hex):{}", outcome.public_key_hex);
            println!(
                "  锚定行数:{} / 当前账本 {} 行(锚定后新增 {} 行,前缀锚不挡合法追加)",
                outcome.anchored_lines,
                outcome.current_lines,
                outcome.current_lines - outcome.anchored_lines
            );
            println!("  前缀链尾:0x{:016x}", outcome.chain_tail);
            println!("  前缀内容 SHA-256:{}", outcome.records_sha256_hex);
            println!(
                "  锚定时刻:{}(Unix 秒);证据以审计原文为准。",
                outcome.anchored_at_unix
            );
            if expect_key.is_none() {
                println!(
                    "注意:未钉定 --expect-key。签名只证明「持对应私钥者签的」,\
                     不证明「持钥者是所有者」——请从所有者公开渠道核对上面这行公钥。"
                );
            } else {
                println!("  期望公钥已钉定并与锚点一致(带外身份核对通过)。");
            }
            Ok(())
        }
        Err(e) => Err(CmdError::Failed(format!("锚点验证失败: {e}"))),
    }
}

// ── 小工具 ───────────────────────────────────────────────────────────────

/// 路径统一正斜杠:Windows 反斜杠在报错/配置里都要转义,正斜杠 Windows 也认
/// (与 wanning-init 的生成侧同一惯例)。
fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn next_value<'a>(args: &'a [String], index: &mut usize, flag: &str) -> Result<&'a str, CmdError> {
    *index += 1;
    args.get(*index)
        .map(String::as_str)
        .ok_or_else(|| CmdError::Usage(format!("{flag} 缺少取值(用 --help 看用法)")))
}

fn next_path(args: &[String], index: &mut usize, flag: &str) -> Result<PathBuf, CmdError> {
    Ok(PathBuf::from(next_value(args, index, flag)?))
}
