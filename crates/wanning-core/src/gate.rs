//! 闸判定面(Gate):四卖点的语义核心。
//!
//! fail-closed 检查顺序(先到先拒):
//!
//! 0. 意图自身合法性(金额/nonce 为正、必填字段非空)——非法意图不必看委托状态
//! 1. 未知委托(未注册的 delegation_id)
//! 2. 未生效 / 已过期(恰在 `valid_until` 按过期处理,半开区间 fail-closed)
//! 3. 已撤销(kill switch:撤销后**永不允许**;先于 nonce 检查,对齐 Mist
//!    「已撤销委托新意图一律拒,不耗 nonce/窗口槽」)
//! 4. 重放((nonce_scope, nonce) 已被成功消费)
//! 5. 策略与预算(先到先拒:**商户名单 → 禁止时段 → 速率 → 类目 → 总预算**;
//!    溢出 → Overflow)——全部通过才原子扣减并消耗 nonce
//!
//! **任何 Deny 都不消耗 nonce、不动账本、不占速率窗口槽**;只有 Allow 才是
//! 「这笔消费可以发生」。两阶段 API 供审计层使用(write-ahead,先落审计再扣账):
//! [`Gate::evaluate`](纯检查)→ 写 WAL → [`Gate::commit`](落地扣减);
//! [`Gate::decide`] 是「判定+落地」一步到位的便捷入口。
//!
//! **单次时钟读**:判定所需的 `now` 由入口(`decide`/`commit`/回放)读一次
//! 时钟后经 [`Gate::evaluate_at`]/[`Gate::commit_at`] 显式传入。若 evaluate 与
//! commit 各自读时钟,跨秒边界时实时侧的速率窗口时刻(WAL ts)与回放侧(记录
//! ts)会漂移,诚实账本的 live_resuming 对账也会 fail-closed——同一个判定必须
//! 用同一个 now。
//!
//! 语义对齐 mist-core 的 `check_budget` 规则 1/3/5(有效期、单笔与总上限)与
//! 撤销/重放闸口顺序;策略维度(速率/类目/商户/时段)是 W-27 增量,挂在委托上
//! ([`crate::delegation::Delegation::policy`]),缺省策略行为与本层引入前一致。

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::budget::BudgetLedger;
use crate::clock::{SharedClock, SystemClock};
use crate::delegation::Delegation;
use crate::error::CoreError;
use crate::intent::SpendIntent;
use crate::policy::{MerchantVerdict, PolicyState};
use crate::replay::ReplayRegistry;
use crate::revocation::RevocationSet;

use serde::{Deserialize, Serialize};

/// 拒绝原因。WAL 与对外审计直接落这里(小写蛇形,机器可 diff,人可读)。
/// (`Ord`/`Hash` 供统计聚合与集合去重使用;派生顺序即枚举声明顺序,不承载语义。)
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// 委托未注册。
    UnknownDelegation,
    /// 委托尚未到生效时刻。
    NotYetValid,
    /// 委托已过期(含恰在 `valid_until` 时刻)。
    Expired,
    /// 委托已被撤销(kill switch,撤销后永不允许)。
    Revoked,
    /// 重放:该 (nonce_scope, nonce) 已被成功消费过。
    Replay,
    /// 超出总预算。
    OverBudget,
    /// 金额加法溢出(u64),按 fail-closed 处理为拒绝。
    Overflow,
    /// 金额非法(0 分)。
    InvalidAmount,
    /// nonce 非法(0)。
    InvalidNonce,
    /// 意图字段非法(必填字段为空白等),不属于以上具体情形。
    InvalidIntent,
    /// 速率限制:滑动窗口内成功放行笔数已达上限(W-27)。
    RateLimited,
    /// 类目预算超限(该类目设了上限;未知类目 fail-open,不产生本原因)(W-27)。
    OverCategoryBudget,
    /// 商户在黑名单(或与白名单冲突,deny 优先)(W-27)。
    MerchantDenied,
    /// 商户不在白名单(白名单非空时;allow 空 = 不设白名单)(W-27)。
    MerchantNotAllowed,
    /// 禁止时段(`[from_ts, until_ts)` 绝对 Unix 秒,半开)(W-27)。
    QuietHours,
}

