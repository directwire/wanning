//! 渠道签名管线(W-28 立槽位,W-50 官方规则填实):报文层参数规范化 + 签名/验签
//! 槽位——「钥匙到手即插」的半边。纪律:**本模块只做报文层,零网络零真实请求**;
//! 真密钥路径零接触(密钥经 env 注入位接入,TODO(账户开通后实签)),绝不写死在
//! 代码里、绝不落仓。
//!
//! # 口径(W-50 起 = 支付宝官方规则,零编造)
//!
//! W-28 时官方规范正文查不到(SPA 仅标题),规范化规则按**本地 mock 契约**钉死;
//! W-50 换路数从 `opendocs.alipay.com` 静态 .md 端点直抓到**正文全文**(抓取在档
//! 临时目录,引用 URL 如下),以下规则逐条改为官方口径,测试
//! `crates/wanning-demo/tests/signing.rs` 逐条钉死:
//!
//! - 《自行实现签名(适用于网关请求签名)》
//!   <https://opendocs.alipay.com/common/057k53>:①排除 `sign` 字段、值为空
//!   (含空白字符与 null)的参数、二进制数据;②按参数名 ASCII 升序排;③拼
//!   `key=value` 以 `&` 连接,**值原样不做 URL 转义**(URL 编码发生在签名之后
//!   的「拼接完整请求」步骤);④SHA256WithRSA(`sign_type=RSA2`,RSA 密钥
//!   ≥2048 位);⑤网关固定 `https://openapi.alipay.com/gateway.do`。
//! - 《自行实现验签(适用于异步通知/同步响应验签)》
//!   <https://opendocs.alipay.com/common/02mse7>:异步通知验签**剔除
//!   `sign` 与 `sign_type` 两参数**(生活号场景保留 `sign_type` 的例外与 Wanning
//!   无关)、其余参数 url_decode 后按参数名 ASCII 升序拼 `key=value` 以 `&`
//!   连接,用支付宝公钥验签。注意:**通知验签没有「空值剔除」规则**——与请求
//!   签名刻意不同,两条契约各自钉死。
//! - ANAI 实战交叉(W-50 双源):所有者侧 ANAI 真机过 APP 支付的生产后端
//!   支付模块(内部仓,W-50 调研在档)同一套 排序/拼接/RSA2/回调剔除
//!   `sign`+`sign_type` 规则,与官方文档逐条一致。
//!
//! 仍属「待实签」的只有一条:**服务器认不认**(真网关 + 真密钥,账户开通后)。
//! 京东 VOP 的签名算法公开面查不到(W-50 复核在档),见 `crate::jd` 与
//! 调研在档——不做猜测实现。
//!
//! # 请求签名契约([`canonical_query`],每条都有测试钉死)
//!
//! 1. 排除 `sign` 键(自指)与**值为空(含纯空白)的参数**;其余键(含
//!    `sign_type`,官方步骤1示例里它出现在待签名清单中)一律参与。
//! 2. 参数按 key 的**字节序**升序排序(不是字典序/本地化序);同名 key 出现两次
//!    = 签名串有歧义 → [`CanonicalError::DuplicateKey`] fail-closed 拒收。
//! 3. 空 key 拒收([`CanonicalError::EmptyKey`])。
//! 4. 值**原样字节**进签名串,不做 URL 转义(`&`、`=`、空格、中文逐字节原样)。
//! 5. 拼接 = `key=value` 以 `&` 连接;排除后参数表为空 → 空串(不拒,签名语义上成立)。
//! 6. 官方语境的 null 参数在本仓 `(&str,&str)` 参数面不存在——可选参数由调用方
//!    **整体省略**表达(adapter 纪律),规范化层只处理空串/空白。
//!
//! # 通知验签契约([`canonical_notify_string`])
//!
//! 剔除 `sign` 与 `sign_type` 后,其余参数(含空值)按 key 字节序拼
//! `key=value` 以 `&` 连接;重复/空 key 同样 fail-closed 拒收。
//!
//! # 签名/验签槽位
//!
//! [`MessageSigner`](商户侧,签请求报文)与 [`SignatureVerifier`](回调侧,验
//! 回调/响应签名——对应 [`crate::channel::apply_pay_notify`] 的「验签不过绝不
//! 入账」前置门)。本仓不自带实现:测试面用 dev-only 的 `rsa`/`sha2`/`rand`(见
//! `Cargo.toml [dev-dependencies]`,运行时依赖树零新增)现场生成 2048 位测试
//! 密钥对跑 sign/verify 往返;真实现 = 账户开通后接支付宝商户私钥(env 注入,
//! TODO(账户开通后实签))。

