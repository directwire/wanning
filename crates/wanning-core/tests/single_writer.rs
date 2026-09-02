//! 单写者不变量:**同一份 WAL 同时至多允许一个活着的写进程**(fail-closed)。
//!
//! 为什么必须有这条:闸的预算账本、nonce 登记、撤销集合都在**内存**里,WAL 只在
//! 启动时回放一次。两个进程同时拿着同一份 WAL 各判各的 = 每边只知道自己花了多少
//! → 预算硬上限失效、同一 nonce 跨进程放行两次、审计行交错。
//!
//! 这不是假想场景:`.mcp.json` 与 `.trae/mcp.json` 指向**同一份**默认 WAL,
//! 老板把 Claude Code 和 Trae 同时挂在仓库上,两个 MCP server 就是并发双闸。
//! 见 master-plan 决策记录 2026-09-02(W-18)。
//!
//! 语义边界:锁只挡**写进程**,不挡读者——回放/审计读取在服务运行期间必须可用。

use std::path::Path;
use std::sync::Arc;

use wanning_core::clock::{Clock, MockClock, SystemClock};
use wanning_core::delegation::Delegation;
use wanning_core::error::CoreError;
use wanning_core::state::WanningState;
use wanning_core::wal::{raw_lines, single_writer_lock_path};

fn tmp_wal(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("wanning-single-writer-tests");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir.join(format!("{tag}-{}.jsonl", std::process::id()))
}

/// 续跑测试专用:委托窗口必须同时覆盖「回放世界」与「系统时钟的现在」。
fn long_lived_delegation() -> Delegation {
    let now = SystemClock.now();
    Delegation::new(
        "d1",
        "老板",
        "agent",
        1000,
        now,
        now.checked_add(86_400).expect("有效期溢出"),
        "agent:agent",
    )
}

fn intent(nonce: u64, amount_cents: u64) -> wanning_core::intent::SpendIntent {
    wanning_core::intent::SpendIntent::new(
        "d1",
        nonce,
        amount_cents,
        "jd:shop-1",
        "grocery",
        "测试",
    )
}

/// 核心:第二个写进程必须被拒启。修复前这里会走到 `Ok` 分支——那个 panic 信息
/// 就是漏洞现场:两进程都从空账出发,各自注册、各判各的,合计消费越过预算上限。
///
/// (注意:晚启动的进程会回放看到旧账,所以「先后启动」并不破防;
/// 真正的洞是**两个都已活着的进程**——各自内存账本只知道自己花了几笔。)
#[test]
fn second_live_process_on_same_wal_is_refused() {
    let path = tmp_wal("exclusive");
    let _ = std::fs::remove_file(&path);

    let mut first = WanningState::live_resuming(&path).expect("第一个进程开张");

    match WanningState::live_resuming(&path) {
        Err(CoreError::WalLocked {
            path: lock,
            message,
        }) => {
            assert!(
                lock.to_string().ends_with(".lock"),
                "锁文件路径要指名: {lock}"
            );
            assert!(
                message.contains("删除"),
                "错误信息必须给出恢复动作: {message}"
            );
        }
        Ok(mut twin) => {
            // 漏洞现场(仅修复前可达):两进程都以为账本为空,各自注册、各判各的。
            twin.register_delegation(long_lived_delegation())
                .expect("第二进程注册");
            first
                .register_delegation(long_lived_delegation())
                .expect("第一进程注册");
            let a = first.decide(&intent(1, 700)).expect("A 判定");
            let b = twin.decide(&intent(2, 700)).expect("B 判定");
            panic!(
                "第二个写进程未被拒启(并发双闸):A 判 {a:?} / B 判 {b:?} —— \
                 两进程合计可花 1400 分,而委托 cap 只有 1000 分,预算硬上限失效;\
                 且 WAL 里出现两行同一 id 的注册,下次回放对账必炸"
            );
        }
        Err(other) => panic!("应报 WalLocked,实际 {other:?}"),
    }

    // 第一个进程不受影响,照常注册、判定、落审计。
    first
        .register_delegation(long_lived_delegation())
        .expect("注册");
    assert!(
        first.decide(&intent(1, 400)).expect("判定").is_allow(),
        "第一笔正常放行"
    );
    drop(first);
    let _ = std::fs::remove_file(&path);
}

