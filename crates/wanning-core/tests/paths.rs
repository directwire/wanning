//! W-43a 产品化:默认路径(`wanning_core::paths`)与 WAL 目录自动创建。
//!
//! 产品化要求:用户什么都不配置时,默认审计账本落在用户家目录的 `.wanning/` 下
//! (Windows = `%USERPROFILE%\.wanning`),父目录不存在就自动创建。解析顺序刻意
//! Windows 惯例优先:`WANNING_HOME`(本仓测试/隔离开关,不是用户要配的东西)→
//! `USERPROFILE`(Windows 标准)→ `HOME`(Unix 标准);三处都拿不到 = `None`,
//! 调用方 fail-closed 报错并给「显式路径」逃生门——绝不猜一个落点。
//!
//! 铁律:测试绝不碰真实家目录。每个用例先抢环境变量互斥锁,再把涉及的变量
//! 改写或清空(Drop 恢复原值);断言目标一律是本测试自建的临时目录。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use wanning_core::paths;
use wanning_core::state::WanningState;
use wanning_core::wal::Wal;

const HOME_VARS: [&str; 3] = ["WANNING_HOME", "USERPROFILE", "HOME"];

fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 保存并改写环境变量,Drop 恢复(测试间互不污染,更不污染真实家目录)。
struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn set(vars: &[(&'static str, String)]) -> Self {
        let saved = vars
            .iter()
            .map(|(k, _)| (*k, std::env::var_os(k)))
            .collect();
        for (k, v) in vars {
            std::env::set_var(k, v);
        }
        Self { saved }
    }

    fn clear(vars: &[&'static str]) -> Self {
        let saved = vars.iter().map(|k| (*k, std::env::var_os(k))).collect();
        for k in vars {
            std::env::remove_var(k);
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in std::mem::take(&mut self.saved) {
            match v {
                Some(value) => std::env::set_var(k, value),
                None => std::env::remove_var(k),
            }
        }
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "w43-paths-{}-{}-{}-{}",
        tag,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间早于 Unix 纪元")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

// ── 解析顺序:WANNING_HOME → USERPROFILE → HOME → None(fail-closed) ──────

#[test]
fn wanning_home_wins_over_userprofile_and_home() {
    let _lock = env_lock();
    let home = temp_dir("wanning-home");
    let _clear = EnvGuard::clear(&HOME_VARS);
    let _set = EnvGuard::set(&[("WANNING_HOME", home.to_string_lossy().into_owned())]);
    assert_eq!(
        paths::default_wal_path(),
        Some(home.join(".wanning").join("wal.jsonl"))
    );
}

#[test]
fn userprofile_is_the_windows_default() {
    let _lock = env_lock();
    let home = temp_dir("userprofile");
    let _set = EnvGuard::set(&[("USERPROFILE", home.to_string_lossy().into_owned())]);
    let _clear = EnvGuard::clear(&["WANNING_HOME"]);
    assert_eq!(
        paths::default_wal_path(),
        Some(home.join(".wanning").join("wal.jsonl"))
    );
}

#[test]
fn home_is_the_last_fallback() {
    let _lock = env_lock();
    let home = temp_dir("home-fallback");
    let _clear = EnvGuard::clear(&["WANNING_HOME", "USERPROFILE"]);
    let _set = EnvGuard::set(&[("HOME", home.to_string_lossy().into_owned())]);
    assert_eq!(
        paths::default_wal_path(),
        Some(home.join(".wanning").join("wal.jsonl"))
    );
}

#[test]
fn default_home_is_dot_wanning_under_the_resolved_home() {
    let _lock = env_lock();
    let home = temp_dir("default-home");
    let _set = EnvGuard::set(&[("WANNING_HOME", home.to_string_lossy().into_owned())]);
    assert_eq!(paths::default_home(), Some(home.join(".wanning")));
    assert_eq!(
        paths::default_wal_path(),
        paths::default_home().map(|h| h.join("wal.jsonl"))
    );
}

#[test]
fn no_home_anywhere_fails_closed_to_none() {
    let _lock = env_lock();
    let _clear = EnvGuard::clear(&HOME_VARS);
    assert_eq!(
        paths::home_dir(),
        None,
        "找不到家目录必须返回 None,绝不猜落点"
    );
    assert_eq!(paths::default_home(), None);
    assert_eq!(paths::default_wal_path(), None);
}

// ── 自动建目录 ────────────────────────────────────────────────────────────

#[test]
fn ensure_wal_parent_creates_missing_directories_and_is_idempotent() {
    let dir = temp_dir("ensure-parent");
    let wal = dir.join("nested").join("deeper").join("audit.jsonl");
    assert!(!wal.parent().expect("有父目录").exists());
    paths::ensure_wal_parent(&wal).expect("父目录不存在时应自动创建");
    assert!(wal.parent().expect("有父目录").is_dir(), "目录已被创建");
    paths::ensure_wal_parent(&wal).expect("已存在 = no-op");
}

#[test]
fn ensure_wal_parent_on_a_bare_filename_is_a_noop() {
    // 相对裸文件名(父目录是空串)没有目录可建,必须不动、不报错。
    paths::ensure_wal_parent(Path::new("w43-bare-name.wal")).expect("裸文件名不应报错");
}

#[test]
fn wal_open_creates_missing_parent_directory() {
    let dir = temp_dir("wal-open-mkdir");
    let wal = dir.join("a").join("b").join("audit.jsonl");
    let opened = Wal::open(&wal).expect("默认路径父目录不存在时应自动创建");
    assert_eq!(opened.line_count(), 0);
    assert!(wal.parent().expect("有父目录").is_dir(), "目录已被自动创建");
    assert!(wal.exists(), "WAL 文件本体已创建");
}

#[test]
fn live_resuming_opens_a_default_path_whose_parents_do_not_exist() {
    // 产品入口(live_resuming)与 Wal::open 同一条路径:自动建目录在开锁之前,
    // 锁文件才能落在刚建出来的目录里。
    let dir = temp_dir("live-mkdir");
    let wal = dir.join("x").join("y").join("wal.jsonl");
    let state = WanningState::live_resuming(&wal).expect("产品默认路径应自动建目录后开闸");
    assert_eq!(state.wal_line_count(), Some(0));
}
