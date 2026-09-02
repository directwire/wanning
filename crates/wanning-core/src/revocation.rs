//! 撤销集合(RevocationSet):kill switch。
//!
//! 语义对齐 mist-core 的 `RevocationRegistry`/`RevocationSet`(撤销即时生效,
//! 已撤销委托的新意图一律拒、不耗 nonce/窗口槽)。
//!
//! **单向开关:本类型刻意不提供「解除撤销」。** 撤销 = 用户收权,必须新建一份委托
//! 重新授权。理由:闸的信任模型里,「撤销后永不允许」是四卖点之一(kill switch),
//! 任何 un-revoke 通路都会让「用户以为已收权」和「agent 还能花钱」之间出现灰色窗口。

use std::collections::BTreeSet;

/// 已撤销的委托 id 集合。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RevocationSet {
    revoked: BTreeSet<String>,
}

impl RevocationSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// 撤销一个委托。重复撤销幂等(返回是否为本次新撤销)。
    pub fn revoke(&mut self, delegation_id: &str) -> bool {
        self.revoked.insert(delegation_id.to_string())
    }

    /// 是否已撤销。
    pub fn is_revoked(&self, delegation_id: &str) -> bool {
        self.revoked.contains(delegation_id)
    }

    /// 已撤销数量(审计用)。
    pub fn len(&self) -> usize {
        self.revoked.len()
    }

    pub fn is_empty(&self) -> bool {
        self.revoked.is_empty()
    }

    /// 迭代已撤销的委托 id(有序,供状态哈希/审计导出)。
    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.revoked.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revoke_then_is_revoked() {
        let mut r = RevocationSet::new();
        assert!(r.is_empty());
        assert!(!r.is_revoked("d1"));
        assert!(r.revoke("d1"), "首次撤销返回 true");
        assert!(r.is_revoked("d1"));
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn revoke_is_idempotent() {
        let mut r = RevocationSet::new();
        assert!(r.revoke("d1"));
        assert!(!r.revoke("d1"), "重复撤销是幂等的,不重复计数");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn revocation_is_one_way_no_unrevoke_api() {
        // 这是语义测试:撤销后没有通路回到未撤销态。集合只能变大。
        let mut r = RevocationSet::new();
        r.revoke("d1");
        r.revoke("d2");
        let before = r.iter().cloned().collect::<Vec<_>>();
        assert_eq!(before, vec!["d1".to_string(), "d2".to_string()]);
        // 只读迭代拿到的集合不可能再被缩回(无任何可变借用出口)。
        assert_eq!(r.len(), 2);
    }
}