impl std::fmt::Display for DenyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DenyReason::UnknownDelegation => "unknown_delegation",
            DenyReason::NotYetValid => "not_yet_valid",
            DenyReason::Expired => "expired",
            DenyReason::Revoked => "revoked",
            DenyReason::Replay => "replay",
            DenyReason::OverBudget => "over_budget",
            DenyReason::Overflow => "overflow",
            DenyReason::InvalidAmount => "invalid_amount",
            DenyReason::InvalidNonce => "invalid_nonce",
            DenyReason::InvalidIntent => "invalid_intent",
            DenyReason::RateLimited => "rate_limited",
            DenyReason::OverCategoryBudget => "over_category_budget",
            DenyReason::MerchantDenied => "merchant_denied",
            DenyReason::MerchantNotAllowed => "merchant_not_allowed",
            DenyReason::QuietHours => "quiet_hours",
        };
        f.write_str(s)
    }
}

/// 闸的判定结果。
///
/// `Allow` 携带扣减后的累计消费(`budget_after_cents`),供审计直接引用;
/// `Deny` 携带原因。金额字段一律分。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateDecision {
    Allow { budget_after_cents: u64 },
    Deny { reason: DenyReason },
}

impl GateDecision {
    /// 是否放行(调用方据此决定是否触发下游真实消费)。
    pub fn is_allow(&self) -> bool {
        matches!(self, GateDecision::Allow { .. })
    }

    /// 拒绝原因(Allow 时为 None)。
    pub fn deny_reason(&self) -> Option<DenyReason> {
        match self {
            GateDecision::Allow { .. } => None,
            GateDecision::Deny { reason } => Some(*reason),
        }
    }
}

#[cfg(test)]
mod type_tests {
    use super::*;

    #[test]
    fn deny_reason_display_is_snake_case() {
        assert_eq!(DenyReason::OverBudget.to_string(), "over_budget");
        assert_eq!(
            DenyReason::UnknownDelegation.to_string(),
            "unknown_delegation"
        );
        assert_eq!(DenyReason::NotYetValid.to_string(), "not_yet_valid");
        assert_eq!(DenyReason::InvalidIntent.to_string(), "invalid_intent");
        assert_eq!(DenyReason::RateLimited.to_string(), "rate_limited");
        assert_eq!(
            DenyReason::OverCategoryBudget.to_string(),
            "over_category_budget"
        );
        assert_eq!(DenyReason::MerchantDenied.to_string(), "merchant_denied");
        assert_eq!(
            DenyReason::MerchantNotAllowed.to_string(),
            "merchant_not_allowed"
        );
        assert_eq!(DenyReason::QuietHours.to_string(), "quiet_hours");
    }

    #[test]
    fn deny_reason_serde_roundtrip_snake_case() {
        for r in [
            DenyReason::UnknownDelegation,
            DenyReason::NotYetValid,
            DenyReason::Expired,
            DenyReason::Revoked,
            DenyReason::Replay,
            DenyReason::OverBudget,
            DenyReason::Overflow,
            DenyReason::InvalidAmount,
            DenyReason::InvalidNonce,
            DenyReason::InvalidIntent,
            DenyReason::RateLimited,
            DenyReason::OverCategoryBudget,
            DenyReason::MerchantDenied,
            DenyReason::MerchantNotAllowed,
            DenyReason::QuietHours,
        ] {
            let json = serde_json::to_string(&r).expect("序列化");
            let back: DenyReason = serde_json::from_str(&json).expect("反序列化");
            assert_eq!(back, r, "{r} roundtrip");
        }
        assert_eq!(
            serde_json::to_string(&DenyReason::OverBudget).unwrap(),
            "\"over_budget\""
        );
    }

