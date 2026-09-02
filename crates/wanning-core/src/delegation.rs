//! 授权(Delegation):用户(钱的主人)→ agent 的一次授权。
//!
//! 一次授权 = 一份预算上限 + 一个有效期 + 一个 nonce 作用域 (+ 可选支出策略)。
//! 金额一律 u64 **分**(cents),全程禁浮点——这是钱。
//!
//! 语义对齐 mist-core 的 `Delegation`(agent/owner/总上限/not_before/expires_at),
//! 差异点:Mist 的 `nonce` 是授权唯一编号(撤销锚点),Wanning 的 nonce 语义放在
//! **意图侧**([`crate::intent::SpendIntent::nonce`],防重放),委托侧用 `nonce_scope`
//! 表达「agent 作用域」——对齐 Mist「`spend_nonce` agent 作用域单调递增」。
//!
//! 支出策略(W-27):[`Delegation::policy`] 是总预算之外的确定性策略维度
//! (速率/类目/商户/时段),**挂在委托上随注册落审计**——WAL 注册记录自带完整
//! 委托含策略,回放零新增记录类型;`Default` = 无附加策略,序列化不落字段,
//! 既有 WAL 行逐字节不漂移。

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::policy::SpendPolicy;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delegation {
    /// 委托唯一 id(闸内主键,审计日志据此关联)。
    pub id: String,
    /// 授权者(钱的主人,真实姓名/账户主体不出现在闸内,只用代号)。
    pub owner: String,
    /// 被授权的 agent(哪个 agent 实例可以花这笔钱)。
    pub agent: String,
    /// 总预算上限,单位分。Σ(成功扣减) ≤ budget_cap_cents。
    pub budget_cap_cents: u64,
    /// 生效时刻(Unix 秒,含)。now < valid_from → 拒。
    pub valid_from: u64,
    /// 失效时刻(Unix 秒,**不含**)。now ≥ valid_until → 拒(fail-closed:恰在边界按过期处理)。
    pub valid_until: u64,
    /// nonce 作用域:同一作用域内 nonce 不得重复。通常取 agent 标识,
    /// 使同一 agent 的多个委托共享一套单调 nonce(对齐 Mist「agent 作用域」)。
    pub nonce_scope: String,
    /// 支出策略(速率/类目/商户/时段)。缺省 = 无附加策略;反序列化缺字段 =
    /// 缺省(旧记录可读);序列化时缺省策略不落字段(既有 WAL 行不漂移)。
    #[serde(default, skip_serializing_if = "SpendPolicy::is_empty")]
    pub policy: SpendPolicy,
}

impl Delegation {
    /// 直接构造字段。**合法性由 [`Delegation::validate`] 把关**,而 validate 由
    /// `Gate::register_delegation`(信任边界)强制调用——构造器不拦,闸拦。
    pub fn new(
        id: impl Into<String>,
        owner: impl Into<String>,
        agent: impl Into<String>,
        budget_cap_cents: u64,
        valid_from: u64,
        valid_until: u64,
        nonce_scope: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            owner: owner.into(),
            agent: agent.into(),
            budget_cap_cents,
            valid_from,
            valid_until,
            nonce_scope: nonce_scope.into(),
            policy: SpendPolicy::default(),
        }
    }

    /// 附上支出策略(builder 链式;策略合法性同样由注册时的 `validate` 把关)。
    pub fn with_policy(mut self, policy: SpendPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// 注册前校验(fail-closed:任何一项不过就拒收)。
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.id.trim().is_empty() {
            return Err(CoreError::InvalidDelegation("id 不能为空".into()));
        }
        if self.owner.trim().is_empty() {
            return Err(CoreError::InvalidDelegation("owner 不能为空".into()));
        }
        if self.agent.trim().is_empty() {
            return Err(CoreError::InvalidDelegation("agent 不能为空".into()));
        }
        if self.nonce_scope.trim().is_empty() {
            return Err(CoreError::InvalidDelegation("nonce_scope 不能为空".into()));
        }
        if self.budget_cap_cents == 0 {
            // 预算为 0 几乎必然是配置错误(单位写错/漏填),fail-closed 拒收,
            // 不允许「注册成功但什么也买不了」的假授权存在。
            return Err(CoreError::InvalidDelegation(
                "budget_cap_cents 不能为 0(单位是分,¥10 = 1000)".into(),
            ));
        }
        if self.valid_until <= self.valid_from {
            return Err(CoreError::InvalidDelegation(format!(
                "有效期倒挂:valid_until({}) 必须 > valid_from({})",
                self.valid_until, self.valid_from
            )));
        }
        // 支出策略(W-27)与委托同一信任边界:坏策略拒收整份委托。
        self.policy.validate()?;
        Ok(())
    }

    /// 是否尚未生效。`now < valid_from` → 尚未生效。
    pub fn not_yet_valid(&self, now: u64) -> bool {
        now < self.valid_from
    }

    /// 是否已过期。**恰在 `valid_until` 时刻按过期处理**(fail-closed,半开区间)。
    pub fn is_expired(&self, now: u64) -> bool {
        now >= self.valid_until
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Delegation {
        // ¥10 总预算,有效期 [1000, 2000) 秒。
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

    #[test]
    fn validate_accepts_well_formed_delegation() {
        assert_eq!(sample().validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_empty_fields() {
        for bad in [
            Delegation {
                id: " ".into(),
                ..sample()
            },
            Delegation {
                owner: "".into(),
                ..sample()
            },
            Delegation {
                agent: "".into(),
                ..sample()
            },
            Delegation {
                nonce_scope: "".into(),
                ..sample()
            },
        ] {
            assert!(
                matches!(bad.validate(), Err(CoreError::InvalidDelegation(_))),
                "应拒收空字段: {bad:?}"
            );
        }
    }

    #[test]
    fn validate_rejects_zero_budget() {
        let d = Delegation {
            budget_cap_cents: 0,
            ..sample()
        };
        assert!(matches!(d.validate(), Err(CoreError::InvalidDelegation(_))));
    }

    #[test]
    fn validate_rejects_inverted_window() {
        let d = Delegation::new("d1", "boss", "agent", 1000, 2000, 2000, "s1");
        assert!(matches!(d.validate(), Err(CoreError::InvalidDelegation(_))));
        let d = Delegation::new("d1", "boss", "agent", 1000, 3000, 2000, "s1");
        assert!(matches!(d.validate(), Err(CoreError::InvalidDelegation(_))));
    }

    #[test]
    fn expiry_boundary_is_half_open_fail_closed() {
        let d = sample();
        assert!(!d.is_expired(1999), "valid_until 前一秒仍在有效期内");
        assert!(d.is_expired(2000), "恰在 valid_until 时刻必须按过期处理");
        assert!(d.is_expired(5000), "过期之后仍是过期");
    }

    #[test]
    fn not_yet_valid_boundary() {
        let d = sample();
        assert!(d.not_yet_valid(999), "valid_from 前一秒尚未生效");
        assert!(!d.not_yet_valid(1000), "恰在 valid_from 时刻已生效(含端点)");
    }

    #[test]
    fn serde_roundtrip() {
        let d = sample();
        let json = serde_json::to_string(&d).expect("序列化");
        let back: Delegation = serde_json::from_str(&json).expect("反序列化");
        assert_eq!(back, d);
        // 字段名稳定,WAL/对外审计依赖这个形状。
        assert!(json.contains("\"budget_cap_cents\":1000"));
        assert!(json.contains("\"nonce_scope\":\"agent:claude-code\""));
    }
}
