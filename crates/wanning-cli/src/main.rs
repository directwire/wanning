//! 统一 CLI 入口 `wanning`(W-43a 产品化):init / audit / demo / anchor-verify。
//!
//! 全部逻辑在 [`wanning_cli::run_cli`](lib),本文件只是薄壳;退出码纪律
//! (0 成功 / 2 用法错 / 1 运行失败)也在 lib,保证与库面测试口径一致。

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    wanning_cli::run_cli(&args)
}
