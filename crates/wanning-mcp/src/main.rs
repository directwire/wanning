//! wanning-mcp bin:stdio 上的 MCP server(每行一条 JSON-RPC 消息)。
//!
//! stdout 是**协议通道**:除 JSON-RPC 响应外一行都不能多打(欢迎语/日志全部走 stderr);
//! EOF(client 关闭输入)即正常退出,对齐 spec 的 stdio shutdown 约定。

use std::io::BufRead;
use std::process::ExitCode;

use wanning_mcp::McpServer;

const USAGE: &str = "wanning-mcp:Wanning 支付闸的 MCP server(stdio,零网络、零真实消费)

用法: wanning-mcp --wal <路径> [--cap-cents <分>] [--hours <小时>]

  --wal <路径>     审计 WAL 文件(append-only JSONL)。**必填**:没有审计的闸不服务(fail-closed)
  --cap-cents <分> 演示委托的总预算,单位分,默认 1000(¥10.00)
  --hours <小时>   演示委托的有效时长,默认 24(从启动时刻起)
  -h / --help      打印本说明后退出
";

struct Config {
    wal: String,
    cap_cents: u64,
    hours: u64,
}

fn parse_args(args: &[String]) -> Result<Option<Config>, String> {
    let mut wal: Option<String> = None;
    let mut cap_cents = wanning_mcp::DEFAULT_CAP_CENTS;
    let mut hours = wanning_mcp::DEFAULT_HOURS;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(None),
            "--wal" => {
                wal = Some(next_value(args, &mut index, "--wal")?);
            }
            "--cap-cents" => {
                let raw = next_value(args, &mut index, "--cap-cents")?;
                cap_cents = raw
                    .parse()
                    .map_err(|_| format!("--cap-cents 必须是非负整数(分),收到: {raw}"))?;
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
        cap_cents,
        hours,
    }))
}

fn next_value(args: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| format!("{flag} 缺少取值(用 --help 看用法)"))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
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

    let mut server = match McpServer::new_with(&config.wal, config.cap_cents, config.hours) {
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
