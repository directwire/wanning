//! Wanning P0 CLI demo(W-43a 起是统一入口 `wanning demo` 之外的旧 bin 名)。
//!
//! 全部 CLI 逻辑在 [`wanning_demo::cli`](lib),本文件只是薄壳——保证
//! `wanning-demo` 与统一入口 `wanning demo` 的用法说明、报错口径、护栏行为
//! 永远一致,不会漂移成两套行为。旧名保留一个发行周期作 alias。

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match wanning_demo::cli::run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("wanning-demo: {message}");
            ExitCode::FAILURE
        }
    }
}