    #[test]
    fn decision_shape_and_accessors() {
        let allow = GateDecision::Allow {
            budget_after_cents: 500,
        };
        assert!(allow.is_allow());
        assert_eq!(allow.deny_reason(), None);

        let deny = GateDecision::Deny {
            reason: DenyReason::Revoked,
        };
        assert!(!deny.is_allow());
        assert_eq!(deny.deny_reason(), Some(DenyReason::Revoked));
    }

    #[test]
    fn decision_serde_roundtrip() {
        for d in [
            GateDecision::Allow {
                budget_after_cents: 1,
            },
            GateDecision::Deny {
                reason: DenyReason::Replay,
            },
        ] {
            let json = serde_json::to_string(&d).expect("序列化");
            let back: GateDecision = serde_json::from_str(&json).expect("反序列化");
            assert_eq!(back, d);
        }
    }
}

/// 闸。单实例持有若干委托及其账本/撤销/重放/策略状态。
#[derive(Debug)]
pub struct Gate {
    delegations: BTreeMap<String, Delegation>,
    revocations: RevocationSet,
    replay: ReplayRegistry,
    ledger: BudgetLedger,
    /// 每委托的策略运行时状态(速率窗口时刻/类目台账)。只在对应维度启用时
    /// 才有数据——缺省策略的委托在本表恒无条目(零额外内存,回放逐位可重建)。
    policy_states: BTreeMap<String, PolicyState>,
    clock: SharedClock,
}

impl Gate {
    /// 用给定时钟建闸。
    pub fn new(clock: SharedClock) -> Self {
        Self {
            delegations: BTreeMap::new(),
            revocations: RevocationSet::new(),
            replay: ReplayRegistry::new(),
            ledger: BudgetLedger::new(),
            policy_states: BTreeMap::new(),
            clock,
        }
    }

    /// 用系统时钟建闸(生产路径)。
    pub fn with_system_clock() -> Self {
        Self::new(Arc::new(SystemClock))
    }

    pub fn clock(&self) -> &SharedClock {
        &self.clock
    }

    /// 换时钟句柄(断点续跑用):闸的委托/账本/撤销/nonce 状态都不含时钟,
    /// 句柄可整体替换——回放对账时用记录 ts 驱动的 [`MockClock`],校验过后
    /// 换回系统时钟继续服务,状态原封不动。
    pub fn with_clock(mut self, clock: SharedClock) -> Self {
        self.clock = clock;
        self
    }

    /// 注册一份委托(fail-closed:校验不过 / id 重复 → 拒收)。
    ///
    /// nonce_scope 允许与既有委托相同(同一 agent 多份委托共享 nonce 序列,属预期设计)。
    pub fn register_delegation(&mut self, delegation: Delegation) -> Result<(), CoreError> {
        delegation.validate()?;
        if self.delegations.contains_key(&delegation.id) {
            return Err(CoreError::DuplicateDelegation(delegation.id));
        }
        self.delegations.insert(delegation.id.clone(), delegation);
        Ok(())
    }

    /// 撤销委托(kill switch,单向,幂等)。未知委托 → 错误。
    pub fn revoke(&mut self, delegation_id: &str) -> Result<(), CoreError> {
        if !self.delegations.contains_key(delegation_id) {
            return Err(CoreError::UnknownDelegation(delegation_id.to_string()));
        }
        self.revocations.revoke(delegation_id);
        Ok(())
    }

    pub fn is_revoked(&self, delegation_id: &str) -> bool {
        self.revocations.is_revoked(delegation_id)
    }

    pub fn delegation(&self, delegation_id: &str) -> Option<&Delegation> {
        self.delegations.get(delegation_id)
    }

    /// 全部已注册委托(有序,供状态哈希/审计导出)。
    pub fn delegations(&self) -> impl Iterator<Item = &Delegation> {
        self.delegations.values()
    }

    /// 某委托已累计消费(分);未知委托返回 None。
    pub fn spent_cents(&self, delegation_id: &str) -> Option<u64> {
        self.delegations
            .contains_key(delegation_id)
            .then(|| self.ledger.spent_cents(delegation_id))
    }

