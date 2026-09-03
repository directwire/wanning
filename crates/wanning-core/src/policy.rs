//! 支出策略层(W-27):总预算之外的确定性策略维度。
//!
//! 闸的既有判定面只有「总预算」一道硬约束;本模块在它之外补四个**确定性**
//! 维度,全部挂在**委托**上([`crate::delegation::Delegation::policy`],随注册
//! 落审计——WAL 注册记录自带完整委托含策略,回放零新增记录类型):
//!
//! | 维度 | 字段 | 语义 | 拒绝原因 |
//! |---|---|---|---|
//! | 速率限制 | [`SpendPolicy::velocity`] | 滑动窗口内至多 n 笔**成功放行** | `rate_limited` |
//! | 类目预算 | [`SpendPolicy::category_caps_cents`] | 每类目独立上限;未知类目 fail-open | `over_category_budget` |
//! | 商户名单 | [`SpendPolicy::merchant_allow`] / [`SpendPolicy::merchant_deny`] | deny 优先;allow 空 = 不设白名单 | `merchant_denied` / `merchant_not_allowed` |
//! | 禁止时段 | [`SpendPolicy::quiet_windows`] | `[from_ts, until_ts)` 绝对 Unix 秒 | `quiet_hours` |
//!
//! 阶段 5 内部先到先拒的顺序(与 [`crate::gate`] 一致):
//! **商户名单 → 禁止时段 → 速率 → 类目 → 总预算**。
//!
//! 三条刻意决策(理由落决策记录(W-27 条)):
//!
//! 1. **类目未知 fail-open**:类目不在表内 = 无类目预算,只受总预算管。理由:
//!    fail-closed 的「未知类目一律拒」会把「委托没写类目策略」误伤成「全部拒」,
//!    与「策略缺省 = 不限制」的委托语义矛盾;而总预算这道硬闸永不 fail-open。
//! 2. **类目上限 0 = 禁该类目**(合法):与总预算 0 拒收口径不同——总预算 0 使
//!    整份委托作废(几乎必然是单位写错),类目 0 只关掉一个类目,是刻意的禁止表达。
//! 3. **速率窗口按委托计**,与预算同口径:同一 agent 的多份委托 = 多份独立预算
//!    与独立窗口(委托模型的既有语义,不是新洞)。
//!
//! 时间语义:全部 u64 Unix 秒,与 [`crate::clock`] 同纲;速率窗口的滑动边界
//! 「恰在 `t + window` 时刻更早一笔不再计入」与过期语义同形(半开,fail-closed
//! 方向一致);禁止时段刻意**不做时区换算**——「每天 23 点」在 Unix 秒语义下
//! 无定义,只表达绝对窗口。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// 速率限制:滑动窗口内至多 [`VelocityLimit::max_spends`] 笔**成功放行**。
///
/// 只有成功放行计入(拒绝不耗号也不占窗口槽);窗口按**委托**计。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VelocityLimit {
    /// 窗口内允许的最大成功放行笔数(≥ 1)。
    pub max_spends: u32,
    /// 滑动窗口长度(秒,≥ 1)。一笔在 `t + window_secs` 时刻起不再计入
    /// (半开:恰在窗口结束时刻已释放,与过期语义同形)。
    pub window_secs: u64,
}

/// 禁止时段(quiet hours):`[from_ts, until_ts)` 绝对 Unix 秒,半开区间。
///
/// 刻意不做时区/每日时段换算——「每天 23 点」在 Unix 秒语义下无定义,只有绝对窗口。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuietWindow {
    /// 禁止开始时刻(含)。
    pub from_ts: u64,
    /// 禁止结束时刻(**不含**,恰在此时刻已放行,与有效期口径一致)。
    pub until_ts: u64,
}

/// 支出策略:总预算之外的确定性策略维度,挂在委托上随注册落审计。
///
/// `Default` = 无附加策略——行为与本模块引入之前完全一致(序列化也不落
/// `policy` 字段,既有 WAL 行逐字节不漂移;四卖点场景回归锁定)。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpendPolicy {
    /// 速率限制(None = 不限)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<VelocityLimit>,
    /// 类目 → 上限(分)。不在表内的类目 = 无类目预算,fail-open(总预算仍管);
    /// 上限 0 = 禁该类目。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub category_caps_cents: BTreeMap<String, u64>,
    /// 商户白名单(空 = 不设白名单;非空时未列商户一律拒)。
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub merchant_allow: BTreeSet<String>,
    /// 商户黑名单(deny 优先:同时在两份名单 = 拒)。
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub merchant_deny: BTreeSet<String>,
    /// 禁止时段(绝对 Unix 秒,半开)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quiet_windows: Vec<QuietWindow>,
}

impl SpendPolicy {
    /// 是否为缺省策略(所有维度均未启用)。
    pub fn is_empty(&self) -> bool {
        self.velocity.is_none()
            && self.category_caps_cents.is_empty()
            && self.merchant_allow.is_empty()
            && self.merchant_deny.is_empty()
            && self.quiet_windows.is_empty()
    }

