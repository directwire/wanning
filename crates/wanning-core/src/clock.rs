//! 可注入时钟(Clock)。
//!
//! 闸的过期/生效判定依赖「现在几点」。**绝不 `sleep` 测试**——测试用 [`MockClock`]
//! 推时间,生产用 [`SystemClock`]。W-06 在此之上落过期语义与边界(恰在 `valid_until`
//! 按过期处理),trait 本体因 `Gate::decide` 需要取当前时间而提前到 W-03 落地。
//!
//! 线程约定:`SharedClock = Arc<dyn Clock + Send + Sync>`,闸整体可跨线程
//! (P1 MCP server 大概率要多线程,这里不留 Rc 债)。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// 时钟抽象。返回 Unix 秒。(`Debug` 约束让持有闸/账本状态的结构体可以直接 derive Debug。)
pub trait Clock: Send + Sync + std::fmt::Debug {
    fn now(&self) -> u64;
}

/// 闸内共享的时钟句柄。
pub type SharedClock = Arc<dyn Clock + Send + Sync>;

/// 生产时钟:系统时间。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        // 系统时间早于 Unix 纪元(时钟被回拨/损坏)时返回 0:
        // 0 会让一切委托被判为「尚未生效」→ fail-closed,闸宁可全拒也不误放。
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

/// 测试时钟:可任意推时间,无 sleep、无真实等待。
#[derive(Clone, Debug)]
pub struct MockClock {
    now: Arc<AtomicU64>,
}

impl MockClock {
    pub fn new(now: u64) -> Self {
        Self {
            now: Arc::new(AtomicU64::new(now)),
        }
    }

    /// 直接设定当前时刻。
    pub fn set_now(&self, now: u64) {
        self.now.store(now, Ordering::Relaxed);
    }

    /// 前进若干秒(测试里模拟时间流逝)。
    pub fn advance(&self, secs: u64) {
        self.now.fetch_add(secs, Ordering::Relaxed);
    }

    /// 读当前时刻(不经 trait,便于断言)。
    pub fn peek(&self) -> u64 {
        self.now.load(Ordering::Relaxed)
    }
}

impl Clock for MockClock {
    fn now(&self) -> u64 {
        self.peek()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_clock_set_and_advance() {
        let c = MockClock::new(1000);
        assert_eq!(c.peek(), 1000);
        c.advance(50);
        assert_eq!(c.peek(), 1050);
        c.set_now(2000);
        assert_eq!(c.peek(), 2000);
        // trait 视角与直读一致
        let shared: SharedClock = Arc::new(c.clone());
        assert_eq!(shared.now(), 2000);
    }

    #[test]
    fn mock_clock_clone_shares_state() {
        let c = MockClock::new(1);
        let d = c.clone();
        c.set_now(99);
        assert_eq!(d.peek(), 99);
    }

    #[test]
    fn system_clock_returns_unix_seconds() {
        let now = SystemClock.now();
        // 2026-09-02 ≈ 1.787e9;只要落在合理区间,说明读到的是 Unix 秒。
        assert!(now > 1_700_000_000, "SystemClock 应返回 Unix 秒,得到 {now}");
    }
}
