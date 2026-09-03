//! Wanning 性能基准(W-30):零依赖手写,不引 criterion。
//!
//! 五项基准,口径全部写死在本模块,数字由 `cargo run -p wanning-bench --release`
//! 真实跑出落 `docs/benchmarks.md`(含机器环境与复现步骤):
//!
//! 1. [`gate_decide_allow`]——闸判定吞吐(Allow 口径:nonce 单调、金额在预算内);
//! 2. [`gate_decide_deny_over_budget`]——闸判定吞吐(Deny 口径:金额恒超预算,
//!    走到预算门被拒,steady state);
//! 3. [`wal_append`]——WAL 追加吞吐(带 flush,每行 = 序列化 + 完整性链 + 写盘);
//! 4. [`wal_replay`]——同一份账本回放吞吐(含完整性链逐行验证 + 状态重建 + 回放对账);
//! 5. [`audit_html_export_5k`]——W-22 审计回放页导出耗时(默认 5k 行账)。
//!
//! 造数沿用 W-04 的 xorshift64*(固定种子,金额可变但可复现),零随机依赖。
//! 每项 = 1 轮预热(不计入)+ `rounds` 轮实测;上报每轮数值 + 中位数,
//! 「跑几次取稳态」的轮数口径见 [`ROUNDS`]。
//!
//! 测试(`tests/bench.rs`)只用小参数锁「可运行、口径正确、数值合法」;
//! 绝不把测试里的小跑数字当基准写档。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use wanning_core::clock::MockClock;
use wanning_core::delegation::Delegation;
use wanning_core::error::CoreError;
use wanning_core::gate::Gate;
use wanning_core::intent::SpendIntent;
use wanning_core::state::WanningState;
use wanning_core::wal::{Wal, WalDecision, WalRecord};

/// 闸判定基准:每轮判定笔数。
pub const GATE_DECIDE_OPS: usize = 200_000;
/// WAL 追加基准:每轮追加行数。
pub const WAL_APPEND_LINES: usize = 20_000;
/// WAL 回放基准:回放账本行数。
pub const WAL_REPLAY_LINES: usize = 50_000;
/// 审计页导出基准:被导出账本行数(任务书口径 = 5k 行)。
pub const AUDIT_HTML_LINES: usize = 5_000;
/// 每项基准实测轮数(另含 1 轮预热,不计入)。
pub const ROUNDS: usize = 5;

/// 基准委托的预算上限(1 亿元 = 足够任何一轮 Allow 口径不触顶)。
const BENCH_CAP_CENTS: u64 = 1_000_000_000;
/// 固定 Unix 起点(与 demo 场景同一时刻,可复现)。
const BENCH_TS: u64 = 1_700_000_000;
/// xorshift64* 种子(固定;与 W-04 property 测试同族的可复现口径)。
const BENCH_SEED: u64 = 0x57C0_4E1C_9E37_79B9;

/// 全部基准的规模参数(默认口径见各常量;测试用小参数跑同一套函数)。
#[derive(Debug, Clone, Copy)]
pub struct Sizing {
    pub gate_decide_ops: usize,
    pub wal_append_lines: usize,
    pub wal_replay_lines: usize,
    pub audit_html_lines: usize,
    pub rounds: usize,
}

impl Default for Sizing {
    fn default() -> Self {
        Self {
            gate_decide_ops: GATE_DECIDE_OPS,
            wal_append_lines: WAL_APPEND_LINES,
            wal_replay_lines: WAL_REPLAY_LINES,
            audit_html_lines: AUDIT_HTML_LINES,
            rounds: ROUNDS,
        }
    }
}

/// 一项基准的实测结果:每轮数值 + 中位数。单位见 [`BenchStats::unit`]。
#[derive(Debug, Clone)]
pub struct BenchStats {
    /// 基准名(报告/文档引用的稳定标识)。
    pub label: &'static str,
    /// 数值单位:"判定/s" / "行/s" / "ms"。
    pub unit: &'static str,
    /// 每轮操作数(判定笔数 / 追加行数 / 回放行数 / 被导出账本行数)。
    pub ops: u64,
    /// 每轮数值(吞吐为 ops/s;审计页导出为毫秒)。
    pub rounds: Vec<f64>,
}

impl BenchStats {
    /// 中位数(偶数轮取中间两轮均值)——「取稳态」的口径。
    pub fn median(&self) -> f64 {
        let mut sorted = self.rounds.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).expect("基准数值不含 NaN"));
        match sorted.len() {
            0 => 0.0,
            1 => sorted[0],
            n if n % 2 == 0 => (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0,
            _ => sorted[self.rounds.len() / 2],
        }
    }

    /// 最差轮(吞吐取最小 = 最保守口径)。
    pub fn min(&self) -> f64 {
        self.rounds.iter().copied().fold(f64::INFINITY, f64::min)
    }
}

