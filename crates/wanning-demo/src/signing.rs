//! 渠道签名管线(W-28):报文层参数规范化 + 签名/验签槽位——「钥匙到手即插」的
//! 半边。纪律:**本模块只做报文层,零网络零真实请求**;真密钥路径零接触(密钥
//! 经 env 注入位接入,TODO(账户开通后)),绝不写死在代码里、绝不落仓。
//!
//! # 口径(零编造)
//!
//! **官方签名规范正文本次调研查不到**,故下面的规范化规则是**本地 mock 契约**,
//! 逐条钉死在本模块文档与 `tests/signing.rs` 测试里,绝不冒充支付宝
//! 官方规则;账户开通后逐条与官方《接口签名规范》核对,不一致处改这里并回归
//! 测试(TODO(账户开通后联调))。调研依据:
//!
//! - W-13(调研在档):回调验签必须落地后才允许
//!   [`crate::channel::apply_pay_notify`] 入账,「验签细节待人工登录文档后核对」。
//! - 2026-09-02 本任务直核补查:支付宝开放文档站 `opendocs.alipay.com/open/291/105971`
//!   与老站 `docs.open.alipay.com/291/106103`(301 → 同一 SPA)均**仅标题无正文**
//!   (JS 渲染);京东 VOP 文档中心 `vop.jd.com/doc/api` 同为 SPA 无正文 →
//!   **京东 VOP 签名算法细节查不到,待人工,不做猜测实现**(见调研记录,在档)。
//!
//! # 本地契约(每条都有测试钉死)
//!
//! 1. 参数按 key 的**字节序**升序排序(不是字典序/本地化序);同名 key 出现两次
//!    = 签名串有歧义 → [`CanonicalError::DuplicateKey`] fail-closed 拒收。
//! 2. 空 key 拒收([`CanonicalError::EmptyKey`])。
//! 3. `sign` 键不参与签名(自指);**其余键一律参与**——`sign_type` 是否被官方
//!    排除、空值是否跳过,查不到正文 → 本地契约选「参与」,联调时核对。
//! 4. 值**原样字节**进签名串,不做 URL 转义(官方是否转义查不到 → 本地契约选
//!    不转义;含 `&`、`=`、空格、中文的值逐字节原样)。
//! 5. 拼接 = `key=value` 以 `&` 连接;空参数表 → 空串(不拒,签名语义上成立)。
//! 6. 签名算法名 [`MessageSigner::alg`] = `sha256withrsa`(本地契约命名,待联调
//!    核对官方算法名)。
//!
//! # 签名/验签槽位
//!
//! [`MessageSigner`](商户侧,签请求报文)与 [`SignatureVerifier`](回调侧,验
//! 回调签名——对应 [`crate::channel::apply_pay_notify`] 的「验签不过绝不入账」
//! 前置门)。本仓不自带实现:测试面用 dev-only 的 `rsa`/`sha2`/`rand`(见
//! `Cargo.toml [dev-dependencies]`,运行时依赖树零新增)现场生成 2048 位测试
//! 密钥对跑 sign/verify 往返;真实现 = 账户开通后接支付宝商户私钥(env 注入)。

use std::collections::BTreeSet;
use std::fmt;

/// 不参与签名的键(`sign` = 签名自身,自指)。其余键(含 `sign_type`)是否被
/// 官方排除查不到正文 → 本地契约一律参与,联调时核对。
const EXCLUDED_SIGNING_KEYS: [&str; 1] = ["sign"];

/// 参数规范化失败(fail-closed:宁可拒签,绝不产出有歧义的签名串)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalError {
    /// 同名参数出现两次:签名串会有歧义,拒收(adapter 层不会产生,保底)。
    DuplicateKey(String),
    /// 空 key:规范化无从谈起,拒收。
    EmptyKey,
}

impl fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CanonicalError::DuplicateKey(key) => write!(
                f,
                "签名参数重复:key `{key}` 出现两次,签名串有歧义,fail-closed 拒收"
            ),
            CanonicalError::EmptyKey => {
                write!(f, "签名参数含空 key,规范化无从谈起,fail-closed 拒收")
            }
        }
    }
}

impl std::error::Error for CanonicalError {}

/// 签名槽位尚未接真密钥/密钥不可用时的失败(账户开通前的唯一合法状态)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigningError {
    /// 签名槽位还没接真密钥:账户开通后经 env 注入位接入(TODO),绝不写死。
    NoKeyMaterial,
    /// 注入的密钥材料不可用(格式/解密失败),带原因。
    KeyRejected(String),
}

impl fmt::Display for SigningError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SigningError::NoKeyMaterial => write!(
                f,
                "签名槽位未接密钥:账户开通后经 env 注入位接入真商户私钥(绝不写死/落仓)"
            ),
            SigningError::KeyRejected(reason) => {
                write!(f, "注入的签名密钥不可用,拒绝签发:{reason}")
            }
        }
    }
}

impl std::error::Error for SigningError {}

/// 把结构化参数规范化成待签名字符串(本地契约,见模块文档逐条规则)。
///
/// 调用方(adapter)从结构化请求字段派生参数,不收裸 Map——重复 key 在这里
/// fail-closed 拒收而不是静默吞掉。
pub fn canonical_query(params: &[(&str, &str)]) -> Result<String, CanonicalError> {
    let mut seen = BTreeSet::new();
    for (key, _) in params {
        if key.is_empty() {
            return Err(CanonicalError::EmptyKey);
        }
        if !seen.insert(*key) {
            return Err(CanonicalError::DuplicateKey((*key).to_string()));
        }
    }
    // 先排除 `sign` 再排序拼接:排除若放在拼接循环里,循环已 push 的分隔符会
    // 留下尾随 `&`(测试 sign_key_never_enters_canonical_string 钉的就是这个)。
    let mut sorted: Vec<(&str, &str)> = params
        .iter()
        .copied()
        .filter(|(key, _)| !EXCLUDED_SIGNING_KEYS.contains(key))
        .collect();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut out = String::new();
    for (index, (key, value)) in sorted.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        out.push_str(key);
        out.push('=');
        out.push_str(value);
    }
    Ok(out)
}

/// 商户侧签名槽位:把规范化签名串签成字节。真实现(支付宝商户私钥)账户开通
/// 后接上——密钥经 env 注入位读取(TODO(账户开通后)),绝不写死在代码或仓里。
pub trait MessageSigner {
    /// 本地契约的算法名(如 `sha256withrsa`),落审计/日志用;真实算法名待联调核对。
    fn alg(&self) -> &'static str;
    /// 签一个规范化签名串;槽位未接密钥或密钥不可用时 fail-closed 报错。
    fn sign(&self, canonical: &str) -> Result<Vec<u8>, SigningError>;
}

/// 回调侧验签槽位:验回调签名,**不过 = 拒**。对应 [`crate::channel::apply_pay_notify`]
/// 的「验签不过的报文绝不入账」前置门(fail-closed,与 W-13 调研结论一致)。
pub trait SignatureVerifier {
    /// 验不过返回 `false`(含签名长度不对等垃圾输入——返回 false,不 panic)。
    fn verify(&self, canonical: &str, signature: &[u8]) -> bool;
}