    /// 某委托剩余预算(分);未知委托返回 None。
    pub fn remaining_cents(&self, delegation_id: &str) -> Option<u64> {
        let cap = self.delegations.get(delegation_id)?.budget_cap_cents;
        Some(self.ledger.remaining_cents(delegation_id, cap))
    }

    pub fn revocations(&self) -> &RevocationSet {
        &self.revocations
    }

    pub fn replay_registry(&self) -> &ReplayRegistry {
        &self.replay
    }

    pub fn ledger(&self) -> &BudgetLedger {
        &self.ledger
    }

    /// 某委托的速率窗口成功放行时刻(全量保留,升序);未启用速率或无记录 → 空切片。
    pub fn velocity_stamps(&self, delegation_id: &str) -> &[u64] {
        self.policy_states
            .get(delegation_id)
            .map(|s| s.velocity_stamps.as_slice())
            .unwrap_or(&[])
    }

    /// 某委托某类目的累计消费(分);未设上限(无台账)或无记录 → None。
    pub fn category_spent_cents(&self, delegation_id: &str, category: &str) -> Option<u64> {
        self.policy_states
            .get(delegation_id)?
            .category_spent_cents
            .get(category)
            .copied()
    }

    /// 全部策略运行时状态(有序,供状态哈希/审计导出)。
    pub fn policy_states(&self) -> impl Iterator<Item = (&String, &PolicyState)> {
        self.policy_states.iter()
    }

    /// 纯检查:不修改任何状态(写审计层在放行时先落 WAL 再 commit)。
    pub fn evaluate(&self, intent: &SpendIntent) -> GateDecision {
        self.evaluate_at(intent, self.clock.now())
    }

    /// [`Gate::evaluate`] 的显式时刻变体:调用方读一次时钟、传同一 `now`
    /// 给 evaluate 与 commit,保证实时侧与回放侧的速率窗口判定用同一时刻。
    pub(crate) fn evaluate_at(&self, intent: &SpendIntent, now: u64) -> GateDecision {
        // ── 阶段 0:意图自身合法性 ─────────────────────────────────────────
        // 与 SpendIntent::validate 同一套规则,但这里产出 DenyReason(业务拒绝,
        // 落审计)而不是 Err(程序错误)。见 gate 测试 `stage0_matches_intent_validate`。
        if intent.amount_cents == 0 {
            return deny(DenyReason::InvalidAmount);
        }
        if intent.nonce == 0 {
            return deny(DenyReason::InvalidNonce);
        }
        if intent.delegation_id.trim().is_empty() {
            return deny(DenyReason::UnknownDelegation);
        }
        if intent.merchant_id.trim().is_empty() {
            return deny(DenyReason::InvalidIntent);
        }

        // ── 阶段 1:未知委托 ─────────────────────────────────────────────
        let Some(delegation) = self.delegations.get(&intent.delegation_id) else {
            return deny(DenyReason::UnknownDelegation);
        };

        // ── 阶段 2:有效期(先看时钟,再看撤销——过期委托连撤销检查都不必做) ──
        if delegation.not_yet_valid(now) {
            return deny(DenyReason::NotYetValid);
        }
        if delegation.is_expired(now) {
            return deny(DenyReason::Expired);
        }

        // ── 阶段 3:撤销(kill switch)─────────────────────────────────
        if self.revocations.is_revoked(&delegation.id) {
            return deny(DenyReason::Revoked);
        }

        // ── 阶段 4:重放 ────────────────────────────────────────────────
        if self.replay.contains(&delegation.nonce_scope, intent.nonce) {
            return deny(DenyReason::Replay);
        }

        // ── 阶段 5:策略与预算(先到先拒)────────────────────────────────
        // 顺序:商户名单 → 禁止时段 → 速率 → 类目 → 总预算。
        // 策略状态按需读取:缺省策略的委托没有运行时状态,零拷贝零分配。
        let empty = PolicyState::default();
        let policy_state = self.policy_states.get(&delegation.id).unwrap_or(&empty);
        let policy = &delegation.policy;

        // 5a 商户名单(deny 优先;allow 空 = 不设白名单)。
        match policy.merchant_verdict(&intent.merchant_id) {
            Some(MerchantVerdict::Denied) => return deny(DenyReason::MerchantDenied),
            Some(MerchantVerdict::NotAllowed) => return deny(DenyReason::MerchantNotAllowed),
            None => {}
        }

        // 5b 禁止时段(绝对 Unix 秒,半开)。
        if policy.is_quiet(now) {
            return deny(DenyReason::QuietHours);
        }

        // 5c 速率限制(滑动窗口,整数秒;只有成功放行才会计入——此刻尚未 commit)。
        if let Some(v) = &policy.velocity {
            if policy_state.in_window_count(now, v.window_secs) >= v.max_spends as usize {
                return deny(DenyReason::RateLimited);
            }
        }

        // 5d 类目预算(未知类目 fail-open = 无类目预算,总预算仍管;上限 0 = 禁类目)。
        if let Some(cap) = policy.category_caps_cents.get(intent.category.as_str()) {
            let spent = policy_state
                .category_spent_cents
                .get(intent.category.as_str())
                .copied()
                .unwrap_or(0);
            let Some(after) = spent.checked_add(intent.amount_cents) else {
                return deny(DenyReason::Overflow);
            };
            if after > *cap {
                return deny(DenyReason::OverCategoryBudget);
            }
        }

        // 5e 总预算(语义对齐 mist-core check_budget 规则 5),先做溢出防御再比上限。
        let spent = self.ledger.spent_cents(&delegation.id);
        let Some(total) = spent.checked_add(intent.amount_cents) else {
            return deny(DenyReason::Overflow);
        };
        if total > delegation.budget_cap_cents {
            return deny(DenyReason::OverBudget);
        }
        GateDecision::Allow {
            budget_after_cents: total,
        }
    }

