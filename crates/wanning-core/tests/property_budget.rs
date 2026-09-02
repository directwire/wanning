//! W-04 · 预算账本不变量 property 测试(手写伪随机序列生成器,零新依赖)。
//!
//! 对闸做随机操作序列(Spend / 撤销 / 推时间 / 重放同 nonce),每步用一个
//! **独立模型账本**(测试内手写)核对闸的真实状态,并断言四条不变量:
//!
//! 1. 任意操作序列后 `remaining = cap - spent ≥ 0`(即 Σ(成功扣减) ≤ cap);
//! 2. 闸内累计消费恒等于模型累计消费(Allow 才扣减,Deny 零副作用);
//! 3. 撤销之后:remaining 不再变化,且后续一切意图必被 Deny(Revoked);
//! 4. 已成功消费的 nonce 再发必被 Deny(Replay),且状态不变。
//!
//! 生成器:xorshift64*(固定种子,失败可复现)。刻意不引 `proptest`:
//! 需求只是「≥1000 case 的随机序列 + 可复现」,手写 20 行即可,少一棵依赖树。

use std::collections::BTreeSet;
use std::sync::Arc;

use wanning_core::clock::MockClock;
use wanning_core::delegation::Delegation;
use wanning_core::gate::{DenyReason, Gate, GateDecision};
use wanning_core::intent::SpendIntent;

/// 固定种子:任何人重跑得到同一序列,失败可复现。
const SEED: u64 = 0x57C0_4E1C_9E37_79B9;
const CASES: usize = 1000;

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

/// 模型账本:与闸同一套规则,独立实现,用来核对闸的真实行为。
#[derive(Debug)]
struct Model {
    cap_cents: u64,
    valid_from: u64,
    valid_until: u64,
    spent_cents: u64,
    revoked: bool,
    used_nonces: BTreeSet<u64>,
}

impl Model {
    /// 期望的判定结果 + 预期的状态迁移(返回 spent_after,Deny 时为 None)。
    fn expect(&self, now: u64, nonce: u64, amount_cents: u64) -> (GateDecision, Option<u64>) {
        if amount_cents == 0 {
            return (deny(DenyReason::InvalidAmount), None);
        }
        if nonce == 0 {
            return (deny(DenyReason::InvalidNonce), None);
        }
        if now < self.valid_from {
            return (deny(DenyReason::NotYetValid), None);
        }
        if now >= self.valid_until {
            return (deny(DenyReason::Expired), None);
        }
        // 口径对齐任务书 W-03 判定顺序:expired 先于 revoked(先到先拒)。
        if self.revoked {
            return (deny(DenyReason::Revoked), None);
        }
        if self.used_nonces.contains(&nonce) {
            return (deny(DenyReason::Replay), None);
        }
        let Some(total) = self.spent_cents.checked_add(amount_cents) else {
            return (deny(DenyReason::Overflow), None);
        };
        if total > self.cap_cents {
            return (deny(DenyReason::OverBudget), None);
        }
        (
            GateDecision::Allow {
                budget_after_cents: total,
            },
            Some(total),
        )
    }
}

fn deny(reason: DenyReason) -> GateDecision {
    GateDecision::Deny { reason }
}

