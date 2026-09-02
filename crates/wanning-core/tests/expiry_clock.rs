//! W-06 · 时钟注入与过期语义。
//!
//! `Clock` trait / `SystemClock` / `MockClock` 本体已随 W-03 落地(`Gate::decide`
//! 必须取当前时间才能判过期)。本文件补齐 W-06 的验收面:
//!
//! 1. **注入真实生效**——判定用的是注入时钟,不是系统时间(否则 1970 时代的测试窗口
//!    早被判过期);
//! 2. **边界语义**——恰在 `valid_until` 时刻按过期处理(fail-closed,半开区间);
//! 3. **过期即拒,且不耗 nonce、不动账本**;
//! 4. **注入时钟一路贯通到审计**——WAL 行里的 `ts` 就是注入时钟的值;
//! 5. **系统时钟路径可用**(`WanningState::live`),全程零 sleep。

use std::sync::Arc;

use wanning_core::clock::{Clock, MockClock, SystemClock};
use wanning_core::delegation::Delegation;
use wanning_core::gate::{DenyReason, Gate, GateDecision};
use wanning_core::intent::SpendIntent;
use wanning_core::state::WanningState;
use wanning_core::wal::{WalDecision, WalRecord};

fn delegation() -> Delegation {
    // ¥10 预算,有效期 [1000, 2000) 秒。
    Delegation::new(
        "d1",
        "boss",
        "claude-code",
        1000,
        1000,
        2000,
        "agent:claude-code",
    )
}

fn intent(nonce: u64, amount_cents: u64) -> SpendIntent {
    SpendIntent::new("d1", nonce, amount_cents, "jd:shop-1", "grocery", "测试")
}

#[test]
fn injected_clock_drives_decisions_not_system_time() {
    // 窗口 [1000, 2000) 在真实世界(2026)早已过期。若闸偷用系统时间,
    // 这里会判 Expired 而不是 Allow——这条测试就是给「注入生效」作证的。
    let clock = MockClock::new(1_000);
    let mut gate = Gate::new(Arc::new(clock.clone()));
    gate.register_delegation(delegation()).expect("注册");

    assert_eq!(gate.clock().now(), 1_000, "闸持有的就是注入时钟");
    assert!(
        gate.decide(&intent(1, 100)).is_allow(),
        "注入时钟说现在 1000 秒,委托在有效期内,必须放行"
    );
    assert!(
        SystemClock.now() > 2_000,
        "系统时间远在窗口之外(2026 年),证明上面放行确实来自注入时钟而非系统时间"
    );
}

#[test]
fn expiry_boundary_is_exact_and_fail_closed() {
    let clock = MockClock::new(1_000);
    let mut gate = Gate::new(Arc::new(clock.clone()));
    gate.register_delegation(delegation()).expect("注册");

    // 一路推到边界前一秒:仍放行。
    clock.set_now(1_999);
    assert!(
        gate.decide(&intent(1, 100)).is_allow(),
        "valid_until 前一秒仍在窗口内"
    );

    // 恰在 valid_until:按过期处理(fail-closed,半开区间 [from, until))。
    clock.set_now(2_000);
    assert_eq!(
        gate.decide(&intent(2, 100)),
        GateDecision::Deny {
            reason: DenyReason::Expired
        }
    );
    // 边界之后一直是过期。
    clock.set_now(2_000 + 86_400);
    assert_eq!(
        gate.decide(&intent(3, 100)),
        GateDecision::Deny {
            reason: DenyReason::Expired
        }
    );
    // 过期拒绝不消耗 nonce、不动账本(实时账本仍是边界前那笔 100 分)。
    assert_eq!(gate.spent_cents("d1"), Some(100));
    assert!(!gate.replay_registry().contains("agent:claude-code", 2));
    assert!(!gate.replay_registry().contains("agent:claude-code", 3));
}

#[test]
fn not_yet_valid_boundary_and_recovery_after_window_opens() {
    let clock = MockClock::new(0);
    let mut gate = Gate::new(Arc::new(clock.clone()));
    gate.register_delegation(delegation()).expect("注册");

    // 委托 1000 秒后才生效:现在(0 秒)必须拒。
    assert_eq!(
        gate.decide(&intent(1, 100)),
        GateDecision::Deny {
            reason: DenyReason::NotYetValid
        }
    );
    // 到点即生效(含端点:valid_from 时刻本身可用)。
    clock.set_now(1_000);
    assert!(gate.decide(&intent(2, 100)).is_allow());
    // 未生效的拒绝同样不占号:nonce=1 之后仍可正常使用。
    assert!(gate.decide(&intent(1, 100)).is_allow(), "拒绝不消耗 nonce");
}

#[test]
fn injected_clock_reaches_the_audit_trail() {
    // WAL 行里的 ts 必须等于注入时钟的值——否则审计时间线不可信。
    let path = std::env::temp_dir()
        .join("wanning-expiry-tests")
        .join(format!("clock-to-wal-{}.jsonl", std::process::id()));
    std::fs::create_dir_all(path.parent().unwrap()).expect("建临时目录");

    let clock = MockClock::new(1_500);
    let mut state = WanningState::with_wal(Arc::new(clock.clone()), &path).expect("开 WAL");
    state.register_delegation(delegation()).expect("注册");
    state.decide(&intent(1, 100)).expect("放行");
    clock.set_now(2_000);
    state.decide(&intent(2, 100)).expect("过期拒");

    let records = wanning_core::wal::read_records(&path).expect("读回审计");
    let ts: Vec<u64> = records.iter().map(|(_, r)| r.ts()).collect();
    assert_eq!(ts, vec![1_500, 1_500, 2_000], "WAL ts 必须逐条跟随注入时钟");

    // 第三条是过期拒绝,审计里要能直接看到 expired。
    match &records[2].1 {
        WalRecord::Decide {
            decision,
            reason,
            budget_after_cents,
            ..
        } => {
            assert_eq!(*decision, WalDecision::Deny);
            assert_eq!(*reason, Some(DenyReason::Expired));
            assert_eq!(*budget_after_cents, 100, "过期拒绝不改账本");
        }
        other => panic!("第三条应是 Decide: {other:?}"),
    }
}

#[test]
fn live_state_uses_system_clock_without_sleep() {
    // 生产路径:系统时钟 + WAL。只验证时间戳来自系统时间,不做任何等待。
    let path = std::env::temp_dir()
        .join("wanning-expiry-tests")
        .join(format!("live-{}.jsonl", std::process::id()));
    std::fs::create_dir_all(path.parent().unwrap()).expect("建临时目录");

    let before = SystemClock.now();
    let mut state = WanningState::live(&path).expect("live 状态");
    state
        .register_delegation(
            // 委托窗口覆盖「现在」,避免被过期语义拒掉。
            Delegation::new(
                "d-live",
                "boss",
                "claude-code",
                1_000,
                before.saturating_sub(60),
                before + 3_600,
                "agent:claude-code",
            ),
        )
        .expect("注册");
    let decision = state
        .decide(&SpendIntent::new(
            "d-live",
            1,
            100,
            "jd:shop-1",
            "grocery",
            "系统时钟冒烟",
        ))
        .expect("判定");
    assert!(decision.is_allow(), "窗口覆盖现在,必须放行");

    let records = wanning_core::wal::read_records(&path).expect("读回审计");
    assert!(
        records[0].1.ts() >= before,
        "WAL ts 必须来自系统时钟,且单调合理"
    );
    assert_eq!(records.len(), 2);
}