    /// 落地一笔已放行的意图:扣减预算 + 消耗 nonce + 记策略状态。
    /// 返回扣减后的累计消费(分)。
    ///
    /// 前置条件:`evaluate` 对同一意图返回 Allow(WanningState 的 write-ahead 顺序
    /// 依赖这一点)。commit 自身仍会重新 evaluate 防御 API 误用。
    pub fn commit(&mut self, intent: &SpendIntent) -> Result<u64, CoreError> {
        self.commit_at(intent, self.clock.now())
    }

    /// [`Gate::commit`] 的显式时刻变体(与 [`Gate::evaluate_at`] 同一 `now`)。
    pub(crate) fn commit_at(&mut self, intent: &SpendIntent, now: u64) -> Result<u64, CoreError> {
        match self.evaluate_at(intent, now) {
            GateDecision::Allow { budget_after_cents } => {
                let delegation = self
                    .delegations
                    .get(&intent.delegation_id)
                    .expect("evaluate 放行 ⇒ 委托必已注册");
                let nonce_scope = delegation.nonce_scope.clone();
                let velocity_enabled = delegation.policy.velocity.is_some();
                let capped_category = delegation
                    .policy
                    .category_caps_cents
                    .contains_key(intent.category.as_str())
                    .then(|| intent.category.clone());
                let after = self
                    .ledger
                    .commit(&intent.delegation_id, intent.amount_cents)?;
                self.replay.consume(&nonce_scope, intent.nonce);
                // 策略状态只在对应维度启用时才产生数据(缺省策略零额外状态,
                // 回放/实时两侧的 state_hash 因此天然一致)。
                if velocity_enabled || capped_category.is_some() {
                    let state = self
                        .policy_states
                        .entry(intent.delegation_id.clone())
                        .or_default();
                    if velocity_enabled {
                        state.record_velocity_stamp(now);
                    }
                    if let Some(category) = capped_category {
                        state.record_category_spend(&category, intent.amount_cents);
                    }
                }
                debug_assert_eq!(after, budget_after_cents);
                Ok(after)
            }
            GateDecision::Deny { reason } => Err(CoreError::CommitRejected(format!(
                "delegation={} nonce={} reason={reason}",
                intent.delegation_id, intent.nonce
            ))),
        }
    }

