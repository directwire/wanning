//! wanning-bench 入口:`cargo run -p wanning-bench --release` 跑全部五项基准,
//! 打印每轮数值 + 中位数;真实数字落 `docs/benchmarks.md`(含机器环境与复现步骤)。
//!
//! 刻意只做薄打印:口径与实现全在 [`wanning_bench`] 库面,测试可复用同一套函数。

use std::process::ExitCode;

use wanning_bench::{run_all, Sizing, ROUNDS};

fn main() -> ExitCode {
    println!("== wanning-bench · W-30 性能基准(零依赖手写,不引 criterion)==");
    println!("口径:每项 {ROUNDS} 轮实测(另含 1 轮预热不计入)+ 中位数;release profile;");
    println!("     判定/追加/回放单位为每秒操作数,审计页导出单位为每次导出毫秒。");

    let sizing = Sizing::default();
    let reports = run_all(&sizing);

    for stats in &reports {
        let per_round = stats
            .rounds
            .iter()
            .map(|v| format_value(stats.unit, *v))
            .collect::<Vec<_>>()
            .join(" | ");
        println!(
            "{:<24} {:>10} {} × {} 轮:[{}] → 中位 {}",
            stats.label,
            stats.ops,
            unit_suffix(stats.unit),
            stats.rounds.len(),
            per_round,
            format_value(stats.unit, stats.median()),
        );
    }

    println!("复现:cargo run -p wanning-bench --release;口径与机器环境见 docs/benchmarks.md。");
    ExitCode::SUCCESS
}

/// 数值格式化:吞吐加下划线千分位,毫秒保留一位小数。
fn format_value(unit: &str, value: f64) -> String {
    if unit == "ms" {
        format!("{value:.1} ms")
    } else {
        format!("{}/s", thousands(value))
    }
}

fn unit_suffix(unit: &str) -> &'static str {
    if unit == "ms" {
        "行账"
    } else {
        "op"
    }
}

fn thousands(value: f64) -> String {
    let digits = (value as u64).to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in digits.chars().enumerate() {
        // len%3 记住首位组的大小:len=7 → "3_241_000",len=6 → "324_100"。
        if i > 0 && i % 3 == len % 3 {
            out.push('_');
        }
        out.push(ch);
    }
    out
}
