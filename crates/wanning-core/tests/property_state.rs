//! W-16 · 状态层不变量 property 测试(WanningState = 闸 + WAL + 回放,零新依赖)。
//!
//! W-04 的 property 测试只对 `Gate` 本体做随机序列;这里把随机序列打到**全状态**上,
//! 每步断言四条状态级不变量:
//!
//! 1. **WAL 记账完备**:每次判定与每次撤销都恰好追加一行(Allow/Deny 一视同仁),
//!    `wal_line_count == 1(注册) + 已落审计动作数`;
//! 2. **实时态与回放态恒等**:任意操作序列后,`live.state_hash() == replay(wal).state_hash()`,
//!    且回放态的账本、撤销、重放登记逐项与模型一致;
//! 3. **回放确定性**:同一 WAL 回放两遍,hash 相同;
//! 4. **篡改 fail-closed**:任何一行被破坏(截半行/字节破坏)后,`replay` 必须报错,
//!    绝不静默跳过、绝不回放出另一个"合法"状态。
//!
//! 生成器与模型账本沿用 property_budget.rs 的写法(独立模型,固定种子可复现);
//! 判定顺序口径对齐任务书 W-03:expired 先于 revoked(先到先拒)。

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use wanning_core::clock::MockClock;
use wanning_core::delegation::Delegation;
use wanning_core::gate::{DenyReason, GateDecision};
use wanning_core::intent::SpendIntent;
use wanning_core::state::WanningState;

const SEED: u64 = 0x57C0_5E17_9E37_79B9;
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

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }
}

/// 独立模型账本(与 property_budget.rs 同一套规则,口径对齐任务书 W-03 判定顺序)。
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

fn fresh_wal_path(case: usize, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("wanning-core-property-state");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间早于 Unix 纪元")
        .as_nanos();
    dir.join(format!("case-{case}-{tag}-{nanos}.jsonl"))
}

