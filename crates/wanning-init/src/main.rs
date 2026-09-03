//! wanning-init bin:统一入口 `wanning init`(W-43a)出现前的旧 bin 名,一个发行
//! 周期内保留为别名。全部逻辑在 [`wanning_init::run_cli`](lib),本文件只是薄壳——
//! 保证两个入口的用法说明、报错口径、退出码纪律(0 成功 / 2 用法错 / 1 运行失败)
//! 永远一致,不会漂移成两套行为。

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    wanning_init::run_cli("wanning-init", &args)
}