#[test]
fn property_budget_invariants_hold_under_random_sequences() {
    let mut total_ops = 0u64;
    let mut allows = 0u64;
    let mut denies = 0u64;
    let mut revocations = 0u64;
    let mut replay_attempts = 0u64;
    let mut exact_cap_hits = 0u64;
    let mut deny_by_reason = std::collections::BTreeMap::new();

    for case in 0..CASES {
        let mut rng = Rng::new(SEED ^ (case as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));

        // 每个 case 一份独立委托:随机预算上限(1..=2000 分)与有效期窗口。
        let cap_cents = 1 + rng.below(2000);
        let valid_from = 1_000u64;
        let valid_until = valid_from + 1 + rng.below(1_000);
        let nonce_scope = format!("agent:case-{case}");
        let clock = MockClock::new(valid_from);
        let mut gate = Gate::new(Arc::new(clock.clone()));
        gate.register_delegation(Delegation::new(
            "d",
            "boss",
            "agent",
            cap_cents,
            valid_from,
            valid_until,
            nonce_scope.as_str(),
        ))
        .expect("模型构造的委托合法");

        let mut model = Model {
            cap_cents,
            valid_from,
            valid_until,
            spent_cents: 0,
            revoked: false,
            used_nonces: BTreeSet::new(),
        };
        let mut spent_at_revoke: Option<u64> = None;
        let mut next_nonce: u64 = 0;

        let ops = 1 + rng.below(40);
        for _ in 0..ops {
            total_ops += 1;
            // 操作分布(11 路均匀):7/11 消费、1/11 恰满 cap、1/11 重放、
            // 1/11 撤销、1/11 推时间——保证「先消费后撤销」「先消费后过期」都可达。
            match rng.below(11) {
                // ── 消费意图(多数路径)──────────────────────────────────
                0..=6 => {
                    next_nonce += 1;
                    let amount_cents = match rng.below(10) {
                        0 => cap_cents + 1 + rng.below(1_000), // 超额
                        1 => u64::MAX,                         // 溢出
                        2 => 0,                                // 非法金额
                        _ => 1 + rng.below(cap_cents),         // 常规
                    };
                    let intent =
                        SpendIntent::new("d", next_nonce, amount_cents, "jd:shop-1", "t", "");
                    let (expected, spent_after) =
                        model.expect(clock.peek(), next_nonce, amount_cents);
                    let actual = gate.decide(&intent);
                    assert_eq!(actual, expected, "case {case} 判定与模型不符: {intent:?}");
                    match spent_after {
                        Some(after) => {
                            allows += 1;
                            model.spent_cents = after;
                            model.used_nonces.insert(next_nonce);
                        }
                        None => {
                            denies += 1;
                            *deny_by_reason
                                .entry(expected.deny_reason().expect("Deny 必有 reason"))
                                .or_insert(0u64) += 1;
                        }
                    }
                }
                // ── 恰好补满 cap 的边界意图(确定性覆盖「恰满 cap 应放行」)──
                7 => {
                    let remaining = model.cap_cents - model.spent_cents;
                    next_nonce += 1;
                    let intent = SpendIntent::new("d", next_nonce, remaining, "jd:shop-1", "t", "");
                    let (expected, spent_after) = model.expect(clock.peek(), next_nonce, remaining);
                    let actual = gate.decide(&intent);
                    assert_eq!(actual, expected, "case {case} 恰满 cap 边界与模型不符");
                    if let Some(after) = spent_after {
                        exact_cap_hits += 1;
                        assert_eq!(after, model.cap_cents, "补满后应恰好等于 cap");
                        assert_eq!(gate.remaining_cents("d"), Some(0));
                        model.spent_cents = after;
                        model.used_nonces.insert(next_nonce);
                    }
                }
                // ── 重放:重发一个已成功消费的 nonce ──────────────────────
                8 if !model.used_nonces.is_empty() => {
                    replay_attempts += 1;
                    let nonce = *model.used_nonces.iter().next().expect("非空已判");
                    let intent = SpendIntent::new("d", nonce, 1, "jd:shop-1", "t", "");
                    let (expected, spent_after) = model.expect(clock.peek(), nonce, 1);
                    let actual = gate.decide(&intent);
                    assert_eq!(actual, expected, "case {case} 重放判定与模型不符");
                    // 重放绝不产生新的扣减。
                    assert!(spent_after.is_none(), "case {case} 重放不得改变账本");
                    // 委托存活(未撤销、未过期)时,拒绝原因必须精确是 Replay。
                    if !model.revoked
                        && clock.peek() >= model.valid_from
                        && clock.peek() < model.valid_until
                    {
                        assert_eq!(
                            expected.deny_reason(),
                            Some(DenyReason::Replay),
                            "case {case} 存活委托上重放必须报 replay"
                        );
                    }
                }
                // ── 撤销(kill switch)───────────────────────────────────
                9 => {
                    gate.revoke("d").expect("撤销已注册委托");
                    if !model.revoked {
                        revocations += 1;
                        model.revoked = true;
                        spent_at_revoke = Some(model.spent_cents);
                    }
                }
                // ── 推时间:让 NotYetValid/Expired 两条路径也进随机覆盖 ──
                _ => {
                    // 步长取小(≤200s),避免大部分 case 过早集体过期、浪费后半序列。
                    let secs = rng.below(200);
                    clock.advance(secs);
                }
            }

            // ── 不变量 1/2:spent ≤ cap 且闸与模型完全一致 ────────────────
            let spent = gate.spent_cents("d").expect("委托已注册");
            assert_eq!(spent, model.spent_cents, "case {case} 闸账本与模型漂移");
            assert!(
                spent <= model.cap_cents,
                "case {case} 不变量破坏:Σ(成功扣减) {spent} > cap {cap_cents}"
            );
            let remaining = gate.remaining_cents("d").expect("委托已注册");
            assert_eq!(
                remaining,
                model.cap_cents - spent,
                "case {case} remaining 口径错"
            );
            assert!(
                model.cap_cents >= spent,
                "case {case} 不变量破坏:remaining < 0(spent {spent} > cap {cap_cents})"
            );

            // ── 不变量 3:撤销之后 remaining 永不再变,且一切意图必拒 ────
            if let Some(frozen) = spent_at_revoke {
                assert!(model.revoked, "case {case} 撤销后模型仍应处于撤销态");
                assert_eq!(
                    spent, frozen,
                    "case {case} 不变量破坏:撤销后 remaining 仍发生了变化"
                );
                // 用全新 nonce 也必须被拒(kill switch,不是预算问题)。
                // 注意口径:任务书判定顺序是 expired → revoked,若已撤销的委托
                // 同时已过有效期,闸报 Expired(先到先拒),两种标签都是「拒」。
                next_nonce += 1;
                let probe = SpendIntent::new("d", next_nonce, 1, "jd:shop-1", "t", "");
                let expected_reason = if clock.peek() >= model.valid_until {
                    DenyReason::Expired
                } else {
                    DenyReason::Revoked
                };
                assert_eq!(
                    gate.decide(&probe),
                    GateDecision::Deny {
                        reason: expected_reason
                    },
                    "case {case} 撤销后新意图必须被拒"
                );
                assert_eq!(
                    gate.spent_cents("d"),
                    Some(frozen),
                    "case {case} 探针意图不得改变账本"
                );
            }
        }
    }

    assert_eq!(CASES, 1000, "case 数被改动:任务书要求 ≥1000");
    println!("property 预算不变量:全绿");
    println!("  case 数          = {CASES}(固定种子 0x{SEED:016X},可复现)");
    println!("  操作总数         = {total_ops}");
    println!("  Allow(成功扣减) = {allows}");
    println!("  Deny             = {denies}");
    for (reason, n) in &deny_by_reason {
        println!("    - {reason:<20} = {n}");
    }
    println!("  撤销次数         = {revocations}");
    println!("  重放尝试(被拒) = {replay_attempts}");
    println!("  恰满 cap 放行    = {exact_cap_hits}");
    assert!(
        allows > 0 && denies > 0,
        "随机分布退化:必须同时覆盖放行与拒绝"
    );
    assert!(revocations > 0, "随机分布退化:必须覆盖撤销路径");
    assert!(exact_cap_hits > 0, "随机分布退化:必须覆盖恰满 cap 边界");
}