#[test]
fn property_state_invariants_hold_under_random_sequences() {
    let mut total_ops = 0u64;
    let mut allows = 0u64;
    let mut revocations = 0u64;
    let mut replay_checks = 0u64;
    let mut tamper_checks = 0u64;
    let mut deny_by_reason: BTreeMap<String, u64> = BTreeMap::new();

    for case in 0..CASES {
        let mut rng = Rng::new(SEED ^ (case as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let wal_path = fresh_wal_path(case, "main");

        let cap_cents = 1 + rng.below(2000);
        let valid_from = 1_000u64;
        let valid_until = valid_from + 1 + rng.below(1_000);
        let clock = MockClock::new(valid_from);
        let mut state =
            WanningState::with_wal(Arc::new(clock.clone()), &wal_path).expect("WAL 可建");

        state
            .register_delegation(Delegation::new(
                "d",
                "boss",
                "agent",
                cap_cents,
                valid_from,
                valid_until,
                format!("agent:case-{case}"),
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
        // 行 1 = register_delegation;此后每次判定/撤销各 +1(推时间不落审计)。
        let mut expected_wal_lines: u64 = 1;
        let mut next_nonce: u64 = 0;

        let ops = 1 + rng.below(30);
        for _ in 0..ops {
            total_ops += 1;
            // 0..=6 消费 / 7 恰满边界 / 8 重放 / 9 撤销 / 10 推时间(值域含 10,
            // 否则 `_` 臂不可达、时钟永停在 valid_from,expired 永远覆盖不到)。
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
                    let actual = state.decide(&intent).expect("WAL 正常时 decide 不报错");
                    assert_eq!(actual, expected, "case {case} 判定与模型不符: {intent:?}");
                    match spent_after {
                        Some(after) => {
                            allows += 1;
                            model.spent_cents = after;
                            model.used_nonces.insert(next_nonce);
                        }
                        None => {
                            if let GateDecision::Deny { reason } = expected {
                                *deny_by_reason.entry(reason.to_string()).or_default() += 1;
                            }
                        }
                    }
                    expected_wal_lines += 1;
                }
                // ── 恰满 cap 边界:剩余额度一笔吃干净 ────────────────────
                7 => {
                    let remaining = model.cap_cents - model.spent_cents;
                    next_nonce += 1;
                    let intent = SpendIntent::new("d", next_nonce, remaining, "jd:shop-1", "t", "");
                    let (expected, spent_after) = model.expect(clock.peek(), next_nonce, remaining);
                    let actual = state.decide(&intent).expect("WAL 正常时 decide 不报错");
                    assert_eq!(actual, expected, "case {case} 恰满 cap 边界与模型不符");
                    if let Some(after) = spent_after {
                        assert_eq!(after, model.cap_cents);
                        model.spent_cents = after;
                        model.used_nonces.insert(next_nonce);
                    }
                    expected_wal_lines += 1;
                }
                // ── 重放已消费的 nonce:必须拒,且账本不动 ────────────────
                8 if !model.used_nonces.is_empty() => {
                    let nonce = *model.used_nonces.iter().next().expect("非空已判");
                    let intent = SpendIntent::new("d", nonce, 1, "jd:shop-1", "t", "");
                    let (expected, spent_after) = model.expect(clock.peek(), nonce, 1);
                    let actual = state.decide(&intent).expect("WAL 正常时 decide 不报错");
                    assert_eq!(actual, expected, "case {case} 重放判定与模型不符");
                    assert!(spent_after.is_none(), "case {case} 重放不得改变账本");
                    if let GateDecision::Deny { reason } = expected {
                        *deny_by_reason.entry(reason.to_string()).or_default() += 1;
                    }
                    expected_wal_lines += 1;
                }
                8 => continue, // 尚无已消费 nonce:跳过(不计 ops 也无妨,已计数)
                // ── 撤销(kill switch):重复撤销也落审计 ─────────────────
                9 => {
                    state.revoke("d").expect("撤销已注册委托");
                    if !model.revoked {
                        revocations += 1;
                        model.revoked = true;
                    }
                    expected_wal_lines += 1;
                }
                // ── 推时间:NotYetValid 之外的时间路径进随机覆盖 ─────────
                _ => {
                    clock.advance(rng.below(200));
                    continue; // 推时间不产生 WAL 行
                }
            }

            // ── 不变量 1:WAL 记账完备(判定与撤销都留痕)─────────────────
            assert_eq!(
                state.wal_line_count(),
                Some(expected_wal_lines),
                "case {case} WAL 行数漂移"
            );

            // ── 不变量 2/3:实时态 == 回放态,且回放可重复 ────────────────
            let replay_1 = WanningState::replay(&wal_path).expect("回放成功");
            let replay_2 = WanningState::replay(&wal_path).expect("回放成功");
            assert_eq!(
                state.state_hash(),
                replay_1.state_hash(),
                "case {case} 实时态与回放态漂移"
            );
            assert_eq!(
                replay_1.state_hash(),
                replay_2.state_hash(),
                "case {case} 回放不确定"
            );
            // 回放态不挂 WAL(不追加记录),账本/撤销必须与模型逐项一致。
            assert_eq!(
                replay_1.wal_line_count(),
                None,
                "case {case} 回放态不得追加记录"
            );
            assert_eq!(
                replay_1.gate().spent_cents("d"),
                Some(model.spent_cents),
                "case {case} 回放账本与模型漂移"
            );
            assert!(
                replay_1.gate().remaining_cents("d").unwrap() + model.spent_cents
                    == model.cap_cents,
                "case {case} 回放侧 remaining+spent 必须 == cap"
            );
            assert_eq!(
                replay_1.gate().is_revoked("d"),
                model.revoked,
                "case {case} 回放侧撤销态与模型漂移"
            );
            replay_checks += 1;
        }

        // ── 不变量 4:篡改 fail-closed(每个 case 收尾各来一次)─────────
        // ① 截半行:追加一行不完整 JSON,replay 必须报错,不静默跳过。
        let mut broken = std::fs::read_to_string(&wal_path).expect("读 WAL");
        broken.push_str("{\"kind\":\"decide\",\"ts\":1");
        std::fs::write(&wal_path, broken).expect("写坏 WAL");
        assert!(
            WanningState::replay(&wal_path).is_err(),
            "case {case} 截半行必须让回放失败(fail-closed)"
        );
        tamper_checks += 1;

        // ② 字节破坏:把首行的首字符 { 换成 x(不再是 JSON),replay 必须报错。
        let wal_path2 = fresh_wal_path(case, "tamper");
        let mut state2 = WanningState::with_wal(Arc::new(MockClock::new(valid_from)), &wal_path2)
            .expect("WAL 可建");
        state2
            .register_delegation(Delegation::new(
                "d",
                "boss",
                "agent",
                cap_cents,
                valid_from,
                valid_until,
                format!("agent:case-{case}-b"),
            ))
            .expect("合法委托");
        state2
            .decide(&SpendIntent::new("d", 1, 1, "jd:shop-1", "t", ""))
            .expect("1 分必过");
        let content = std::fs::read_to_string(&wal_path2).expect("读 WAL");
        let tampered = content.replacen('{', "x", 1);
        assert_ne!(tampered, content, "至少有一行以 {{ 开头可被破坏");
        std::fs::write(&wal_path2, tampered).expect("写坏 WAL");
        assert!(
            WanningState::replay(&wal_path2).is_err(),
            "case {case} 字节破坏必须让回放失败(fail-closed)"
        );
        tamper_checks += 1;
    }

    assert_eq!(CASES, 1000, "case 数被改动:任务书要求 ≥1000");
    println!("property 状态不变量:全绿");
    println!("  case 数                = {CASES}(固定种子 0x{SEED:016X},可复现)");
    println!("  操作总数               = {total_ops}");
    println!("  Allow 次数             = {allows}");
    println!("  撤销次数               = {revocations}");
    println!("  实时==回放 校验次数    = {replay_checks}");
    println!("  篡改 fail-closed 校验  = {tamper_checks}(每 case 两式)");
    println!("  Deny 按原因分布:");
    for (reason, n) in &deny_by_reason {
        println!("    - {reason:<16} = {n}");
    }
    assert!(allows > 0, "随机分布退化:必须覆盖放行路径");
    assert!(revocations > 0, "随机分布退化:必须覆盖撤销路径");
    assert!(
        replay_checks >= CASES as u64,
        "随机分布退化:每个 case 至少一次实时==回放校验"
    );
    assert_eq!(tamper_checks, 2 * CASES as u64, "篡改校验应每 case 两式");
    for reason in [
        "over_budget",
        "expired",
        "revoked",
        "replay",
        "invalid_amount",
        "overflow",
    ] {
        assert!(
            deny_by_reason.get(reason).copied().unwrap_or(0) > 0,
            "随机分布退化:拒绝原因 {reason} 未被覆盖"
        );
    }
}

// ---------------------------------------------------------------------------
// W-27 扩展:策略维度(速率/类目/商户名单/禁止时段)下的状态级不变量。
//
// 策略运行时状态(速度窗口时刻 / 类目台账)是随 commit 演化的状态——必须能从
// WAL 逐行重建,「实时态 == 回放态」(hash 含策略状态)才继续成立。判定顺序口径:
// 阶段 0..4 与主测试一致,阶段 5 = 名单 → 时段 → 速率 → 类目 → 总预算。
// ---------------------------------------------------------------------------

use wanning_core::policy::{QuietWindow, SpendPolicy, VelocityLimit};

/// 策略感知模型:镜像闸的完整判定顺序,独立实现。
#[derive(Debug)]
struct PolicyModel {
    cap_cents: u64,
    valid_from: u64,
    valid_until: u64,
    spent_cents: u64,
    revoked: bool,
    used_nonces: BTreeSet<u64>,
    velocity: Option<VelocityLimit>,
    stamps: Vec<u64>,
    category_caps_cents: BTreeMap<String, u64>,
    category_spent_cents: BTreeMap<String, u64>,
    merchant_allow: BTreeSet<String>,
    merchant_deny: BTreeSet<String>,
    quiet_windows: Vec<QuietWindow>,
}

impl PolicyModel {
    #[allow(clippy::too_many_arguments)]
    fn expect(
        &self,
        now: u64,
        nonce: u64,
        amount_cents: u64,
        merchant: &str,
        category: &str,
    ) -> (GateDecision, Option<u64>) {
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
        // 阶段 5:名单 → 时段 → 速率 → 类目 → 总预算(先到先拒)。
        if self.merchant_deny.contains(merchant) {
            return (deny(DenyReason::MerchantDenied), None);
        }
        if !self.merchant_allow.is_empty() && !self.merchant_allow.contains(merchant) {
            return (deny(DenyReason::MerchantNotAllowed), None);
        }
        if self
            .quiet_windows
            .iter()
            .any(|w| now >= w.from_ts && now < w.until_ts)
        {
            return (deny(DenyReason::QuietHours), None);
        }
        if let Some(v) = &self.velocity {
            let in_window = self
                .stamps
                .iter()
                .filter(|&&t| now.saturating_sub(t) < v.window_secs)
                .count();
            if in_window >= v.max_spends as usize {
                return (deny(DenyReason::RateLimited), None);
            }
        }
        if let Some(cap) = self.category_caps_cents.get(category) {
            let spent = self
                .category_spent_cents
                .get(category)
                .copied()
                .unwrap_or(0);
            let Some(after) = spent.checked_add(amount_cents) else {
                return (deny(DenyReason::Overflow), None);
            };
            if after > *cap {
                return (deny(DenyReason::OverCategoryBudget), None);
            }
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

    /// 放行后的状态迁移(与闸 commit 一致:只有 Allow 才动账本/窗口/类目台账)。
    fn record_allow(
        &mut self,
        now: u64,
        nonce: u64,
        amount_cents: u64,
        after: u64,
        category: &str,
    ) {
        self.spent_cents = after;
        self.used_nonces.insert(nonce);
        if self.velocity.is_some() {
            self.stamps.push(now);
        }
        if self.category_caps_cents.contains_key(category) {
            *self
                .category_spent_cents
                .entry(category.to_string())
                .or_insert(0) += amount_cents;
        }
    }
}

#[test]
fn property_state_policy_dimensions_survive_replay() {
    const CASES: usize = 200;
    const POLICY_SEED_MIX: u64 = 0xC2B2_AE3D_27D4_EB4F;

    let mut total_ops = 0u64;
    let mut allows = 0u64;
    let mut revocations = 0u64;
    let mut replay_checks = 0u64;
    let mut deny_by_reason: BTreeMap<String, u64> = BTreeMap::new();

    for case in 0..CASES {
        let mut rng = Rng::new(SEED ^ (case as u64).wrapping_mul(POLICY_SEED_MIX));
        let wal_path = fresh_wal_path(case, "policy");

        let cap_cents = 1 + rng.below(2000);
        let valid_from = 1_000u64;
        let valid_until = valid_from + 1 + rng.below(2_000);

        // 随机策略:四维各自随机启用(概率偏置保证五条新拒绝原因全部真实可达)。
        let velocity = (rng.below(4) != 3).then(|| VelocityLimit {
            max_spends: 1 + rng.below(3) as u32,
            window_secs: 1 + rng.below(120),
        });
        let category_caps_cents = if rng.below(2) == 0 {
            BTreeMap::from([
                ("a".to_string(), rng.below(600)), // 0 上限 = 禁类目,合法
                ("b".to_string(), 1 + rng.below(600)),
            ])
        } else {
            BTreeMap::new()
        };
        let (merchant_allow, merchant_deny) = {
            let mut allow = BTreeSet::new();
            let mut deny = BTreeSet::new();
            match rng.below(6) {
                0 => {
                    allow.insert("jd:shop-1".to_string());
                }
                1 => {
                    deny.insert("jd:shop-2".to_string());
                }
                2 => {
                    allow.insert("jd:shop-1".to_string());
                    allow.insert("jd:shop-2".to_string());
                }
                3 => {
                    deny.insert("jd:shop-1".to_string());
                    deny.insert("jd:shop-2".to_string());
                }
                4 => {
                    // deny 优先现场:同一商户同时在两份名单。
                    allow.insert("jd:shop-1".to_string());
                    deny.insert("jd:shop-1".to_string());
                }
                _ => {}
            }
            (allow, deny)
        };
        let quiet_windows = if rng.below(3) == 0 {
            let from = 1_000 + rng.below(900);
            vec![QuietWindow {
                from_ts: from,
                until_ts: from + 1 + rng.below(300),
            }]
        } else {
            Vec::new()
        };

        let policy = SpendPolicy {
            velocity, // Option<VelocityLimit> 是 Copy,直接按值放(策略与模型各一份)
            category_caps_cents: category_caps_cents.clone(),
            merchant_allow: merchant_allow.clone(),
            merchant_deny: merchant_deny.clone(),
            quiet_windows: quiet_windows.clone(),
        };

        let clock = MockClock::new(valid_from);
        let mut state =
            WanningState::with_wal(Arc::new(clock.clone()), &wal_path).expect("WAL 可建");
        state
            .register_delegation(
                Delegation::new(
                    "d",
                    "boss",
                    "agent",
                    cap_cents,
                    valid_from,
                    valid_until,
                    format!("agent:case-{case}-p"),
                )
                .with_policy(policy),
            )
            .expect("模型构造的委托合法");

        let mut model = PolicyModel {
            cap_cents,
            valid_from,
            valid_until,
            spent_cents: 0,
            revoked: false,
            used_nonces: BTreeSet::new(),
            velocity,
            stamps: Vec::new(),
            category_caps_cents,
            category_spent_cents: BTreeMap::new(),
            merchant_allow,
            merchant_deny,
            quiet_windows,
        };
        let mut expected_wal_lines: u64 = 1;
        let mut next_nonce: u64 = 0;

        let ops = 1 + rng.below(20);
        for _ in 0..ops {
            total_ops += 1;
            match rng.below(9) {
                // 消费意图(多数路径)
                0..=5 => {
                    next_nonce += 1;
                    let merchant = if rng.below(10) < 7 {
                        "jd:shop-1"
                    } else {
                        "jd:shop-2"
                    };
                    let category = ["a", "b", "c"][rng.below(3) as usize];
                    let amount_cents = match rng.below(10) {
                        0 => cap_cents + 1 + rng.below(1_000), // 超额
                        1 => u64::MAX,                         // 溢出
                        2 => 0,                                // 非法金额
                        _ => 1 + rng.below(cap_cents),         // 常规
                    };
                    let intent =
                        SpendIntent::new("d", next_nonce, amount_cents, merchant, category, "");
                    let (expected, spent_after) =
                        model.expect(clock.peek(), next_nonce, amount_cents, merchant, category);
                    let actual = state.decide(&intent).expect("WAL 正常时 decide 不报错");
                    assert_eq!(
                        actual, expected,
                        "case {case} 策略判定与模型不符: {intent:?}"
                    );
                    match spent_after {
                        Some(after) => {
                            allows += 1;
                            model.record_allow(
                                clock.peek(),
                                next_nonce,
                                amount_cents,
                                after,
                                category,
                            );
                        }
                        None => {
                            if let GateDecision::Deny { reason } = expected {
                                *deny_by_reason.entry(reason.to_string()).or_default() += 1;
                            }
                        }
                    }
                    expected_wal_lines += 1;
                }
                // 重放已消费 nonce(replay 先于策略,模型镜像同序)
                6 if !model.used_nonces.is_empty() => {
                    let nonce = *model.used_nonces.iter().next().expect("非空已判");
                    let intent = SpendIntent::new("d", nonce, 1, "jd:shop-1", "a", "");
                    let (expected, spent_after) =
                        model.expect(clock.peek(), nonce, 1, "jd:shop-1", "a");
                    let actual = state.decide(&intent).expect("WAL 正常时 decide 不报错");
                    assert_eq!(actual, expected, "case {case} 重放判定与模型不符");
                    assert!(spent_after.is_none(), "case {case} 重放不得改变账本");
                    if let GateDecision::Deny { reason } = expected {
                        *deny_by_reason.entry(reason.to_string()).or_default() += 1;
                    }
                    expected_wal_lines += 1;
                }
                6 => continue, // 尚无已消费 nonce:跳过
                // 撤销(kill switch)
                7 => {
                    state.revoke("d").expect("撤销已注册委托");
                    if !model.revoked {
                        revocations += 1;
                        model.revoked = true;
                    }
                    expected_wal_lines += 1;
                }
                // 推时间(步长与速率窗口/禁止窗口同量级)
                _ => {
                    clock.advance(rng.below(150));
                    continue; // 推时间不产生 WAL 行
                }
            }

            // ── 不变量 1:WAL 记账完备 ────────────────────────────────────
            assert_eq!(
                state.wal_line_count(),
                Some(expected_wal_lines),
                "case {case} WAL 行数漂移"
            );

            // ── 不变量 2/3:实时态 == 回放态(含策略状态),回放可重复 ─────
            let replay_1 = WanningState::replay(&wal_path).expect("回放成功");
            let replay_2 = WanningState::replay(&wal_path).expect("回放成功");
            assert_eq!(
                state.state_hash(),
                replay_1.state_hash(),
                "case {case} 实时态与回放态漂移(含策略运行时状态)"
            );
            assert_eq!(
                replay_1.state_hash(),
                replay_2.state_hash(),
                "case {case} 回放不确定"
            );
            assert_eq!(
                replay_1.gate().spent_cents("d"),
                Some(model.spent_cents),
                "case {case} 回放账本与模型漂移"
            );
            assert_eq!(
                replay_1.gate().velocity_stamps("d"),
                model.stamps.as_slice(),
                "case {case} 速率窗口时刻与模型漂移"
            );
            for cat in model.category_caps_cents.keys() {
                assert_eq!(
                    replay_1.gate().category_spent_cents("d", cat),
                    model.category_spent_cents.get(cat).copied(),
                    "case {case} 类目台账与模型漂移: {cat}"
                );
            }
            assert_eq!(
                replay_1.gate().is_revoked("d"),
                model.revoked,
                "case {case} 回放撤销态与模型漂移"
            );
            replay_checks += 1;
        }
    }

    println!("property 策略维度状态不变量:全绿");
    println!("  case 数                = {CASES}(固定种子 0x{SEED:016X} ^ 0x{POLICY_SEED_MIX:016X},可复现)");
    println!("  操作总数               = {total_ops}");
    println!("  Allow 次数             = {allows}");
    println!("  撤销次数               = {revocations}");
    println!("  实时==回放 校验次数    = {replay_checks}");
    println!("  Deny 按原因分布:");
    for (reason, n) in &deny_by_reason {
        println!("    - {reason:<22} = {n}");
    }
    assert!(allows > 0, "随机分布退化:必须覆盖放行路径");
    assert!(revocations > 0, "随机分布退化:必须覆盖撤销路径");
    assert!(
        replay_checks >= CASES as u64,
        "随机分布退化:每个 case 至少一次实时==回放校验"
    );
    // 五条新拒绝原因必须全部被随机序列真实命中(零编造:跑出来的才作数)。
    for reason in [
        "rate_limited",
        "over_category_budget",
        "merchant_denied",
        "merchant_not_allowed",
        "quiet_hours",
    ] {
        assert!(
            deny_by_reason.get(reason).copied().unwrap_or(0) > 0,
            "随机分布退化:策略拒绝原因 {reason} 未被覆盖"
        );
    }
}
