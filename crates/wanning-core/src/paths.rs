//! W-43a 产品化:默认路径与 WAL 目录自动创建。
//!
//! 产品化的「零配置」体验:用户什么都不配置时,审计账本落在用户家目录的
//! `.wanning/wal.jsonl` 下(Windows = `%USERPROFILE%\.wanning\wal.jsonl`),
//! 父目录不存在就自动创建。两件事都刻意**保守**:
//!
//! - 家目录解析顺序固定为 `WANNING_HOME`(测试/隔离开关,不是用户要配的东西)
//!   → `USERPROFILE`(Windows 标准)→ `HOME`(Unix 标准);三处都拿不到就返回
//!   `None`,调用方 fail-closed 报错并给「显式路径」逃生门——**绝不猜一个落点**,
//!   也绝不静默落到当前目录。
//! - 只自动建 WAL 的父目录这一个目录,不建「.wanning 之外的任何东西」;裸文件名
//!   (无父目录)是 no-op。

use std::env;
use std::path::{Path, PathBuf};

use crate::error::CoreError;

/// 家目录环境变量,按优先级排列(测试隔离 > Windows 标准 > Unix 标准)。
const HOME_VARS: [&str; 3] = ["WANNING_HOME", "USERPROFILE", "HOME"];

/// 解析家目录。三处都拿不到 = `None`(fail-closed,调用方报错,绝不猜落点)。
pub fn home_dir() -> Option<PathBuf> {
    for key in HOME_VARS {
        match env::var_os(key) {
            Some(value) if !value.is_empty() => return Some(PathBuf::from(value)),
            _ => continue,
        }
    }
    None
}

/// 默认配置/账本根目录:`<home>/.wanning`(Windows = `%USERPROFILE%\.wanning`)。
pub fn default_home() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".wanning"))
}

/// 默认审计账本路径:`<home>/.wanning/wal.jsonl`。纯路径计算,零 IO。
pub fn default_wal_path() -> Option<PathBuf> {
    default_home().map(|home| home.join("wal.jsonl"))
}

/// 确保 WAL 的父目录存在(不存在则递归创建;已存在或裸文件名 = no-op)。
///
/// 在 [`crate::wal::Wal::open`] 里于拿锁**之前**调用:锁文件 `<wal>.lock` 要落在
/// 刚建出来的目录里。Windows 的裸文件名 `Path::new("a.jsonl").parent()` 返回
/// `Some("")`,空串按「无目录」处理。
pub fn ensure_wal_parent(wal_path: &Path) -> Result<(), CoreError> {
    let Some(parent) = wal_path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(parent).map_err(|e| {
        CoreError::WalIo(format!(
            "创建审计账本目录 {parent:?} 失败: {e}(可用显式路径绕开默认位置)"
        ))
    })
}