    /// 判定并落地(便捷入口,任务书 W-03 指定签名)。
    ///
    /// 时钟**只读一次**:evaluate 与 commit 用同一 `now`,否则跨秒边界时
    /// WAL 记录的 ts 与速率窗口时刻可能不一致,回放对账 fail-closed。
    pub fn decide(&mut self, intent: &SpendIntent) -> GateDecision {
        let now = self.clock.now();
        match self.evaluate_at(intent, now) {
            GateDecision::Allow { budget_after_cents } => {
                // 不可能失败:evaluate_at 已用同一时刻排除溢出,且中间无状态变更。
                let after = self
                    .commit_at(intent, now)
                    .expect("evaluate_at 放行 ⇒ commit_at 必成功(同一时刻重判)");
                debug_assert_eq!(after, budget_after_cents);
                GateDecision::Allow {
                    budget_after_cents: after,
                }
            }
            deny => deny,
        }
    }
}

fn deny(reason: DenyReason) -> GateDecision {
    GateDecision::Deny { reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ¥10 总预算、有效期 [1000, 2000) 秒、nonce 作用域 agent:claude-code 的样例闸。
    fn gate_with(now: u64) -> (Gate, crate::clock::MockClock) {
        let clock = crate::clock::MockClock::new(now);
        let mut gate = Gate::new(Arc::new(clock.clone()));
        gate.register_delegation(Delegation::new(
            "d1",
            "boss",
            "claude-code",
            1000,
            1000,
            2000,
            "agent:claude-code",
        ))
        .expect("样例委托合法");
        (gate, clock)
    }

    fn intent(nonce: u64, amount_cents: u64) -> SpendIntent {
        SpendIntent::new("d1", nonce, amount_cents, "jd:shop-1", "grocery", "测试")
    }

    #[test]
    fn allow_happy_path_deducts_budget() {
        let (mut gate, _clock) = gate_with(1500);
        assert_eq!(
            gate.decide(&intent(1, 500)),
            GateDecision::Allow {
                budget_after_cents: 500
            }
        );
        assert_eq!(gate.remaining_cents("d1"), Some(500));
        assert_eq!(gate.spent_cents("d1"), Some(500));
    }

    #[test]
    fn deny_unknown_delegation() {
        let (mut gate, _clock) = gate_with(1500);
        let i = SpendIntent::new("ghost", 1, 100, "jd:shop-1", "x", "");
        assert_eq!(
            gate.decide(&i),
            GateDecision::Deny {
                reason: DenyReason::UnknownDelegation
            }
        );
        assert_eq!(gate.spent_cents("ghost"), None);
    }

    #[test]
    fn deny_not_yet_valid() {
        let (mut gate, _clock) = gate_with(999);
        assert_eq!(
            gate.decide(&intent(1, 100)),
            GateDecision::Deny {
                reason: DenyReason::NotYetValid
            }
        );
    }

    #[test]
    fn deny_expired_including_exact_boundary() {
        let (mut gate, clock) = gate_with(1999);
        assert!(
            gate.decide(&intent(1, 100)).is_allow(),
            "valid_until 前一秒仍可消费"
        );
        clock.set_now(2000);
        assert_eq!(
            gate.decide(&intent(2, 100)),
            GateDecision::Deny {
                reason: DenyReason::Expired
            },
            "恰在 valid_until 时刻必须按过期处理(fail-closed)"
        );
    }

    #[test]
    fn deny_revoked_and_never_allowed_again() {
        let (mut gate, clock) = gate_with(1500);
        assert!(gate.decide(&intent(1, 100)).is_allow());
        gate.revoke("d1").expect("撤销已注册委托");
        assert_eq!(
            gate.decide(&intent(2, 100)),
            GateDecision::Deny {
                reason: DenyReason::Revoked
            }
        );
        // 撤销后永不允许:时间前进、预算充足、nonce 全新,仍然拒。
        clock.advance(100);
        assert_eq!(
            gate.decide(&intent(3, 1)),
            GateDecision::Deny {
                reason: DenyReason::Revoked
            }
        );
        // 撤销不影响既有账本(kill switch 是止血,不是抹账)。
        assert_eq!(gate.spent_cents("d1"), Some(100));
    }

    #[test]
    fn deny_replay_same_nonce_same_scope() {
        let (mut gate, _clock) = gate_with(1500);
        assert!(gate.decide(&intent(1, 100)).is_allow());
        assert_eq!(
            gate.decide(&intent(1, 100)),
            GateDecision::Deny {
                reason: DenyReason::Replay
            }
        );
        // 同 nonce、不同金额,一样是重放(重放判定只看 nonce)。
        assert_eq!(
            gate.decide(&intent(1, 1)),
            GateDecision::Deny {
                reason: DenyReason::Replay
            }
        );
    }

    #[test]
    fn replay_is_scoped_by_nonce_scope() {
        // 同一 agent 的两份委托共享 nonce_scope → 跨委托重放同 nonce 也被拦。
        let clock = crate::clock::MockClock::new(1500);
        let mut gate = Gate::new(Arc::new(clock));
        gate.register_delegation(Delegation::new(
            "d1",
            "boss",
            "claude-code",
            1000,
            1000,
            2000,
            "agent:claude-code",
        ))
        .unwrap();
        gate.register_delegation(Delegation::new(
            "d2",
            "boss",
            "claude-code",
            1000,
            1000,
            2000,
            "agent:claude-code",
        ))
        .unwrap();
        assert!(gate.decide(&intent(1, 100)).is_allow());
        let other = SpendIntent::new("d2", 1, 100, "jd:shop-1", "x", "");
        assert_eq!(
            gate.decide(&other),
            GateDecision::Deny {
                reason: DenyReason::Replay
            },
            "同作用域跨委托重放同 nonce 必须被拦"
        );
    }

    #[test]
    fn deny_over_budget_but_exact_cap_is_allowed() {
        let (mut gate, _clock) = gate_with(1500);
        assert!(gate.decide(&intent(1, 500)).is_allow());
        assert!(
            gate.decide(&intent(2, 500)).is_allow(),
            "恰好花满 cap 应放行"
        );
        assert_eq!(gate.remaining_cents("d1"), Some(0));
        assert_eq!(
            gate.decide(&intent(3, 1)),
            GateDecision::Deny {
                reason: DenyReason::OverBudget
            }
        );
    }

    #[test]
    fn deny_amount_overflow() {
        let (mut gate, _clock) = gate_with(1500);
        assert!(gate.decide(&intent(1, 500)).is_allow());
        let huge = SpendIntent::new("d1", 2, u64::MAX, "jd:shop-1", "x", "");
        assert_eq!(
            gate.decide(&huge),
            GateDecision::Deny {
                reason: DenyReason::Overflow
            }
        );
        // 状态未被污染
        assert_eq!(gate.spent_cents("d1"), Some(500));
    }

    #[test]
    fn deny_invalid_amount_zero() {
        let (mut gate, _clock) = gate_with(1500);
        assert_eq!(
            gate.decide(&intent(1, 0)),
            GateDecision::Deny {
                reason: DenyReason::InvalidAmount
            }
        );
    }

    #[test]
    fn deny_invalid_nonce_zero() {
        let (mut gate, _clock) = gate_with(1500);
        assert_eq!(
            gate.decide(&intent(0, 100)),
            GateDecision::Deny {
                reason: DenyReason::InvalidNonce
            }
        );
    }

    #[test]
    fn deny_invalid_intent_empty_merchant() {
        let (mut gate, _clock) = gate_with(1500);
        let i = SpendIntent::new("d1", 1, 100, " ", "x", "");
        assert_eq!(
            gate.decide(&i),
            GateDecision::Deny {
                reason: DenyReason::InvalidIntent
            }
        );
    }

    #[test]
    fn stage0_matches_intent_validate() {
        // 闸的阶段 0 与 SpendIntent::validate 必须对同一非法意图给出一致结论,
        // 否则决策循环先 validate 后提交会出现「validate 过了闸却拒」的口径漂移。
        let (gate, _clock) = gate_with(1500);
        let cases = vec![
            intent(1, 0),                                        // 金额 0
            intent(0, 100),                                      // nonce 0
            SpendIntent::new("", 1, 100, "jd:shop-1", "x", ""),  // 空 delegation_id
            SpendIntent::new("d1", 1, 100, "", "x", ""),         // 空 merchant_id
            SpendIntent::new(" ", 1, 100, "jd:shop-1", "x", ""), // 空白 delegation_id
        ];
        for c in cases {
            let validates = c.validate();
            let decision = gate.evaluate(&c);
            if validates.is_ok() {
                assert!(decision.is_allow(), "validate 通过但闸拒: {c:?}");
            } else {
                assert!(
                    decision.deny_reason().is_some(),
                    "validate 拒但闸放行: {c:?}"
                );
            }
        }
    }

    #[test]
    fn denied_intent_does_not_consume_nonce() {
        // 拒绝不占号:修好金额后用同一 nonce 重发是合法的。
        let (mut gate, _clock) = gate_with(1500);
        assert_eq!(
            gate.decide(&intent(1, 5000)),
            GateDecision::Deny {
                reason: DenyReason::OverBudget
            }
        );
        assert!(
            gate.decide(&intent(1, 100)).is_allow(),
            "同一 nonce 在拒绝后重发应放行"
        );
        assert_eq!(gate.spent_cents("d1"), Some(100));
    }

    #[test]
    fn evaluate_is_pure_commit_is_the_only_mutation() {
        let (mut gate, _clock) = gate_with(1500);
        for _ in 0..5 {
            assert!(
                gate.evaluate(&intent(1, 100)).is_allow(),
                "evaluate 反复调用结果一致且不改状态"
            );
        }
        assert_eq!(gate.spent_cents("d1"), Some(0));
        assert!(!gate.replay_registry().contains("agent:claude-code", 1));
        gate.commit(&intent(1, 100)).expect("放行后 commit");
        assert_eq!(gate.spent_cents("d1"), Some(100));
        assert!(gate.replay_registry().contains("agent:claude-code", 1));
    }

    #[test]
    fn commit_rejects_when_gate_would_deny() {
        let (mut gate, _clock) = gate_with(1500);
        let err = gate.commit(&intent(1, 5000)).unwrap_err();
        assert!(matches!(err, CoreError::CommitRejected(_)), "{err}");
        assert_eq!(gate.spent_cents("d1"), Some(0));
    }

    #[test]
    fn decide_matches_evaluate_then_commit() {
        let (mut gate, _clock) = gate_with(1500);
        let d = gate.decide(&intent(1, 300));
        let spent = gate.spent_cents("d1");
        let replayed = gate.replay_registry().contains("agent:claude-code", 1);
        assert_eq!(spent, Some(300));
        assert!(replayed);
        assert!(d.is_allow());
        // 换一份干净闸,走两阶段路径,结果必须完全一致。
        let (mut gate2, _c) = gate_with(1500);
        let verdict = gate2.evaluate(&intent(1, 300));
        let after = gate2.commit(&intent(1, 300)).unwrap();
        assert_eq!(
            verdict,
            GateDecision::Allow {
                budget_after_cents: after
            }
        );
    }

    #[test]
    fn register_rejects_invalid_and_duplicate() {
        let (mut gate, _clock) = gate_with(1500);
        let bad = Delegation::new("bad", "boss", "agent", 0, 1000, 2000, "s");
        assert!(matches!(
            gate.register_delegation(bad),
            Err(CoreError::InvalidDelegation(_))
        ));
        let dup = Delegation::new("d1", "boss", "agent", 1000, 1000, 2000, "s");
        assert!(matches!(
            gate.register_delegation(dup),
            Err(CoreError::DuplicateDelegation(_))
        ));
        assert_eq!(gate.delegations().count(), 1);
    }

    #[test]
    fn revoke_unknown_delegation_is_an_error() {
        let (mut gate, _clock) = gate_with(1500);
        assert!(matches!(
            gate.revoke("ghost"),
            Err(CoreError::UnknownDelegation(_))
        ));
    }
}