/// 锁随 Drop 释放:正常退出后,下一个进程能接续旧账(W-17 语义不变)。
#[test]
fn lock_released_on_drop_and_next_startup_resumes() {
    let path = tmp_wal("release");
    let _ = std::fs::remove_file(&path);
    let lock_path = single_writer_lock_path(&path);

    {
        let mut a = WanningState::live_resuming(&path).expect("开张");
        a.register_delegation(long_lived_delegation())
            .expect("注册");
        assert!(a.decide(&intent(1, 600)).expect("判定").is_allow());
        assert!(lock_path.exists(), "持锁期间锁文件必须存在: {lock_path:?}");
    }

    assert!(
        !lock_path.exists(),
        "Drop 必须删锁,否则一次崩溃就把 WAL 永久锁死"
    );
    let b = WanningState::live_resuming(&path).expect("释放后可续跑");
    assert_eq!(
        b.gate().spent_cents("d1"),
        Some(600),
        "接续旧账语义不变(W-17)"
    );
    drop(b);
    let _ = std::fs::remove_file(&path);
}

/// 孤儿锁(持锁进程被 kill -9):拒启 + 错误信息可排查,WAL 与别人的锁都不动。
#[test]
fn stale_lock_blocks_startup_without_touching_wal_or_foreign_lock() {
    let path = tmp_wal("stale");
    let _ = std::fs::remove_file(&path);
    {
        let mut a = WanningState::live_resuming(&path).expect("开张");
        a.register_delegation(long_lived_delegation())
            .expect("注册");
    } // 正常退出,锁已删

    let lock_path = single_writer_lock_path(&path);
    std::fs::write(&lock_path, format!("pid=999999\nwal={}\n", path.display()))
        .expect("人为放置孤儿锁(模拟持锁进程崩溃)");

    match WanningState::live_resuming(&path) {
        Err(CoreError::WalLocked {
            path: lock,
            message,
        }) => {
            assert_eq!(lock, lock_path.to_string_lossy());
            assert!(
                message.contains("999999"),
                "错误信息要能指认持锁进程: {message}"
            );
            assert!(
                message.contains("删除"),
                "错误信息必须给出恢复动作: {message}"
            );
        }
        other => panic!("孤儿锁必须 fail-closed 拒启,实际 {other:?}"),
    }
    assert_eq!(
        raw_lines(&path).expect("读 WAL").len(),
        1,
        "被拒进程不得动 WAL(一行 = 注册)"
    );
    assert!(lock_path.exists(), "拒启方不得删除别人的锁");
    std::fs::remove_file(&lock_path).expect("清理孤儿锁");
    let _ = std::fs::remove_file(&path);
}

/// 构造路径同样受锁:两个 `with_wal` 状态共写同一份 WAL 一样要被拒。
#[test]
fn second_with_wal_state_on_same_path_is_refused() {
    let path = tmp_wal("with-wal");
    let _ = std::fs::remove_file(&path);
    let clock = MockClock::new(1500);

    let first = WanningState::with_wal(Arc::new(clock.clone()), &path).expect("第一个状态");
    let second = WanningState::with_wal(Arc::new(clock.clone()), &path);
    assert!(
        matches!(second, Err(CoreError::WalLocked { .. })),
        "第二个写状态必须被拒: {second:?}"
    );
    drop(first);

    let reopened = WanningState::with_wal(Arc::new(clock.clone()), &path).expect("释放后可重开");
    assert_eq!(
        reopened.wal_line_count(),
        Some(0),
        "重开不得截断,也必须是同一份空账"
    );
    drop(reopened);
    let _ = std::fs::remove_file(&path);
}

/// 锁只挡写进程,不挡读者:持锁期间回放(审计对账)照常可用。
#[test]
fn replay_works_while_writer_holds_lock() {
    let path = tmp_wal("reader");
    let _ = std::fs::remove_file(&path);
    let mut a = WanningState::live_resuming(&path).expect("开张");
    a.register_delegation(long_lived_delegation())
        .expect("注册");
    assert!(a.decide(&intent(1, 400)).expect("判定").is_allow());

    let replayed = WanningState::replay(&path).expect("持锁期间回放必须可用(审计不因锁而不可见)");
    assert_eq!(replayed.state_hash(), a.state_hash(), "回放态与实时态一致");
    drop(a);
    let _ = std::fs::remove_file(&path);
}

/// 锁文件必须与 WAL 同目录、名为 `<wal文件名>.lock`(相对路径含 `..` 也要成立)。
#[test]
fn lock_path_derivation() {
    let cases = [
        ("target/mcp-demo.wal", "target/mcp-demo.wal.lock"),
        ("C:\\tmp\\a.jsonl", "C:\\tmp\\a.jsonl.lock"),
        ("../x/wal.jsonl", "../x/wal.jsonl.lock"),
    ];
    for (wal, expected) in cases {
        let got = single_writer_lock_path(Path::new(wal));
        assert_eq!(
            got.to_string_lossy().replace('\\', "/"),
            expected.replace('\\', "/"),
            "WAL {wal} 的锁路径"
        );
    }
    assert_eq!(
        single_writer_lock_path(Path::new("wal.jsonl")),
        Path::new("wal.jsonl.lock"),
        "无目录成分的 WAL 也要能派生锁路径"
    );
}
