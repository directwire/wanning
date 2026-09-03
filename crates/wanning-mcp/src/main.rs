//! wanning-mcp bin:stdio 上的 MCP server(每行一条 JSON-RPC 消息)。
//!
//! stdout 是**协议通道**:除 JSON-RPC 响应外一行都不能多打(欢迎语/日志全部走 stderr);
//! EOF(client 关闭输入)即正常退出,对齐 spec 的 stdio shutdown 约定。

use std::io::BufRead;
use std::process::ExitCode;

use wanning_mcp::McpServer;

const USAGE: &str = "wanning-mcp:Wanning 支付闸的 MCP server(stdio,零网络、零真实消费)

用法: wanning-mcp --wal <路径> [--budget <分>] [--max-spends <笔数>] [--hours <小时>]
                 [--pay-mode <pending_pay|auto_debit|manual>] [--pending-ttl-secs <秒>]

  --wal <路径>      审计 WAL 文件(append-only JSONL)。**必填**:没有审计的闸不服务
                    (fail-closed)。产品默认账本位置是 ~/.wanning/wal.jsonl;
                    `wanning init` 生成配置时会把写实路径直接写进配置。
  --budget <分>     演示委托的总预算,单位分,默认 1000(¥10.00)。产品主别名。
  --cap-cents <分>  旧名,与 --budget 同义(两个同时给 = 拒,两义性 fail-closed)。
  --max-spends <n>  速率护栏:滑动窗内至多 n 笔成功放行,默认 10;0 = 关掉护栏。
  --hours <小时>    演示委托的有效时长,默认 24(从启动时刻起)。
  --pay-mode <档>   支付形态(W-53):pending_pay = 人在环待支付(默认;闸放行即开
                    待支付单,人用 `wanning confirm` 按指纹确认);auto_debit = 免密
                    代扣(平台侧第二形式;这里只改账本语义——放行即落地,不接任何
                    通道);manual = 纯闸(只判定不开单)。**任何档位下确认都不在
                    工具面上**:AI 不能确认 AI 自己的支付。
  --pending-ttl-secs <秒>
                    待支付单的有效窗口(半开 [开单, 过期)),默认 900 秒(15 分钟);
                    pending_pay 档位下 0 = 拒启(开出来就死的单)。
  -V / --version    打印版本后退出(wanning doctor 的 ①/⑥ 检查靠它读版本)
  -h / --help       打印本说明后退出
";

struct Config {
    wal: String,
    cap_cents: u64,
    hours: u64,
    max_spends: u32,
    pay_mode: wanning_mcp::PayMode,
    pending_ttl_secs: u64,
}

fn parse_args(args: &[String]) -> Result<Option<Config>, String> {
    let mut wal: Option<String> = None;
    let mut budget: Option<u64> = None;
    let mut hours = wanning_mcp::DEFAULT_HOURS;
    let mut max_spends = wanning_mcp::DEFAULT_MAX_SPENDS_PER_DAY;
    let mut pay_mode = wanning_mcp::PayMode::default();
    let mut pending_ttl_secs = wanning_mcp::DEFAULT_PENDING_TTL_SECS;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(None),
            "--wal" => {
                wal = Some(next_value(args, &mut index, "--wal")?);
            }
            // --budget 是产品主别名(W-43a),--cap-cents 是旧名(兼容一个发行周期)。
            "--budget" | "--cap-cents" => {
                let flag = args[index].as_str();
                let raw = next_value(args, &mut index, flag)?;
                let parsed: u64 = raw
                    .parse()
                    .map_err(|_| format!("{flag} 必须是非负整数(分),收到: {raw}"))?;
                if parsed == 0 {
                    return Err(format!(
                        "{flag} 必须为正(0 分预算的委托一注册就没有可花额度,属配置错误)"
                    ));
                }
                if budget.is_some() {
                    return Err(
                        "--budget 与 --cap-cents 是同义别名,同时出现即拒(两义性 fail-closed)"
                            .to_string(),
                    );
                }
                budget = Some(parsed);
            }
            "--max-spends" => {
                let raw = next_value(args, &mut index, "--max-spends")?;
                max_spends = raw
                    .parse()
                    .map_err(|_| format!("--max-spends 必须是非负整数(笔数),收到: {raw}"))?;
            }
            "--hours" => {
                let raw = next_value(args, &mut index, "--hours")?;
                hours = raw
                    .parse()
                    .map_err(|_| format!("--hours 必须是非负整数(小时),收到: {raw}"))?;
                if hours == 0 {
                    return Err("--hours 必须为正(0 小时的授权一启动就过期)".to_string());
                }
            }
            "--pay-mode" => {
                let raw = next_value(args, &mut index, "--pay-mode")?;
                pay_mode = parse_pay_mode(&raw)?;
            }
            "--pending-ttl-secs" => {
                let raw = next_value(args, &mut index, "--pending-ttl-secs")?;
                pending_ttl_secs = raw
                    .parse()
                    .map_err(|_| format!("--pending-ttl-secs 必须是非负整数(秒),收到: {raw}"))?;
            }
            other => return Err(format!("未知参数: {other}(用 --help 看用法)")),
        }
        index += 1;
    }
    let Some(wal) = wal else {
        return Err(
            "启动被拒(fail-closed):必须提供 --wal <路径>——闸的一切判定都要落审计,\
             没有审计日志的闸不服务任何请求"
                .to_string(),
        );
    };
    Ok(Some(Config {
        wal,
        cap_cents: budget.unwrap_or(wanning_mcp::DEFAULT_CAP_CENTS),
        hours,
        max_spends,
        pay_mode,
        pending_ttl_secs,
    }))
}

/// 档位字面量 → PayMode(与 core 的 serde snake_case 落盘形状逐字一致)。
fn parse_pay_mode(raw: &str) -> Result<wanning_mcp::PayMode, String> {
    match raw {
        "pending_pay" => Ok(wanning_mcp::PayMode::PendingPay),
        "auto_debit" => Ok(wanning_mcp::PayMode::AutoDebit),
        "manual" => Ok(wanning_mcp::PayMode::Manual),
        other => Err(format!(
            "--pay-mode 只认 pending_pay / auto_debit / manual,收到: {other}"
        )),
    }
}

fn next_value(args: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| format!("{flag} 缺少取值(用 --help 看用法)"))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // --version 短路在 parse_args 之前:版本探测不能要求 --wal(W-51b doctor 的
    // ①/⑥ 只问「你是谁、哪个版本」,不该为了问版本就要一份账本)。
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("wanning-mcp {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let config = match parse_args(&args) {
        Ok(Some(config)) => config,
        Ok(None) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("wanning-mcp: {message}");
            return ExitCode::from(2);
        }
    };

    let mut server = match McpServer::new_full(
        &config.wal,
        config.cap_cents,
        config.hours,
        config.max_spends,
        config.pay_mode,
        config.pending_ttl_secs,
    ) {
        Ok(server) => server,
        Err(e) => {
            eprintln!("wanning-mcp 启动失败(fail-closed): {e}");
            return ExitCode::FAILURE;
        }
    };

    // spec(stdio shutdown):客户端关闭输入流 → server 退出。
    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        line.clear();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        // stdout 只允许协议消息;通知(无响应)不打任何东西。
        if let Some(response) = server.handle_line(&line) {
            println!("{response}");
        }
    }
    ExitCode::SUCCESS
}
