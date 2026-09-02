//! 真实消费 fail-closed 护栏(铁律 2 的代码化)。
//!
//! 任何「真实消费」路径(真调 GLM 计费 API、真在京东下单、真走支付宝扣款)必须先过
//! [`check_real_spend`]:**全部**条件同时满足才放行,缺任何一项立即拒绝并清楚列出缺什么。
//!
//! - `WANNING_ALLOW_REAL_SPEND=1`:明示授权开关,值必须**恰好**是 `"1"`;
//! - `WANNING_GLM_KEY`:智谱 GLM 密钥;
//! - `WANNING_JD_APP_KEY` / `WANNING_JD_APP_SECRET` / `WANNING_JD_ACCESS_TOKEN`:
//!   京东开放平台凭证(具体字段名以 W-12 调研结论为准;账户开通前是最小占位清单)。
//!
//! 护栏只读**环境变量快照**([`EnvSnapshot`]),不碰进程 env——测试直接构造快照,
//! 「设/不设 env 两路」都能在单测里实证,且互不串扰。
//!
//! 密钥安全:[`RealSpendConfig`] 手写 `Debug`,输出永远打码;绝不把密钥打进日志。

use std::collections::BTreeMap;
use std::fmt;

/// 明示授权开关:真实消费必须显式设为 `1`,缺省(未设)即拒。
pub const ENV_ALLOW_REAL_SPEND: &str = "WANNING_ALLOW_REAL_SPEND";

/// 其余必填密钥,缺一即拒。
pub const REQUIRED_KEYS: &[&str] = &[
    "WANNING_GLM_KEY",
    "WANNING_JD_APP_KEY",
    "WANNING_JD_APP_SECRET",
    "WANNING_JD_ACCESS_TOKEN",
];

/// 护栏检查的环境变量快照。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvSnapshot(BTreeMap<String, String>);

impl EnvSnapshot {
    /// 从当前进程环境抓取护栏关心的 key(只复制这几个,不枚举整个环境)。
    pub fn from_process_env() -> Self {
        let mut snapshot = Self::default();
        for key in std::iter::once(ENV_ALLOW_REAL_SPEND).chain(REQUIRED_KEYS.iter().copied()) {
            if let Ok(value) = std::env::var(key) {
                snapshot.0.insert(key.to_string(), value);
            }
        }
        snapshot
    }

    pub fn insert(&mut self, key: &str, value: &str) {
        self.0.insert(key.to_string(), value.to_string());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }
}

/// 护栏通过后解锁的真实通道凭证。
///
/// **手写 `Debug`:密钥一律打码。** derive(Debug) 会把密钥原样打进日志/崩溃输出,
/// 这里宁可少信息也不泄密。
pub struct RealSpendConfig {
    pub glm_key: String,
    pub jd_app_key: String,
    pub jd_app_secret: String,
    pub jd_access_token: String,
}

impl fmt::Debug for RealSpendConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RealSpendConfig")
            .field("glm_key", &"***(已配置,打码)")
            .field("jd_app_key", &"***")
            .field("jd_app_secret", &"***")
            .field("jd_access_token", &"***")
            .finish()
    }
}

/// 护栏拒绝:列出全部未满足项(一次说清,不让用户改一项跑一次)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardDenied {
    pub reasons: Vec<String>,
}

impl fmt::Display for GuardDenied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "真实消费被拒(fail-closed):护栏条件未全部满足,共 {} 项:",
            self.reasons.len()
        )?;
        for (idx, reason) in self.reasons.iter().enumerate() {
            writeln!(f, "  {}. {}", idx + 1, reason)?;
        }
        write!(
            f,
            "真实消费路径保持关闭。以上每一项都齐了才会放行;任何一项缺失都被拒。测试请走 --dry-run(默认)与本地 mock。"
        )
    }
}

impl std::error::Error for GuardDenied {}

/// 护栏检查:全部条件满足 → 返回通道配置;缺任何一项 → [`GuardDenied`] 列全缺什么。
pub fn check_real_spend(env: &EnvSnapshot) -> Result<RealSpendConfig, GuardDenied> {
    let mut reasons = Vec::new();

    match env.get(ENV_ALLOW_REAL_SPEND) {
        None => reasons.push(format!(
            "{ENV_ALLOW_REAL_SPEND} 未设置(明示授权开关,必须显式设为 \"1\")"
        )),
        Some(value) if value != "1" => reasons.push(format!(
            "{ENV_ALLOW_REAL_SPEND} = {value:?},必须恰好是 \"1\""
        )),
        Some(_) => {}
    }

    for key in REQUIRED_KEYS {
        match env.get(key) {
            None => reasons.push(format!("缺少环境变量 {key}")),
            Some(value) if value.trim().is_empty() => {
                reasons.push(format!("环境变量 {key} 已设但为空/空白,视为缺失"))
            }
            Some(_) => {}
        }
    }

    if reasons.is_empty() {
        Ok(RealSpendConfig {
            glm_key: env.get("WANNING_GLM_KEY").unwrap_or_default().to_string(),
            jd_app_key: env
                .get("WANNING_JD_APP_KEY")
                .unwrap_or_default()
                .to_string(),
            jd_app_secret: env
                .get("WANNING_JD_APP_SECRET")
                .unwrap_or_default()
                .to_string(),
            jd_access_token: env
                .get("WANNING_JD_ACCESS_TOKEN")
                .unwrap_or_default()
                .to_string(),
        })
    } else {
        Err(GuardDenied { reasons })
    }
}