    /// 注册前校验(fail-closed:任何一项不过就拒收,由
    /// `Gate::register_delegation` 强制调用)。
    pub fn validate(&self) -> Result<(), CoreError> {
        if let Some(v) = &self.velocity {
            if v.max_spends == 0 {
                return Err(CoreError::InvalidDelegation(
                    "velocity.max_spends 不能为 0(拒绝本来就不计数;0 笔窗口等价于禁一切,应为配置错误)"
                        .into(),
                ));
            }
            if v.window_secs == 0 {
                return Err(CoreError::InvalidDelegation(
                    "velocity.window_secs 不能为 0(零长窗口等价于不限速,应为配置错误)".into(),
                ));
            }
        }
        for key in self.category_caps_cents.keys() {
            if key.trim().is_empty() {
                return Err(CoreError::InvalidDelegation(
                    "类目键不能为空白(空白类目的意图按「无类目」fail-open,设了也不生效)".into(),
                ));
            }
        }
        for (name, list) in [
            ("merchant_allow", &self.merchant_allow),
            ("merchant_deny", &self.merchant_deny),
        ] {
            for entry in list {
                if entry.trim().is_empty() {
                    return Err(CoreError::InvalidDelegation(format!(
                        "{name} 名有条目为空白(商户 id 精确匹配,空白条目是配置错误)"
                    )));
                }
            }
        }
        for w in &self.quiet_windows {
            if w.until_ts <= w.from_ts {
                return Err(CoreError::InvalidDelegation(format!(
                    "禁止时段倒挂或零长:until_ts({}) 必须 > from_ts({})",
                    w.until_ts, w.from_ts
                )));
            }
        }
        Ok(())
    }

    /// 商户是否被名单拒绝(deny 优先;allow 空 = 不设白名单)。
    /// 返回 `Some(原因)` 表示拒,`None` 表示名单不拦。
    pub fn merchant_verdict(&self, merchant_id: &str) -> Option<MerchantVerdict> {
        if self.merchant_deny.contains(merchant_id) {
            return Some(MerchantVerdict::Denied);
        }
        if !self.merchant_allow.is_empty() && !self.merchant_allow.contains(merchant_id) {
            return Some(MerchantVerdict::NotAllowed);
        }
        None
    }

    /// `now` 是否落在任一禁止时段内(半开 `[from_ts, until_ts)`)。
    pub fn is_quiet(&self, now: u64) -> bool {
        self.quiet_windows
            .iter()
            .any(|w| now >= w.from_ts && now < w.until_ts)
    }
}

/// 商户名单的拒绝类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MerchantVerdict {
    /// 在黑名单(或与白名单冲突,deny 优先)。
    Denied,
    /// 不在白名单(白名单非空时)。
    NotAllowed,
}

/// 每委托的策略运行时状态:随 commit 演化、随回放重建。
///
/// 只在对应策略维度启用时才记录对应数据(未启用速率不记时刻,未设上限的类目
/// 不记账),因此缺省策略下本结构恒空、零额外内存。时刻全量保留、不做剪枝:
/// 与账本同阶(每笔成功放行一个 u64,审计本身也在按行增长),剪枝会破坏
/// 「访问器 = 全量成功时刻」的直读语义。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyState {
    /// 成功放行时刻(仅启用速率时记录;按时间升序)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub velocity_stamps: Vec<u64>,
    /// 类目累计消费(仅设了上限的类目记账)。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub category_spent_cents: BTreeMap<String, u64>,
}

impl PolicyState {
    /// `now` 时刻窗口内的成功放行笔数。
    ///
    /// 一笔在 `t` 时刻放行,当 `now - t < window_secs` 时计入;恰在
    /// `t + window_secs` 时刻不再计入(半开,与过期语义同形)。
    pub fn in_window_count(&self, now: u64, window_secs: u64) -> usize {
        self.velocity_stamps
            .iter()
            .filter(|&&t| now.saturating_sub(t) < window_secs)
            .count()
    }

    /// 记录一笔成功放行的时刻(**仅当该委托启用了速率限制才调用**——未启用
    /// 速率的委托不产生任何窗口时刻,`velocity_stamps` 恒空)。
    pub fn record_velocity_stamp(&mut self, now: u64) {
        self.velocity_stamps.push(now);
    }