// ---------------------------------------------------------------------------
// W-27 扩展:速率限制与总预算联合 property(任务书指定「至少把 velocity 与预算
// 联合打」)。独立模型镜像「阶段 5:速率 → 总预算」顺序,时间随机推进让窗口滑动。
// ---------------------------------------------------------------------------

/// 速率+预算模型:与闸同一套规则,独立实现。
#[derive(Debug)]
struct VelocityModel {
    cap_cents: u64,
    valid_from: u64,
    valid_until: u64,
    spent_cents: u64,
    revoked: bool,
    used_nonces: BTreeSet<u64>,
    velocity: wanning_core::policy::VelocityLimit,
    /// 成功放行时刻(全量保留,与闸访问器逐位对齐)。
    stamps: Vec<u64>,
}

impl VelocityModel {
    fn expect(&self, now: u64, nonce: u64, amount_cents: u64) -> (GateDecision, Option<u64>) {
        if amount_cents == 0 {
            return (deny(DenyReason::InvalidAmount), None);
        }
        if nonce == 0 {
            return (deny(DenyReason::InvalidNonce), None);
        }
        if now < self.valid_from {
            return (deny(DenyReason::NotYetValid), None);
        }
        if now >= self.valid_until {
            return (deny(DenyReason::Expired), None);
        }
        if self.revoked {
            return (deny(DenyReason::Revoked), None);
        }
        if self.used_nonces.contains(&nonce) {
            return (deny(DenyReason::Replay), None);
        }
        // 阶段 5a:速率(先于总预算,先到先拒)。
        let in_window = self
            .stamps
            .iter()
            .filter(|&&t| now.saturating_sub(t) < self.velocity.window_secs)
            .count();
        if in_window >= self.velocity.max_spends as usize {
            return (deny(DenyReason::RateLimited), None);
        }
        // 阶段 5b:总预算。
        let Some(total) = self.spent_cents.checked_add(amount_cents) else {
            return (deny(DenyReason::Overflow), None);
        };
        if total > self.cap_cents {
            return (deny(DenyReason::OverBudget), None);
        }
        (
            GateDecision::Allow {
                budget_after_cents: total,
            },
            Some(total),
        )
    }
}

