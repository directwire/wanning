//! wanning-init bin:把 [`wanning_init`] 的生成物打到终端或文件。
//!
//! 写文件纪律(本任务核心):默认**只打印 stdout**;`--out` 显式给路径才写;
//! 已存在文件**绝不覆盖**(create_new + 预检,双重拒)——动别人工具的配置 =
//! 危险动作,拒绝时生成内容已在 stdout 上,可自行粘贴。

use std::fs::OpenOptions;
use std::io::Write;
use std::process::ExitCode;

use wanning_init::{generate, parse_platform};

const USAGE: &str = "wanning-init:给编码工具吐 Wanning MCP 配置(零网络、零真实消费)

用法: wanning-init --platform <名> [--out <路径>]

  --platform <名>  claude-code | codex | kimi | trae | workbuddy
  --out <路径>     写入文件;不给则只打印 stdout。已存在文件绝不覆盖
  -h / --help      打印本说明后退出

生成内容带 WAL 路径占位符或平台路径变量,注释按各工具语法(TOML/shell 用 #,
严格 JSON 无注释 → 说明打在 stdout)。
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let mut platform: Option<String> = None;
    let mut out: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--platform" => match next_value(&args, &mut index, "--platform") {
                Some(v) => platform = Some(v),
                None => return usage_fail("--platform 需要一个值"),
            },
            "--out" => match next_value(&args, &mut index, "--out") {
                Some(v) => out = Some(v),
                None => return usage_fail("--out 需要一个路径"),
            },
            other => return usage_fail(&format!("未知参数 '{other}'")),
        }
    }

    let platform_name = match platform {
        Some(p) => p,
        None => return usage_fail("缺 --platform"),
    };
    let platform = match parse_platform(&platform_name) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("{}", err.message());
            return ExitCode::FAILURE;
        }
    };

    let artifact = generate(platform);
    for note in &artifact.notes {
        println!("# {note}");
    }
    println!();
    println!("{}", artifact.content);

    if let Some(path) = out {
        let path = std::path::PathBuf::from(path);
        if path.exists() {
            eprintln!(
                "拒绝覆盖:{} 已存在。动别人工具的配置 = 危险动作;\
                 生成内容已在上面 stdout,可自行粘贴,或换 --out 路径。",
                path.display()
            );
            return ExitCode::FAILURE;
        }
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(err) => {
                eprintln!("写入失败 {}: {err}", path.display());
                return ExitCode::FAILURE;
            }
        };
        if let Err(err) = file.write_all(artifact.content.as_bytes()) {
            eprintln!("写入失败 {}: {err}", path.display());
            return ExitCode::FAILURE;
        }
        eprintln!("已写入 {}(create_new,不覆盖既有文件)", path.display());
    }

    ExitCode::SUCCESS
}

fn next_value(args: &[String], index: &mut usize, flag: &str) -> Option<String> {
    *index += 1;
    if *index >= args.len() {
        eprintln!("{flag} 缺少值");
        return None;
    }
    let value = args[*index].clone();
    *index += 1;
    Some(value)
}

fn usage_fail(reason: &str) -> ExitCode {
    eprintln!("{reason}\n{USAGE}");
    ExitCode::FAILURE
}
