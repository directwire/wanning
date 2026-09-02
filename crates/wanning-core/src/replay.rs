//! nonce 防重放登记(ReplayRegistry)。
//!
//! 语义对齐 mist-core:`spend_nonce` 在 **agent 作用域内**单调唯一,同一
//! `(nonce_scope, nonce)` 只允许成功消费一次;被拒绝的意图**不消耗** nonce
//! (对齐 Mist「已撤销委托新意图一律拒,不耗 nonce/窗口槽」——拒绝不占额度,
//! 这样 agent 修好金额/等用户续预算后用同一 nonce 重发是合法的)。
//!
//! 作用域来自 [`crate::delegation::Delegation::nonce_scope`],通常取 agent 标识:
//! 同一 agent 在多份委托间共用一套 nonce 序列,跨委托重放同 nonce 也会被拦。

use std::collections::BTreeSet;

/// 已被成功消费的 `(nonce_scope, nonce)` 集合。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplayRegistry {
    used: BTreeSet<(String, u64)>,
}

impl ReplayRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 该 (scope, nonce) 是否已被消费过。
    pub fn contains(&self, nonce_scope: &str, nonce: u64) -> bool {
        self.used.contains(&(nonce_scope.to_string(), nonce))
    }

    /// 尝试消费一个 nonce:首次消费返回 true;已被消费过(重放)返回 false 且不改状态。
    pub fn consume(&mut self, nonce_scope: &str, nonce: u64) -> bool {
        self.used.insert((nonce_scope.to_string(), nonce))
    }

    pub fn len(&self) -> usize {
        self.used.len()
    }

    pub fn is_empty(&self) -> bool {
        self.used.is_empty()
    }

    /// 迭代已消费的 (scope, nonce)(有序,供状态哈希/审计导出)。
    pub fn iter(&self) -> impl Iterator<Item = &(String, u64)> {
        self.used.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consume_is_first_use_wins() {
        let mut r = ReplayRegistry::new();
        assert!(r.consume("agent:a", 1), "首次消费成功");
        assert!(!r.consume("agent:a", 1), "同 scope 同 nonce = 重放");
        assert!(r.contains("agent:a", 1));
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn same_nonce_different_scope_is_not_replay() {
        // 作用域隔离:不同 agent 用撞号 nonce 互不影响。
        let mut r = ReplayRegistry::new();
        assert!(r.consume("agent:a", 1));
        assert!(r.consume("agent:b", 1), "跨作用域不算重放");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn rejected_intent_does_not_consume_nonce() {
        // 闸层语义的根基:拒绝不占号。这里先只证登记层的可逆性——
        // 没消费就查不到,「重试同 nonce」因此在闸层是合法的(见 gate 测试)。
        let mut r = ReplayRegistry::new();
        assert!(!r.contains("agent:a", 7));
        assert!(r.consume("agent:a", 7));
        assert!(r.contains("agent:a", 7));
    }
}