/// 跑全部五项基准(顺序固定,报告与文档对齐)。
pub fn run_all(sizing: &Sizing) -> Vec<BenchStats> {
    vec![
        gate_decide_allow(sizing.gate_decide_ops, sizing.rounds)
            .expect("闸判定基准(Allow)必须可跑:纯内存,无外部依赖"),
        gate_decide_deny_over_budget(sizing.gate_decide_ops, sizing.rounds)
            .expect("闸判定基准(Deny)必须可跑:纯内存,无外部依赖"),
        wal_append(sizing.wal_append_lines, sizing.rounds)
            .expect("WAL 追加基准必须可跑:临时目录可写"),
        wal_replay(sizing.wal_replay_lines, sizing.rounds)
            .expect("WAL 回放基准必须可跑:临时目录可写"),
        audit_html_export(sizing.audit_html_lines, sizing.rounds)
            .expect("审计页导出基准必须可跑:临时目录可写"),
    ]
}

// ---------------------------------------------------------------------------
// ①② 闸判定(纯内存,不含 WAL)
// ---------------------------------------------------------------------------

/// 闸判定吞吐(Allow 口径):nonce 单调递增、金额 100..499 分在预算内,
/// 每笔都走完六道门并扣减预算。
pub fn gate_decide_allow(ops: usize, rounds: usize) -> Result<BenchStats, CoreError> {
    bench_gate(true, ops, rounds)
}

/// 闸判定吞吐(Deny 口径):金额恒为「上限+1」分,每笔都走完前面的门后在预算门被拒
/// (steady state 的拒绝路径;拒绝不耗号也不动账本)。
pub fn gate_decide_deny_over_budget(ops: usize, rounds: usize) -> Result<BenchStats, CoreError> {
    bench_gate(false, ops, rounds)
}

fn bench_gate(allow: bool, ops: usize, rounds: usize) -> Result<BenchStats, CoreError> {
    let label = if allow {
        "gate_decide_allow"
    } else {
        "gate_decide_deny_over_budget"
    };
    let mut round_stats = Vec::with_capacity(rounds);
    for _round in 0..rounds {
        let clock = MockClock::new(BENCH_TS);
        let mut gate = Gate::new(Arc::new(clock.clone()));
        gate.register_delegation(bench_delegation())?;

        // 预热(不计入):100 笔;nonce 与正式轮错开,Allow 口径绝不撞 replay。
        let warmup_nonces = 100u64;
        for nonce in 1..=warmup_nonces {
            let _ = gate.decide(&bench_intent(nonce, allow));
        }

        let start = Instant::now();
        for offset in 0..ops as u64 {
            let _ = gate.decide(&bench_intent(warmup_nonces + 1 + offset, allow));
        }
        let elapsed = start.elapsed().as_secs_f64();
        round_stats.push(ops as f64 / elapsed);
    }
    Ok(BenchStats {
        label,
        unit: "判定/s",
        ops: ops as u64,
        rounds: round_stats,
    })
}

fn bench_delegation() -> Delegation {
    Delegation::new(
        "d1",
        "所有者",
        "bench-agent",
        BENCH_CAP_CENTS,
        BENCH_TS,
        BENCH_TS + 3_600,
        "agent:bench",
    )
}

/// 基准意图:Allow 口径金额 = 100..499 分(xorshift 造数,固定种子可复现);
/// Deny 口径金额 = 上限+1(必然 OverBudget)。
fn bench_intent(nonce: u64, allow: bool) -> SpendIntent {
    let amount = if allow {
        100 + (Rng::new(BENCH_SEED ^ nonce).below(400))
    } else {
        BENCH_CAP_CENTS + 1
    };
    SpendIntent::new(
        "d1",
        nonce,
        amount,
        "jd:bench-shop",
        "grocery",
        "wanning-bench",
    )
}

// ---------------------------------------------------------------------------
// ③ WAL 追加(带 flush)
// ---------------------------------------------------------------------------

/// WAL 追加吞吐:每行 = 记录序列化 + 完整性链 + 写盘 + flush。
/// 每轮新开一份 WAL(首行先落 RegisterDelegation,文件保持可回放);计时只含追加循环。
pub fn wal_append(lines: usize, rounds: usize) -> Result<BenchStats, CoreError> {
    let mut round_stats = Vec::with_capacity(rounds);
    for _round in 0..rounds {
        let wal_path = bench_wal_path("append");
        let mut wal = Wal::open(&wal_path)?;
        wal.append(&WalRecord::RegisterDelegation {
            ts: BENCH_TS,
            delegation: bench_delegation(),
        })?;
        // 预热(不计入):同一文件先追加 100 行,把文件系统缓存与序列化路径热起来。
        for nonce in 1..=100u64 {
            wal.append(&bench_decide_record(nonce))?;
        }

        let start = Instant::now();
        for offset in 0..lines as u64 {
            wal.append(&bench_decide_record(100 + 1 + offset))?;
        }
        let elapsed = start.elapsed().as_secs_f64();
        round_stats.push(lines as f64 / elapsed);

        drop(wal); // 释放单写者锁,再清理临时文件
        cleanup_wal(&wal_path);
    }
    Ok(BenchStats {
        label: "wal_append",
        unit: "行/s",
        ops: lines as u64,
        rounds: round_stats,
    })
}