    /// 记一笔类目消费(**仅当该类目设了上限才调用**——未设上限的类目没有
    /// 「类目预算」可言,不记账)。
    ///
    /// # 前提
    /// `amount_cents` 的溢出必须由调用方先经 `checked_add` 判过(闸在阶段 5
    /// 里先验溢出再 commit);本方法不重复判,溢出 panic 会在回放与实时两侧
    /// 同位触发,不破坏「实时态 == 回放态」。
    pub fn record_category_spend(&mut self, category: &str, amount_cents: u64) {
        let entry = self
            .category_spent_cents
            .entry(category.to_string())
            .or_insert(0);
        *entry = entry
            .checked_add(amount_cents)
            .expect("调用方必须先 checked_add 判过溢出");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_empty_and_valid() {
        let p = SpendPolicy::default();
        assert!(p.is_empty());
        assert_eq!(p.validate(), Ok(()));
        assert_eq!(p.merchant_verdict("m1"), None);
        assert!(!p.is_quiet(0));
    }

    #[test]
    fn serde_roundtrip_and_skip_empty_fields() {
        let p = SpendPolicy {
            velocity: Some(VelocityLimit {
                max_spends: 3,
                window_secs: 60,
            }),
            ..SpendPolicy::default()
        };
        let json = serde_json::to_string(&p).expect("序列化");
        // 未启用的维度不落字段(审计/WAL 记录保持紧凑)。
        assert!(json.contains("\"velocity\""));
        assert!(!json.contains("category_caps_cents"));
        assert!(!json.contains("merchant_allow"));
        let back: SpendPolicy = serde_json::from_str(&json).expect("反序列化");
        assert_eq!(back, p);
        // 缺字段 = 缺省维度(旧记录可读)。
        let old: SpendPolicy = serde_json::from_str("{}").expect("空对象 = 缺省策略");
        assert_eq!(old, SpendPolicy::default());
    }

    #[test]
    fn validate_rejects_bad_velocity_and_windows_and_blank_keys() {
        let bad = SpendPolicy {
            velocity: Some(VelocityLimit {
                max_spends: 0,
                window_secs: 60,
            }),
            ..SpendPolicy::default()
        };
        assert!(matches!(
            bad.validate(),
            Err(CoreError::InvalidDelegation(_))
        ));
        let bad = SpendPolicy {
            velocity: Some(VelocityLimit {
                max_spends: 1,
                window_secs: 0,
            }),
            ..SpendPolicy::default()
        };
        assert!(matches!(
            bad.validate(),
            Err(CoreError::InvalidDelegation(_))
        ));
        let bad = SpendPolicy {
            quiet_windows: vec![QuietWindow {
                from_ts: 100,
                until_ts: 100,
            }],
            ..SpendPolicy::default()
        };
        assert!(matches!(
            bad.validate(),
            Err(CoreError::InvalidDelegation(_))
        ));
        let bad = SpendPolicy {
            merchant_deny: BTreeSet::from(["  ".to_string()]),
            ..SpendPolicy::default()
        };
        assert!(matches!(
            bad.validate(),
            Err(CoreError::InvalidDelegation(_))
        ));
        let bad = SpendPolicy {
            category_caps_cents: BTreeMap::from([("".to_string(), 100)]),
            ..SpendPolicy::default()
        };
        assert!(matches!(
            bad.validate(),
            Err(CoreError::InvalidDelegation(_))
        ));
    }

    #[test]
    fn merchant_verdict_deny_wins_and_allow_gates() {
        let p = SpendPolicy {
            merchant_allow: BTreeSet::from(["m1".to_string()]),
            merchant_deny: BTreeSet::from(["m1".to_string(), "m3".to_string()]),
            ..SpendPolicy::default()
        };
        assert_eq!(p.merchant_verdict("m1"), Some(MerchantVerdict::Denied));
        assert_eq!(p.merchant_verdict("m3"), Some(MerchantVerdict::Denied));
        assert_eq!(p.merchant_verdict("m2"), Some(MerchantVerdict::NotAllowed));
        // allow 空 = 不设白名单。
        let p = SpendPolicy {
            merchant_deny: BTreeSet::from(["m1".to_string()]),
            ..SpendPolicy::default()
        };
        assert_eq!(p.merchant_verdict("m2"), None);
    }

    #[test]
    fn is_quiet_is_half_open() {
        let p = SpendPolicy {
            quiet_windows: vec![QuietWindow {
                from_ts: 100,
                until_ts: 200,
            }],
            ..SpendPolicy::default()
        };
        assert!(!p.is_quiet(99));
        assert!(p.is_quiet(100));
        assert!(p.is_quiet(199));
        assert!(!p.is_quiet(200), "恰在 until_ts 已出窗口");
    }

    #[test]
    fn policy_state_window_count_is_half_open() {
        let mut s = PolicyState::default();
        s.record_velocity_stamp(1000);
        s.record_velocity_stamp(1050);
        assert_eq!(
            s.in_window_count(1099, 100),
            2,
            "两笔都在窗口内(1099-1000=99 < 100)"
        );
        assert_eq!(
            s.in_window_count(1100, 100),
            1,
            "t=1000 恰在 1000+100=1100 时刻滑出(半开:now-t < window 才计入)"
        );
        assert_eq!(
            s.in_window_count(1149, 100),
            1,
            "t=1050 还在窗口内(1149-1050=99)"
        );
        assert_eq!(
            s.in_window_count(1150, 100),
            0,
            "t=1050 恰在 1050+100=1150 时刻滑出,窗口清空"
        );
        // 类目只在设了上限时记账(闸侧纪律:未设上限的类目不产生台账),
        // 且类目记账不产生速率时刻——两个维度各自独立记录。
        s.record_category_spend("grocery", 300);
        assert_eq!(s.category_spent_cents.get("grocery"), Some(&300));
        assert_eq!(s.velocity_stamps.len(), 2, "类目记账不影响速率窗口时刻");
    }
}
