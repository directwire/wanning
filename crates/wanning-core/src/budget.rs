//! 预算账本(BudgetLedger):分/元语义的确定性账本。
//!
//! **刻意本地实现**(任务书 W-02/W-04 授权):mist-core 的账本走链上 Amount 语义,
//! Wanning 是人民币分,语义同形不同纲。对齐点以 `// 语义对齐 mist-core` 标注。
//!
//! 不变量(W-04 property 测试覆盖):
//! 1. 任意操作序列后 `remaining = cap - spent ≥ 0`(即 Σ(成功扣减) ≤ cap);
//! 2. 只有 [`BudgetLedger::commit`] 会增加 `spent`,且只在闸判定通过后被调用;
//! 3. 拒绝不改变任何状态(无「部分扣减」「预扣」这种东西)。

use std::collections::BTreeMap;

use crate::error::CoreError;

/// 委托 id → 已累计消费(分)。不存在的委托视为 0(从未消费)。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BudgetLedger {
    spent_cents: BTreeMap<String, u64>,
}

impl BudgetLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// 已累计消费(分);未知委托 = 0。
    pub fn spent_cents(&self, delegation_id: &str) -> u64 {
        self.spent_cents.get(delegation_id).copied().unwrap_or(0)
    }

    /// 剩余预算(分);未知委托 = `cap`(从未消费)。
    pub fn remaining_cents(&self, delegation_id: &str, cap_cents: u64) -> u64 {
        cap_cents.saturating_sub(self.spent_cents(delegation_id))
    }

    /// 提交一笔扣减,返回扣减后的累计消费(分)。
    ///
    /// 前置条件:调用方(闸)已确认 `spent + amount ≤ cap`。这里仍做溢出防御:
    /// u64 加法溢出 = 状态异常,fail-closed 报错而不是回绕(回绕会把巨额消费记成小额)。
    pub fn commit(&mut self, delegation_id: &str, amount_cents: u64) -> Result<u64, CoreError> {
        let spent = self.spent_cents(delegation_id);
        let after = spent.checked_add(amount_cents).ok_or_else(|| {
            CoreError::LedgerOverflow(format!(
                "预算累计溢出:spent({spent}) + amount({amount_cents}) 超出 u64"
            ))
        })?;
        self.spent_cents.insert(delegation_id.to_string(), after);
        Ok(after)
    }

    /// 撤销(kill switch)后账本不再变化——本方法存在仅为回放/审计对账使用:
    /// 读取某委托的累计消费,不产生任何写。
    pub fn entries(&self) -> impl Iterator<Item = (&String, &u64)> {
        self.spent_cents.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_delegation_reads_as_zero_spent() {
        let l = BudgetLedger::new();
        assert_eq!(l.spent_cents("nope"), 0);
        assert_eq!(l.remaining_cents("nope", 1000), 1000);
    }

    #[test]
    fn commit_accumulates_and_reports_spent_after() {
        let mut l = BudgetLedger::new();
        assert_eq!(l.commit("d1", 300).unwrap(), 300);
        assert_eq!(l.commit("d1", 200).unwrap(), 500);
        assert_eq!(l.commit("d2", 1).unwrap(), 1);
        assert_eq!(l.spent_cents("d1"), 500);
        assert_eq!(l.remaining_cents("d1", 1000), 500);
    }

    #[test]
    fn commit_rejects_overflow_instead_of_wrapping() {
        let mut l = BudgetLedger::new();
        l.commit("d1", u64::MAX - 10).unwrap();
        let err = l.commit("d1", 100).unwrap_err();
        assert!(matches!(err, CoreError::LedgerOverflow(_)), "{err}");
        // 状态未被污染
        assert_eq!(l.spent_cents("d1"), u64::MAX - 10);
    }

    #[test]
    fn zero_amount_commit_is_a_noop_record() {
        // 闸在阶段 0 已拒 0 金额意图;账本层对 0 扣减本身不特殊处理(幂等)。
        let mut l = BudgetLedger::new();
        assert_eq!(l.commit("d1", 0).unwrap(), 0);
        assert_eq!(l.spent_cents("d1"), 0);
    }
}
