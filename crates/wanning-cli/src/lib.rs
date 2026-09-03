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
//! - `wanning channel-test`:渠道钥匙验证(W-52,L0→L1→L2→L3 分级阶梯绝不跳级;
//!   三重明示 fail-closed;定位 = 免密代扣(平台侧)的钥匙验证工具,个人用户旅程
//!   用不到;详见 [`channel_test`] 模块文档)。
//! - `wanning confirm`:人在环待支付确认(W-53b,**只在 CLI 人工面**——AI 不能
//!   确认 AI 自己的支付,确认动作因此绝不出现在 MCP 工具面上;金额一致 / 幂等 /
//!   TTL 三钉 fail-closed,被拒的确认一行都不落账)。
//!
//! 旧 bin 名(`wanning-demo` / `wanning-init` / `wanning-anchor-verify`)保留一个
//! 发行周期作 alias,全部走同一段 lib 实现,不会漂移成两套行为。
//!
//! 退出码纪律:0 成功;2 = 用法错(未知子命令/参数缺失);1 = 运行失败
//! (护栏拒/坏账/找不到账本)。零网络、零真实消费(除 channel-test 的 L2/L3
//! 显式授权阶梯,该阶梯自身有三重明示 fail-closed)。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wanning_core::clock::{Clock, SystemClock};
use wanning_demo::anchor_v2;
use wanning_demo::audit_html;

pub mod channel_test;
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
  wanning channel-test --channel <名> [--wal <账本>] [--evidence <目录>] [--real] [--real-spend]
      渠道钥匙验证(L0 环境齐套 → L1 签名自测零网络 → L2 真网关零资金探针 →
      L3 协议内 0.01 元真实扣款;分级阶梯绝不跳级)。三重明示 fail-closed 缺一即拒
      (WANNING_ALLOW_REAL_SPEND=1 + --real 显式 + TTY 交互确认;L3 追加 --real-spend)。
      定位 = 免密代扣(平台侧)的钥匙验证工具,个人用户旅程用不到;京东/微信/美团
      如实标不支持;详情 --help
  wanning confirm <单号> --amount <元> --proof <交易号> [--wal <账本>]
      人在环待支付确认(W-53b,人的显式动作,MCP 工具面绝不出现):闸放行后 AI 把
      单开在「待支付」,你付完款按本命令把支付凭证入账;金额必须与审批额一致,
      同一单只能确认一次,过期单拒(被拒的确认一行都不落账)。不给 --wal 读默认
      账本 ~/.wanning/wal.jsonl(与 `wanning init` 生成的配置同一本账)
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
        "channel-test" => finish(channel_test::run(rest).map_err(|e| match e {
            channel_test::ChannelTestError::Usage(message) => CmdError::Usage(message),
            channel_test::ChannelTestError::Failed(message) => CmdError::Failed(message),
        })),
        "confirm" => finish(confirm_cmd(rest)),
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
    // W-53a 人在环三段(待支付/确认/终态)有活动才打印:老账本的 stdout
    // 与 W-53 之前逐字节相同,不制造一次输出漂移;统计与 HTML 回放页的
    // KPI 瓦片同一来源(report.counts),不另写一套口径。
    let pending_family = report.counts.pending
        + report.counts.confirm
        + report.counts.terminal_completed
        + report.counts.terminal_voided;
    if pending_family > 0 {
        println!(
            "人在环:待支付 {} / 人确认 {}(完成 {} / 过期作废 {})",
            report.counts.pending,
            report.counts.confirm,
            report.counts.terminal_completed,
            report.counts.terminal_voided
        );
    }
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

// ── confirm:人在环待支付确认(W-53b;只在这一张人工脸上) ────────────────