/// 一条放行判定记录(金额与 budget_after 随 nonce 递增,内容真实可回放)。
fn bench_decide_record(nonce: u64) -> WalRecord {
    let amount = 100 + (Rng::new(BENCH_SEED ^ nonce).below(400));
    WalRecord::Decide {
        ts: BENCH_TS,
        decision: WalDecision::Allow,
        delegation_id: "d1".to_string(),
        intent: SpendIntent::new(
            "d1",
            nonce,
            amount,
            "jd:bench-shop",
            "grocery",
            "wanning-bench",
        ),
        reason: None,
        budget_after_cents: amount * nonce,
    }
}

// ---------------------------------------------------------------------------
// ④ WAL 回放 + ⑤ 审计页导出(共用一份生成好的账本)
// ---------------------------------------------------------------------------

/// 同一份账本的回放吞吐:读侧逐行验完整性链 + 状态重建 + 回放对账(全部计入——
/// 这才是「重启一次要多久」的真实口径)。
pub fn wal_replay(lines: usize, rounds: usize) -> Result<BenchStats, CoreError> {
    let wal_path = generate_wal(lines, "replay-src")?;
    let mut round_stats = Vec::with_capacity(rounds);
    for _round in 0..rounds {
        let start = Instant::now();
        let state = WanningState::replay(&wal_path)?;
        let elapsed = start.elapsed().as_secs_f64();
        // 回放态不持 WAL(只读重建),行数用读侧验链口径独立核对(计时外,不掺进数字)。
        debug_assert_eq!(
            wanning_core::wal::read_verified(&wal_path)?.records.len(),
            lines + 1,
            "账本行数 = 注册 1 行 + 放行 lines 行"
        );
        debug_assert!(state.gate().delegation("d1").is_some(), "回放后委托在闸内");
        round_stats.push(lines as f64 / elapsed);
    }
    cleanup_wal(&wal_path);
    Ok(BenchStats {
        label: "wal_replay",
        unit: "行/s",
        ops: lines as u64,
        rounds: round_stats,
    })
}

/// W-22 审计回放页导出耗时(验链 → 回放对账 → 渲染 → 原子落盘,全部计入;
/// 默认口径 = 5k 行账,任务书指定)。上报单位 = 毫秒/次。
pub fn audit_html_export(lines: usize, rounds: usize) -> Result<BenchStats, CoreError> {
    let wal_path = generate_wal(lines, "audit-html-src")?;
    let out_path = wal_path.with_extension("html");
    let mut round_stats = Vec::with_capacity(rounds);
    for _round in 0..rounds {
        let start = Instant::now();
        wanning_demo::audit_html::export_audit(&wal_path, &out_path, None)?;
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        round_stats.push(elapsed_ms);
        let _ = std::fs::remove_file(&out_path);
    }
    cleanup_wal(&wal_path);
    Ok(BenchStats {
        label: "audit_html_export_5k",
        unit: "ms",
        ops: lines as u64,
        rounds: round_stats,
    })
}

/// 生成一份全部放行的账本(WanningState 真实写路径,行内容可回放对账)。
/// 金额 100..499 分,上限 1 亿元 → 任何行数下每笔都放行,回放重算逐行一致。
fn generate_wal(lines: usize, tag: &str) -> Result<PathBuf, CoreError> {
    let wal_path = bench_wal_path(tag);
    let clock = MockClock::new(BENCH_TS);
    let mut state = WanningState::with_wal(Arc::new(clock.clone()), &wal_path)?;
    state.register_delegation(bench_delegation())?;
    for offset in 0..lines as u64 {
        let nonce = offset + 1;
        state.decide(&bench_intent(nonce, true))?;
    }
    Ok(wal_path)
}

// ---------------------------------------------------------------------------
// 造数与临时文件
// ---------------------------------------------------------------------------

/// xorshift64*(与 W-04 property 测试同一实现口径;固定种子 → 造数可复现)。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// [0, n) 内的均匀值;n == 0 时返回 0。
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }
}

/// 基准临时 WAL 路径:进程内原子序号 + 纳秒 + pid(W-21 教训:同 tick 撞名
/// 会抢同一把单写者锁)。
fn bench_wal_path(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join("wanning-bench");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let unix_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间早于 Unix 纪元")
        .as_nanos();
    dir.join(format!(
        "{tag}-{unix_nanos}-{}-{seq}.jsonl",
        std::process::id()
    ))
}

/// 尽力清理基准产物(临时目录,失败不影响结果)。
fn cleanup_wal(wal_path: &std::path::Path) {
    let _ = std::fs::remove_file(wal_path);
    let _ = std::fs::remove_file(wanning_core::wal::single_writer_lock_path(wal_path));
}
