//! 消费意图(SpendIntent):agent 发起的一笔待判定消费。
//!
//! 这是闸的**输入**。闸只看意图,不看 agent 内部怎么想的——决策循环(GLM/脚本)
//! 负责产出意图,闸负责判定与扣减。
//!
//! 语义对齐 mist-core 的 `SpendIntent`(delegation_hash/recipient/amount/category/
//! spend_nonce),差异点:
//! - Wanning 用人类可读的 `delegation_id` / `merchant_id` / `category` 字符串,
//!   不做哈希(链下审计要人能读懂;Mist 哈希是为了进电路)。
//! - `nonce` 防重放(同一 nonce_scope 内只许成功消费一次),Mist 侧同名同义。

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpendIntent {
    /// 目标委托 id(必须已注册,否则 UnknownDelegation 拒)。
    pub delegation_id: String,
    /// 防重放 nonce,agent 作用域内单调递增;0 非法(对齐 Mist「spend_nonce > 0」)。
    pub nonce: u64,
    /// 本笔金额,单位分。0 非法;溢出在闸内按预算阶段拒绝。
    pub amount_cents: u64,
    /// 商户 id(京东 SKU 店铺/商户标识,开放平台侧语义,闸不解析)。
    pub merchant_id: String,
    /// 消费类别(自由文本标签,落审计用;Mist 是哈希白名单,P0 不做白名单)。
    pub category: String,
    /// 备注(人类可读,落审计;空串 = 无备注)。
    pub memo: String,
}

impl SpendIntent {
    pub fn new(
        delegation_id: impl Into<String>,
        nonce: u64,
        amount_cents: u64,
        merchant_id: impl Into<String>,
        category: impl Into<String>,
        memo: impl Into<String>,
    ) -> Self {
        Self {
            delegation_id: delegation_id.into(),
            nonce,
            amount_cents,
            merchant_id: merchant_id.into(),
            category: category.into(),
            memo: memo.into(),
        }
    }

    /// 意图自身合法性(闸判定前先过这道,非法意图不必看委托状态)。
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.delegation_id.trim().is_empty() {
            return Err(CoreError::InvalidIntent("delegation_id 不能为空".into()));
        }
        if self.merchant_id.trim().is_empty() {
            return Err(CoreError::InvalidIntent("merchant_id 不能为空".into()));
        }
        if self.amount_cents == 0 {
            return Err(CoreError::InvalidIntent(
                "amount_cents 不能为 0(单位是分)".into(),
            ));
        }
        if self.nonce == 0 {
            return Err(CoreError::InvalidIntent(
                "nonce 不能为 0(防重放 nonce 从 1 起,对齐 mist-core 断言 7)".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SpendIntent {
        SpendIntent::new("d1", 1, 500, "jd:shop-1", "grocery", "早饭")
    }

    #[test]
    fn validate_accepts_well_formed_intent() {
        assert_eq!(sample().validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_zero_amount() {
        let i = SpendIntent {
            amount_cents: 0,
            ..sample()
        };
        assert!(matches!(i.validate(), Err(CoreError::InvalidIntent(_))));
    }

    #[test]
    fn validate_rejects_zero_nonce() {
        let i = SpendIntent {
            nonce: 0,
            ..sample()
        };
        assert!(matches!(i.validate(), Err(CoreError::InvalidIntent(_))));
    }

    #[test]
    fn validate_rejects_empty_fields() {
        for bad in [
            SpendIntent {
                delegation_id: "".into(),
                ..sample()
            },
            SpendIntent {
                merchant_id: " ".into(),
                ..sample()
            },
        ] {
            assert!(
                matches!(bad.validate(), Err(CoreError::InvalidIntent(_))),
                "应拒收空字段: {bad:?}"
            );
        }
    }

    #[test]
    fn serde_roundtrip() {
        let i = sample();
        let json = serde_json::to_string(&i).expect("序列化");
        let back: SpendIntent = serde_json::from_str(&json).expect("反序列化");
        assert_eq!(back, i);
        assert!(json.contains("\"amount_cents\":500"));
        assert!(json.contains("\"nonce\":1"));
    }
}