/// `wanning confirm <单号> --amount <元> --proof <交易号> [--wal <账本>]`。
///
/// W-53b 的安全根:**确认是人的显式动作,只存在于这张 CLI 人工面**。AI 侧
/// (MCP 工具面)能做的止步于提交意图与只读查询——AI 不能确认 AI 自己的支付,
/// 否则人在环空转(wanning-mcp 的工具清单契约测试断言 confirm 字样零命中)。
///
/// 语义钉死(闸本体 [`wanning_core::state::WanningState::confirm_pending`] 的
/// 三钉原样生效,CLI 不另写一套判定):
/// 1. **金额一致**:`--amount` 由人亲手照审批额敲——审批 400 确认 500 = 拒
///    (防夹带,「限制 AI」的本体语义);元→分走 W-50 同一款严格解析
///    ([`wanning_demo::alipay::yuan_to_cents`],两位小数歧义零容忍,这是钱);
/// 2. **幂等**:同一单只能确认一次,二次确认 = 拒;
/// 3. **TTL**:过期单拒(作废本身落一行终态账)。
///
/// 被拒的确认一行都不落账(fail-closed);闸只记账不碰钱——支付本身发生在
/// 用户自己的渠道(手机按指纹等),`--proof` 是那笔支付的凭证,入账供回放对账。
fn confirm_cmd(args: &[String]) -> Result<(), CmdError> {
    let mut pending_id: Option<String> = None;
    let mut amount: Option<String> = None;
    let mut proof: Option<String> = None;
    let mut wal: Option<PathBuf> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--amount" => amount = Some(next_value(args, &mut index, "--amount")?.to_string()),
            "--proof" => proof = Some(next_value(args, &mut index, "--proof")?.to_string()),
            "--wal" => wal = Some(next_path(args, &mut index, "--wal")?),
            other => {
                if other.starts_with('-') {
                    return Err(CmdError::Usage(format!(
                        "未知参数 '{other}'(用法:wanning confirm <单号> --amount <元> \
                         --proof <交易号> [--wal <账本>])"
                    )));
                }
                if pending_id.is_some() {
                    return Err(CmdError::Usage("待支付单号只给一个(位置参数)".to_string()));
                }
                pending_id = Some(other.to_string());
            }
        }
        index += 1;
    }
    let pending_id = pending_id.ok_or_else(|| {
        CmdError::Usage("缺少待支付单号(形如 p-…,来自闸放行回执/待支付查询)".to_string())
    })?;
    let amount = amount.ok_or_else(|| {
        CmdError::Usage("缺少 --amount <元>(必须照审批额敲,分文不差)".to_string())
    })?;
    let proof = proof.ok_or_else(|| {
        CmdError::Usage("缺少 --proof <交易号>(支付凭证,回放对账靠它)".to_string())
    })?;
    if proof.trim().is_empty() {
        return Err(CmdError::Usage(
            "--proof 支付凭证为空:没有凭证的确认不是可对账的确认".to_string(),
        ));
    }
    // 元 → 分:W-50 同一款严格解析(0/1/2 位小数;负号/空白/三位小数一律拒)。
    // 人的手滑在这里挡下,绝不带歧义金额进闸。
    let amount_cents = wanning_demo::alipay::yuan_to_cents(&amount)
        .map_err(|e| CmdError::Usage(format!("--amount 金额不合法: {e}")))?;

    // 不给路径 = 产品默认账本(与 `wanning init` 生成的宿主配置同一本账);
    // 家目录解析不出 = fail-closed,绝不猜一个落点。
    let wal = match wal {
        Some(wal) => wal,
        None => wanning_core::paths::default_wal_path().ok_or_else(|| {
            CmdError::Failed(
                "解析不出默认账本路径(WANNING_HOME / USERPROFILE / HOME 都没有)。\
                 用 `wanning confirm <单号> --amount <元> --proof <凭证> --wal <账本>` 显式给一个"
                    .to_string(),
            )
        })?,
    };
    if !wal.exists() {
        return Err(CmdError::Failed(format!(
            "审计账本不存在:{}(这张待支付单是哪个闸开的,就确认哪本账)",
            slash(&wal)
        )));
    }

    // live_resuming:先整链回放对账,再接旧账续写(确认行 + 终态行)。
    // 闸正被宿主进程占着(单写者锁)时这里拿不到锁,fail-closed 报给人工。
    let mut state = wanning_core::state::WanningState::live_resuming(&wal)
        .map_err(|e| CmdError::Failed(format!("账本打开失败(fail-closed): {e}")))?;
    let order = state
        .confirm_pending(&pending_id, amount_cents, proof.trim())
        .map_err(|e| CmdError::Failed(format!("确认被拒(fail-closed,一行未落账): {e}")))?;

    println!("待支付单已确认:{pending_id}");
    println!(
        "  金额: ¥{}(确认额 = 审批额,分文不差)",
        wanning_demo::alipay::cents_to_yuan_amount(order.approved_amount_cents)
    );
    println!("  支付凭证: {}", order.proof.as_deref().unwrap_or("-"));
    println!("  状态: {:?}(确认行 + 终态行已落账)", order.state);
    println!(
        "  账本:{}(完整性链逐行验证通过;逐段回放:`wanning audit {} --out <report.html>`)",
        slash(&wal),
        slash(&wal)
    );
    Ok(())
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