/// 便捷入口:对当前进程环境跑护栏。
pub fn real_spend_from_process_env() -> Result<RealSpendConfig, GuardDenied> {
    check_real_spend(&EnvSnapshot::from_process_env())
}

/// 护栏通过后的下一步:今晚真实通道**不接线**(京东账户未开通,GLM/adapter 待
/// W-08/W-10/W-11 接入)。继续 fail-closed——护栏过了也不代表能真花钱。
pub fn open_real_channel(_config: RealSpendConfig) -> Result<(), String> {
    Err(
        "真实消费通道尚未接线:京东账户未开通,GLM 决策回路与商城/支付 adapter 待 W-08/W-10/W-11 接入。\
         今晚一切真实消费都不可达,请用 --dry-run(默认)离线场景。"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_env() -> EnvSnapshot {
        let mut env = EnvSnapshot::default();
        env.insert(ENV_ALLOW_REAL_SPEND, "1");
        env.insert("WANNING_GLM_KEY", "glm-test-key");
        env.insert("WANNING_JD_APP_KEY", "jd-key");
        env.insert("WANNING_JD_APP_SECRET", "jd-secret");
        env.insert("WANNING_JD_ACCESS_TOKEN", "jd-token");
        env
    }

    #[test]
    fn denies_when_allow_flag_missing_and_lists_every_gap() {
        // 全空环境:一次说清全部 5 项(开关 + 4 个密钥)。
        let err = check_real_spend(&EnvSnapshot::default()).unwrap_err();
        assert_eq!(err.reasons.len(), 5, "一次列全,不让用户改一项跑一次");
        assert!(err.to_string().contains(ENV_ALLOW_REAL_SPEND));
        assert!(err.to_string().contains("fail-closed"));
        for key in REQUIRED_KEYS {
            assert!(err.to_string().contains(key), "必须点名缺少 {key}");
        }
    }

    #[test]
    fn denies_when_allow_flag_is_not_exactly_one() {
        for bad in ["0", "true", "yes", " 1", "1 "] {
            let mut env = full_env();
            env.insert(ENV_ALLOW_REAL_SPEND, bad);
            let err = check_real_spend(&env).unwrap_err();
            assert_eq!(err.reasons.len(), 1, "{bad:?} 必须被拒");
            assert!(err.reasons[0].contains("必须恰好是"));
        }
    }

    #[test]
    fn denies_on_each_single_missing_key() {
        let mut env = full_env();
        env.0.remove(ENV_ALLOW_REAL_SPEND);
        assert!(check_real_spend(&env).is_err(), "开关缺失必须拒");

        for key in REQUIRED_KEYS {
            let mut env = full_env();
            env.0.remove(*key);
            let err = check_real_spend(&env).unwrap_err();
            assert_eq!(err.reasons.len(), 1, "只缺 {key} 一项时报错应只提这一项");
            assert!(err.reasons[0].contains(key));
        }
    }

    #[test]
    fn denies_on_blank_value() {
        let mut env = full_env();
        env.insert("WANNING_GLM_KEY", "   ");
        let err = check_real_spend(&env).unwrap_err();
        assert!(err.reasons[0].contains("WANNING_GLM_KEY"), "{err}");
        assert!(err.reasons[0].contains("空白"));
    }

    #[test]
    fn passes_with_all_conditions_met_and_never_leaks_secrets_in_debug() {
        let config = check_real_spend(&full_env()).expect("条件齐全必须放行");
        assert_eq!(config.glm_key, "glm-test-key");
        assert_eq!(config.jd_access_token, "jd-token");

        let debug = format!("{config:?}");
        assert!(debug.contains("***"), "Debug 必须打码: {debug}");
        assert!(!debug.contains("glm-test-key"), "密钥绝不能进 Debug 输出");
        assert!(!debug.contains("jd-secret"), "密钥绝不能进 Debug 输出");
    }

    #[test]
    fn open_real_channel_still_refuses_tonight() {
        // 护栏过了也不等于能花钱:通道未接线,继续 fail-closed。
        let config = check_real_spend(&full_env()).expect("护栏过");
        let err = open_real_channel(config).unwrap_err();
        assert!(err.contains("尚未接线"), "{err}");
    }
}