#[test]
fn property_velocity_and_budget_invariants_hold_under_random_sequences() {
    use wanning_core::policy::{SpendPolicy, VelocityLimit};

    const CASES: usize = 1000;
    const VELOCITY_SEED_MIX: u64 = 0xC2B2_AE3D_27D4_EB4F;

    let mut total_ops = 0u64;
    let mut allows = 0u64;
    let mut rate_limited = 0u64;
    let mut revocations = 0u64;

    for case in 0..CASES {
        let mut rng = Rng::new(SEED ^ (case as u64).wrapping_mul(VELOCITY_SEED_MIX));

        let cap_cents = 1 + rng.below(2000);
        let valid_from = 1_000u64;
        let valid_until = valid_from + 1 + rng.below(1_000);
        // 速率上限 1..=3 笔、窗口 1..=300 秒(与推时间步长同量级,窗口会真实滑动)。
        let velocity = VelocityLimit {
            max_spends: 1 + rng.below(3) as u32,
            window_secs: 1 + rng.below(300),
        };
        let clock = MockClock::new(valid_from);
        let mut gate = Gate::new(Arc::new(clock.clone()));
        gate.register_delegation(
            Delegation::new(
                "d",
                "boss",
                "agent",
                cap_cents,
                valid_from,
                valid_until,
                format!("agent:case-{case}-v"),
            )
            .with_policy(SpendPolicy {
                velocity: Some(velocity),
                ..SpendPolicy::default()
            }),
        )
        .expect("模型构造的委托合法");

        let mut model = VelocityModel {
            cap_cents,
            valid_from,
            valid_until,
            spent_cents: 0,
            revoked: false,
            used_nonces: BTreeSet::new(),
            velocity,
            stamps: Vec::new(),
        };
        let mut next_nonce: u64 = 0;

        let ops = 1 + rng.below(40);
        for _ in 0..ops {
            total_ops += 1;
            match rng.below(10) {
                // 消费意图(多数路径)
                0..=6 => {
                    next_nonce += 1;
                    let amount_cents = match rng.below(10) {
                        0 => cap_cents + 1 + rng.below(1_000), // 超额
                        1 => u64::MAX,                         // 溢出
                        2 => 0,                                // 非法金额
                        _ => 1 + rng.below(cap_cents),         // 常规
                    };
                    let intent =
                        SpendIntent::new("d", next_nonce, amount_cents, "jd:shop-1", "t", "");
                    let (expected, spent_after) =
                        model.expect(clock.peek(), next_nonce, amount_cents);
                    let actual = gate.decide(&intent);
                    assert_eq!(
                        actual, expected,
                        "case {case} 速率+预算联合判定与模型不符: {intent:?}"
                    );
                    match spent_after {
                        Some(after) => {
                            allows += 1;
                            model.spent_cents = after;
                            model.used_nonces.insert(next_nonce);
                            model.stamps.push(clock.peek());
                        }
                        None => {
                            if expected.deny_reason() == Some(DenyReason::RateLimited) {
                                rate_limited += 1;
                            }
                        }
                    }
                }
                // 重放已消费 nonce
                7 if !model.used_nonces.is_empty() => {
                    let nonce = *model.used_nonces.iter().next().expect("非空已判");
                    let intent = SpendIntent::new("d", nonce, 1, "jd:shop-1", "t", "");
                    let (expected, spent_after) = model.expect(clock.peek(), nonce, 1);
                    assert_eq!(
                        gate.decide(&intent),
                        expected,
                        "case {case} 重放判定与模型不符"
                    );
                    assert!(spent_after.is_none(), "case {case} 重放不得改变账本");
                }
                7 => continue,
                // 撤销
                8 => {
                    gate.revoke("d").expect("撤销已注册委托");
                    if !model.revoked {
                        revocations += 1;
                        model.revoked = true;
                    }
                }
                // 推时间(步长与窗口同量级,窗口真实滑动)
                _ => clock.advance(rng.below(300)),
            }

            // ── 不变量:闸与模型逐位一致(账本 + 速率窗口时刻)─────────────
            let spent = gate.spent_cents("d").expect("委托已注册");
            assert_eq!(spent, model.spent_cents, "case {case} 闸账本与模型漂移");
            assert!(spent <= cap_cents, "case {case} 总预算不变量破坏");
            assert_eq!(
                gate.velocity_stamps("d"),
                model.stamps.as_slice(),
                "case {case} 速率窗口时刻与模型漂移"
            );
            // 速率不变量:任意时刻窗口内成功笔数 ≤ max(模型侧按构造成立,闸侧独立核)。
            let now = clock.peek();
            let in_window = gate
                .velocity_stamps("d")
                .iter()
                .filter(|&&t| now.saturating_sub(t) < velocity.window_secs)
                .count();
            assert!(
                in_window <= velocity.max_spends as usize,
                "case {case} 速率不变量破坏:窗口内 {in_window} 笔 > 上限 {}",
                velocity.max_spends
            );
        }
    }

    assert_eq!(CASES, 1000, "case 数被改动:任务书要求 ≥1000");
    println!("property 速率+预算联合不变量:全绿");
    println!(
        "  case 数          = {CASES}(固定种子 0x{SEED:016X} ^ 0x{VELOCITY_SEED_MIX:016X},可复现)"
    );
    println!("  操作总数         = {total_ops}");
    println!("  Allow            = {allows}");
    println!("  RateLimited 拒   = {rate_limited}");
    println!("  撤销次数         = {revocations}");
    assert!(allows > 0, "随机分布退化:必须覆盖放行路径");
    assert!(rate_limited > 0, "随机分布退化:速率拒绝路径未被真实覆盖");
    assert!(revocations > 0, "随机分布退化:必须覆盖撤销路径");
}