use std::collections::BTreeSet;
use std::fmt;

/// 请求签名不参与的键(`sign` = 签名自身,自指)。`sign_type` 官方示例里参与
/// 签名([公开文档直核 057k53 步骤1]),故不在此列。
const EXCLUDED_SIGNING_KEYS: [&str; 1] = ["sign"];

/// 异步通知验签不参与的键(`sign` + `sign_type`,官方 02mse7 第2步 [公开文档直核];
/// 生活号保留 `sign_type` 的例外与 Wanning 无关——Wanning 不是生活号)。
const EXCLUDED_NOTIFY_KEYS: [&str; 2] = ["sign", "sign_type"];

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
    /// 签名槽位还没接真密钥:账户开通后经 env 注入位接入(TODO(账户开通后实签)),
    /// 绝不写死。
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

/// 值是否按官方「值为空」规则剔除(含空白字符;[公开文档直核 057k53 步骤1])。
fn official_empty(value: &str) -> bool {
    value.trim().is_empty()
}

/// 请求参数规范化成待签名字符串(**支付宝官方规则**,见模块文档逐条来源)。
///
/// 调用方(adapter)从结构化请求字段派生参数,不收裸 Map——重复 key 在这里
/// fail-closed 拒收而不是静默吞掉。
pub fn canonical_query(params: &[(&str, &str)]) -> Result<String, CanonicalError> {
    canonical_with_exclusions(params, &EXCLUDED_SIGNING_KEYS, true)
}

/// 异步通知参数规范化成待验签字符串(**支付宝官方规则**,剔除 `sign`+`sign_type`、
/// 保留空值;见模块文档《自行实现验签》来源)。
pub fn canonical_notify_string(params: &[(&str, &str)]) -> Result<String, CanonicalError> {
    canonical_with_exclusions(params, &EXCLUDED_NOTIFY_KEYS, false)
}

/// 共同实现:校验(空 key/重复 key fail-closed)→ 剔除 → 字节序排序 →
/// `key=value` 以 `&` 连接(值原样,零转义)。
///
/// `drop_empty_values` 两条契约刻意不同:请求签名按官方规则剔除空值;通知验签
/// 官方规则没有空值剔除(「凡是通知返回的参数皆是待验签的参数」),原样保留。
fn canonical_with_exclusions(
    params: &[(&str, &str)],
    excluded: &[&str],
    drop_empty_values: bool,
) -> Result<String, CanonicalError> {
    let mut seen = BTreeSet::new();
    for (key, _) in params {
        if key.is_empty() {
            return Err(CanonicalError::EmptyKey);
        }
        if !seen.insert(*key) {
            return Err(CanonicalError::DuplicateKey((*key).to_string()));
        }
    }
    // 先剔除再排序拼接:剔除若放在拼接循环里,循环已 push 的分隔符会留下尾随
    // `&`(测试 sign_key_never_enters_canonical_string 钉的就是这个)。
    let mut sorted: Vec<(&str, &str)> = params
        .iter()
        .copied()
        .filter(|(key, _)| !excluded.contains(key))
        .filter(|(_, value)| !(drop_empty_values && official_empty(value)))
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
/// 后接上——密钥经 env 注入位读取(TODO(账户开通后实签)),绝不写死在代码或仓里。
pub trait MessageSigner {
    /// 算法名(`RSA2` = SHA256WithRSA [公开文档直核 057k53 步骤3]),落审计/日志用。
    fn alg(&self) -> &'static str;
    /// 签一个规范化签名串;槽位未接密钥或密钥不可用时 fail-closed 报错。
    fn sign(&self, canonical: &str) -> Result<Vec<u8>, SigningError>;
}

/// 回调侧验签槽位:验异步通知/同步响应签名,**不过 = 拒**。对应
/// [`crate::channel::apply_pay_notify`] 的「验签不过的报文绝不入账」前置门
/// (fail-closed,与 W-13 调研结论一致)。
pub trait SignatureVerifier {
    /// 验不过返回 `false`(含签名长度不对等垃圾输入——返回 false,不 panic)。
    fn verify(&self, canonical: &str, signature: &[u8]) -> bool;
}
