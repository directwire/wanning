//! 支付宝 adapter:[`AlipayBackend`](PaymentChannel trait 的第一个实现)。
//!
//! 两条路径,共用一套 trait 与幂等回调管线:
//! - **mock 路径**(`new_mock`):报文是本地 mock 契约(wanning-demo 自定义字段),
//!   只用于把传输/错误映射/幂等/解析的管线测起来,测试打本地 mock server,零外网;
//! - **真实路径**(`from_snapshot_real`):报文 = **支付宝官方模板**(W-50 公开文档
//!   直核填实,来源见 [`build_trade_pay_request`] 与 [`verify_pay_notify`] 文档),
//!   网关固定官方地址,默认可被 `WANNING_ALIPAY_ENDPOINT` 覆盖(测试打本地替身)。
//!   「服务器认不认」如实标 **待实签**——真网关 + 真密钥那一步留账户开通后。
//!
//! 合规边界(docs/compliance-redlines.md)——**先读这里再动这个文件**:
//! - 免密代扣的语义是「**协议内扣款**」:用户事先与收款方签约(代扣/委托扣款协议),
//!   扣款发生在协议约定的额度与场景之内。**它不是无协议的裸转账**,Wanning 绝不
//!   实现、也不封装任何绕过签约协议的转账能力——真实路径没有签约协议号
//!   (`WANNING_ALIPAY_AGREEMENT_NO`)就拒绝构建,没有 agreement_no 的扣款请求
//!   绝不出网(fail-closed,见 [`from_snapshot_real`]);
//! - 资金零沉淀:Wanning 不碰钱、不代收代付、不做二清(刑事红线,无豁免);
//!   授权动作走闸([`crate::guard`] + wanning-core 的 delegation/gate),资金流走
//!   支付宝侧既有通道(商家扣款产品),从签约到扣款都在官方协议产品内;
//! - 红线:零密钥零注册零真实消费。真密钥经 [`crate::signing`] 的 env 注入实现
//!   现取现用(W-52),绝不写死在代码里、绝不落仓;本仓测试全部用自生成测试
//!   密钥对(W-28 先例)扮演两端。
//!
//! 共用类型(trait/请求/回调幂等/错误)在 [`crate::channel`];这里 `pub use` 再导出,
//! 既有引用路径(`wanning_demo::alipay::…`,W-11 测试引用)保持有效。
//!
//! TODO(账户开通后实签)清单(W-50 收敛到「实签」一级,W-52 落地引导半边):
//! 1. 真密钥接入:**已落地(W-52)**——[`crate::signing::EnvRsaSigner`] /
//!    [`crate::signing::EnvRsaVerifier`] 从 env 注入位现取现用(绝不写死/落仓);
//!    密钥管理工具(开放平台后台/密钥工具)由所有者侧操作。
//! 2. 实签验证:`wanning channel-test --channel alipay`(W-52)一条命令引导——
//!    L1 签名自测(零网络)→ L2 网关探针(precreate 零资金移动,核「签名认不认」)
//!    → L3 协议内 0.01 元真实扣款(四重明示,核完整链路与 async_payment_mode
//!    实际取值)。不一致处改这里并回归测试。
//! 3. 签约半边(alipay.user.agreement.page.sign → agreement_no 的落库)不归本仓:
//!    签约是所有者侧动作,Wanning 只消费协议号(口径在档,内部清单 B 节)。

use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use percent_encoding::utf8_percent_encode;
use serde::Deserialize;
use wanning_core::clock::{Clock, SystemClock};

use crate::guard::{check_real_spend, EnvSnapshot, RealSpendConfig};
use crate::http::{ApiTransport, HttpFailure, UreqApiTransport};
use crate::signing::{canonical_notify_string, canonical_query, MessageSigner, SignatureVerifier};

// 共用类型从 [`crate::channel`] 引入并**整体再导出**:既有引用路径
// (`wanning_demo::alipay::…`,W-11 测试引用)保持有效。
pub use crate::channel::{
    apply_pay_notify, PayNotify, PayRequest, PayResult, PayStatus, PaymentChannel, PaymentError,
    TradeState,
};

/// 支付宝官方网关([公开文档直核 W-50]:《自行实现签名》步骤5,网关固定;
/// 来源 https://opendocs.alipay.com/common/057k53 )。
pub const ALIPAY_GATEWAY: &str = "https://openapi.alipay.com/gateway.do";

/// 商家扣款单笔扣款上限:100 元(产品规则 [公开文档直核 W-50]:《周期/商家扣款 FAQ》
/// 「对每个用户的单笔扣款不超过 100 元」;来源在调研文档)。total_amount 官方字段
/// 上限是 100000000 元——本仓按产品口径取更严的 100 元,扣款语义由协议约定。
pub const ALIPAY_SINGLE_DEDUCT_MAX_CENTS: u64 = 100 * 100;

/// precreate 探针金额:1 分 = 0.01 元——官方 total_amount 最小值([公开文档直核
/// W-52]:取值范围 [0.01,100000000];探针只走报文,零资金移动)。
pub const PRECREATE_PROBE_AMOUNT_CENTS: u64 = 1;

/// precreate 探针的固定销售产品码 `FACE_TO_FACE_PAYMENT`(当面付;[公开文档直核
/// W-52]:官方 Java SDK AlipayTradePrecreateModel 默认值。v3 spec 未标必填 →
/// 显式带上:探针回答「签名认不认」,业务权限被拒也是已验签响应,同样回答问题)。
pub const PRECREATE_PRODUCT_CODE: &str = "FACE_TO_FACE_PAYMENT";

/// 真实路径请求模板的配置(所有者侧提供,经 env 注入,见 [`AlipayBackend::from_snapshot_real`])。
#[derive(Clone, PartialEq, Eq)]
pub struct AlipayRealConfig {
    /// 网关(默认 [`ALIPAY_GATEWAY`],可 env 覆盖以便测试打本地替身)。
    pub gateway: String,
    /// 支付宝开放平台应用 app_id(`WANNING_ALIPAY_APP_ID`)。
    pub app_id: String,
    /// 用户签约协议号(`WANNING_ALIPAY_AGREEMENT_NO`)——协议内扣款的凭证,缺它绝不发扣款。
    pub agreement_no: String,
    /// 异步通知地址(可选;`WANNING_ALIPAY_NOTIFY_URL`)。
    pub notify_url: Option<String>,
}

impl std::fmt::Debug for AlipayRealConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // agreement_no 是用户授权凭证(协议内扣款的扣款依据),Debug 打码
        // (同 wechat.rs contract_id 先例):只露前 4 字符 + 长度。
        f.debug_struct("AlipayRealConfig")
            .field("gateway", &self.gateway)
            .field("app_id", &self.app_id)
            .field("agreement_no", &masked(&self.agreement_no))
            .field("notify_url", &self.notify_url)
            .finish()
    }
}

fn masked(value: &str) -> String {
    let prefix: String = value.chars().take(4).collect();
    format!("{prefix}…({} 字符)", value.chars().count())
}

/// 一条已构建、待发送的真实扣款请求(报文模板的产物,便于测试逐字段钉死)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingPayRequest {
    /// 完整请求 URL:官方网关 + query(平台参数,ASCII 升序,`sign` 最后)。
    pub url: String,
    /// 请求体:`biz_content=<URL 编码的业务参数 JSON>`。
    pub body: String,
    /// 固定 `application/x-www-form-urlencoded`(网关表单语义,非 JSON)。
    pub content_type: &'static str,
}

/// 表单/URL 值编码:保留 RFC 3986 unreserved(`[A-Za-z0-9-._~]`),其余全编码
/// (空格 → `%20`)。与官方「URL 编码」语义一致([公开文档直核 W-50])。
const FORM_URLENCODED_SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

fn form_encode(value: &str) -> String {
    utf8_percent_encode(value, FORM_URLENCODED_SET).to_string()
}

/// 幂等键 → 官方 `out_trade_no` 字符集净化。
///
/// [公开文档直核 W-50]:out_trade_no「仅支持字母、数字、下划线」,64 字符以内。
/// 本仓幂等键形如 `w-{delegation_id}-{nonce}-{order}`(含 `-`),`-` 不在官方
/// 字符集 → 统一替换为 `_`。注意:替换是按字符的,极小概率不同键净化后碰撞
/// (如 `a-b` 与 `a_b`)——delegation_id/order_id 侧禁用 `_` 与 `-` 混用时无此忧;
/// 碰撞即幂等键冲突,上游幂等语义会拦下第二笔,不会重复扣款。
pub fn sanitize_out_trade_no(raw: &str) -> Result<String, PaymentError> {
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        return Err(PaymentError::InvalidRequest(
            "out_trade_no 净化后为空:out_request_no 不能为空(扣款必须挂授权上下文)".to_string(),
        ));
    }
    // 官方上限 64 字符([公开文档直核 W-50]);UTF-8 中文已被替换为 `_`,按字节计。
    if sanitized.len() > 64 {
        return Err(PaymentError::InvalidRequest(format!(
            "out_trade_no 超长:净化后 {} 字节 > 官方上限 64 字符;缩短 delegation_id/order_id",
            sanitized.len()
        )));
    }
    Ok(sanitized)
}

/// 分 → 元字符串(精确两位小数,纯整数运算,禁浮点——这是钱)。
///
/// [公开文档直核 W-50]:total_amount 精确到小数点后两位,取值范围 [0.01,100000000]。
pub fn cents_to_yuan_amount(cents: u64) -> String {
    format!("{}.{:02}", cents / 100, cents % 100)
}

/// 元字符串 → 分(严格解析,歧义零容忍——这是钱)。
///
/// 接受 0/1/2 位小数(`"10"`/`"88.8"`/`"88.88"`);拒绝三位以上小数、负号、
/// 正号、空白、非数字、多个小数点。解析前不做 trim:`" 1.00"` 拒(调用方应传
/// 渠道原样字符串,静默吞空白会掩盖报文异常)。
pub fn yuan_to_cents(amount: &str) -> Result<u64, PaymentError> {
    let (int_part, frac_part) = match amount.split_once('.') {
        None => (amount, ""),
        Some((i, f)) => (i, f),
    };
    let digits_only = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits_only(int_part) {
        return Err(PaymentError::BadResponse(format!(
            "金额解析失败:整数部分 `{int_part}` 不是纯数字(报文原样: {amount:?})"
        )));
    }
    if frac_part.len() > 2 || !frac_part.bytes().all(|b| b.is_ascii_digit()) {
        return Err(PaymentError::BadResponse(format!(
            "金额解析失败:小数部分 `{frac_part}` 超过两位或非数字(渠道金额精确到分,歧义零容忍)"
        )));
    }
    let yuan: u64 = int_part.parse().map_err(|_| {
        PaymentError::BadResponse(format!("金额解析失败:整数部分 `{int_part}` 溢出"))
    })?;
    let cents = yuan
        .checked_mul(100)
        .and_then(|v| {
            let frac = match frac_part.len() {
                0 => 0,
                1 => frac_part.parse::<u64>().ok()? * 10,
                _ => frac_part.parse::<u64>().ok()?,
            };
            v.checked_add(frac)
        })
        .ok_or_else(|| PaymentError::BadResponse(format!("金额解析失败: {amount:?} 溢出")))?;
    Ok(cents)
}

/// Unix 秒 → 支付宝 `timestamp` 格式(北京时间,`yyyy-MM-dd HH:mm:ss`)。
///
/// 格式 [公开文档直核 W-50](《自行实现签名》请求示例);时区取东八区与
/// ANAI 实战一致(W-50 双源交叉,ANAI 生产后端按东八区生成)。复用 W-22 审计页
/// 的零依赖 UTC 推导(含闰日,已测),+8 小时即北京时间。
pub fn beijing_timestamp(now_unix: u64) -> String {
    crate::audit_html::format_utc(now_unix.saturating_add(8 * 3600))
}

/// 构建 `alipay.trade.pay`(商家扣款)**真实请求模板**。
///
/// 报文来源([公开文档直核 W-50],引用 URL 落调研文档;ANAI 实战双源交叉一致):
/// - 接口 `alipay.trade.pay`(统一收单交易支付接口,商家扣款场景
///   `product_code=GENERAL_WITHHOLDING` + `agreement_params.agreement_no`);
/// - 平台参数放 query、业务参数(`biz_content`)放 body(《自行实现签名》步骤5
///   请求拆分);`Content-Type: application/x-www-form-urlencoded`;
/// - 签名 = [`crate::signing::canonical_query`] 官方规则 → [`MessageSigner`] 签名 →
///   base64 → query 里 `sign` 参数(排序参数之后追加,官方示例形状)。
///
/// 状态映射是**模板决策**(非官方规定):`code=10000` 且无 `async_payment_mode`
/// 或其值为 `SYNC_DIRECT_PAY` → 同步成功;其余异步受理值 → `Pending`,终态由
/// **已验签回调**确认([`verify_pay_notify`] + [`apply_pay_notify`])。
pub fn build_trade_pay_request(
    cfg: &AlipayRealConfig,
    request: &PayRequest,
    timestamp: &str,
    signer: &dyn MessageSigner,
) -> Result<OutgoingPayRequest, PaymentError> {
    request.validate()?;
    // 协议内扣款语义(模块文档;W-52 起探针配置不带协议号,这层构建期守卫保证
    // 探针配置绝无可能被拿去构建扣款报文——fail-closed,零网络)。
    if cfg.agreement_no.trim().is_empty() {
        return Err(PaymentError::InvalidRequest(
            "无协议号绝不发扣款:AlipayRealConfig 缺 agreement_no(协议内扣款,不是裸转账)"
                .to_string(),
        ));
    }
    if request.amount_cents > ALIPAY_SINGLE_DEDUCT_MAX_CENTS {
        return Err(PaymentError::InvalidRequest(format!(
            "扣款金额超商家扣款产品单笔上限:{} 分 > {} 分(100 元,产品规则;协议内扣款语义,见模块文档)",
            request.amount_cents, ALIPAY_SINGLE_DEDUCT_MAX_CENTS
        )));
    }
    let out_trade_no = sanitize_out_trade_no(&request.out_request_no())?;
    let total_amount = cents_to_yuan_amount(request.amount_cents);
    let biz = serde_json::json!({
        // 协议内扣款凭证:签约协议号(没有它网关拒绝,本地也不该发——见模块文档)。
        "agreement_params": { "agreement_no": cfg.agreement_no },
        "out_trade_no": out_trade_no,
        // 商家扣款产品固定销售产品码([公开文档直核 W-50]:接口文档示例值)。
        "product_code": "GENERAL_WITHHOLDING",
        // 商品标题/交易描述。固定文案:Wanning 是闸不是商户,这笔钱是「被授权的
        // agent 消费意图」对应的订单扣款;订单号已在 out_trade_no 里可对账。
        "subject": "Wanning 协议内扣款",
        "total_amount": total_amount,
    })
    .to_string();

    // 平台参数(官方公共参数;notify_url 可选,配置了才参与)。
    let mut params: Vec<(&str, String)> = vec![
        ("app_id", cfg.app_id.clone()),
        ("charset", "utf-8".to_string()),
        ("method", "alipay.trade.pay".to_string()),
        ("sign_type", "RSA2".to_string()),
        ("timestamp", timestamp.to_string()),
        ("version", "1.0".to_string()),
    ];
    if let Some(url) = &cfg.notify_url {
        params.push(("notify_url", url.clone()));
    }
    // 平台参数 + biz_content → 签名 → 完整请求(与 precreate 共用一条管线,见
    // [`sign_gateway_request`];官方步骤2/4/5 同一规则)。
    sign_gateway_request(&cfg.gateway, params, &biz, signer)
}

/// 平台参数 + 业务参数(`biz_content`)→ 待签串 → RSA2 签名 → base64 → 完整请求。
/// trade.pay 与 precreate(W-52)共用,**不另写字段面**(W-52 任务书;第二个
/// 使用方出现才抽,http.rs/mock_server.rs 同一先例)。
///
/// - 签名串:biz_content 以原始 JSON 字符串参与(值原样,零转义——官方步骤2);
/// - query = 平台参数按 ASCII 升序 + `sign` 追加在最后(官方请求示例形状);
///   值全部 URL 编码(官方步骤4:签名之后才编码)。
fn sign_gateway_request(
    gateway: &str,
    mut params: Vec<(&'static str, String)>,
    biz: &str,
    signer: &dyn MessageSigner,
) -> Result<OutgoingPayRequest, PaymentError> {
    let mut signed: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    signed.push(("biz_content", biz));
    let canonical =
        canonical_query(&signed).map_err(|e| PaymentError::InvalidRequest(e.to_string()))?;
    let signature = signer
        .sign(&canonical)
        .map_err(|e| PaymentError::Config(format!("请求签名失败,拒绝出网: {e}")))?;
    let sign_b64 = B64.encode(signature);

    params.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut query_pairs: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", form_encode(v)))
        .collect();
    query_pairs.push(format!("sign={}", form_encode(&sign_b64)));
    Ok(OutgoingPayRequest {
        url: format!("{}?{}", gateway, query_pairs.join("&")),
        body: format!("biz_content={}", form_encode(biz)),
        content_type: "application/x-www-form-urlencoded",
    })
}

/// 解析**已签名**的同步响应:提取 `alipay_trade_pay_response` 原文(RawValue 保真)
/// → 验签先行 → `code` 语义映射 → 金额对账。见 [`build_trade_pay_request`] 的
/// 状态映射说明与模块文档来源。
///
/// 验签对象 = `xxx_response` 成员**逐字节原文**(含大括号与引号;官方《自行实现
/// 验签》同步响应规则)。部分网关返回会把 `/` 转义成 `\/`,官方 SDK 对此做一次
/// 替换重试——本仓同样:先按原文验,不过再把 `\/` → `/` 验一次(两次都不过才拒)。
fn parse_trade_pay_response(
    raw: &str,
    request: &PayRequest,
    verifier: &dyn SignatureVerifier,
) -> Result<PayResult, PaymentError> {
    #[derive(Deserialize)]
    struct PayEnvelope<'a> {
        #[serde(rename = "alipay_trade_pay_response", borrow)]
        response: &'a serde_json::value::RawValue,
        #[serde(rename = "sign")]
        sign: String,
    }
    let envelope: PayEnvelope = serde_json::from_str(raw).map_err(|e| {
        PaymentError::BadResponse(format!(
            "同步响应缺少 alipay_trade_pay_response/sign 形态,拒绝解析: {e};原文: {raw:.200}"
        ))
    })?;
    let inner = envelope.response.get();
    // 验签半边与 precreate 探针(W-52)共用一条管线,见 [`verify_envelope_response`]。
    verify_envelope_response(inner, &envelope.sign, verifier)?;

    let body: serde_json::Value = serde_json::from_str(inner).map_err(|e| {
        PaymentError::BadResponse(format!("同步响应正文解析失败: {e};原文: {inner:.200}"))
    })?;
    let code = body
        .get("code")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PaymentError::BadResponse("同步响应缺 code".to_string()))?;
    if code != "10000" {
        // 网关明确拒绝(响应可信:已验签):码值原样带出,逐条落审计追因。
        return Err(PaymentError::GatewayRejected {
            code: code.to_string(),
            sub_code: str_field(&body, "sub_code"),
            sub_msg: str_field(&body, "sub_msg"),
        });
    }
    let trade_no = required_field(&body, "trade_no", "无法对账")?;
    let total_amount = required_field(&body, "total_amount", "无法对账")?;
    let amount_cents = yuan_to_cents(&total_amount)?;
    if amount_cents != request.amount_cents {
        return Err(PaymentError::BadResponse(format!(
            "渠道回传金额与请求不符:请求 {} 分 / 响应 {total_amount} 元(拒绝采信,需人工核对)",
            request.amount_cents
        )));
    }
    let status = match str_field(&body, "async_payment_mode") {
        // 无该字段或 SYNC_DIRECT_PAY = 同步直接扣款完成(模板决策,见文档)。
        None => PayStatus::Success,
        Some(mode) => match mode.as_str() {
            "SYNC_DIRECT_PAY" => PayStatus::Success,
            // 官方枚举其余四个值 = 异步受理,终态等已验签回调。
            "ASYNC_DELAY_PAY"
            | "ASYNC_REALTIME_PAY"
            | "NORMAL_ASYNC_PAY"
            | "QUOTA_OCCUPYIED_ASYNC_PAY" => PayStatus::Pending,
            // 未知取值 fail-closed:不猜语义,宁可人工核对。
            other => {
                return Err(PaymentError::BadResponse(format!(
                    "未知 async_payment_mode `{other}`,拒绝猜测语义(fail-closed)"
                )))
            }
        },
    };
    Ok(PayResult {
        out_request_no: request.out_request_no(),
        trade_no,
        status,
        amount_cents,
    })
}

fn str_field(body: &serde_json::Value, key: &str) -> Option<String> {
    body.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// 同步响应信封的**验签半边**(trade.pay 与 precreate 探针 W-52 共用;第二个
/// 使用方出现才抽,http.rs/sign_gateway_request 同一先例):sign base64 解码
/// → 先按 `xxx_response` 成员**逐字节原文**验,不过再把 `\/` → `/` 替换重试
/// 一次(官方 SDK 对部分网关的转义响应同款),两次都不过 = 拒绝采信。
fn verify_envelope_response(
    inner: &str,
    sign_b64: &str,
    verifier: &dyn SignatureVerifier,
) -> Result<(), PaymentError> {
    let sig_bytes = B64
        .decode(sign_b64.as_bytes())
        .map_err(|e| PaymentError::BadResponse(format!("同步响应 sign 不是合法 base64: {e}")))?;
    let unescaped = inner.replace("\\/", "/");
    let verified = verifier.verify(inner, &sig_bytes)
        || (unescaped != inner && verifier.verify(&unescaped, &sig_bytes));
    if !verified {
        return Err(PaymentError::BadResponse(
            "同步响应验签不过:报文与签名不匹配,拒绝采信(fail-closed)".to_string(),
        ));
    }
    Ok(())
}

/// `code=10000` 响应的必备字段:缺失**或空串** = BadResponse 点名(报文不完整
/// 绝不猜;`why` 说明为什么必须由调用方给,如「无法对账」「预下单未成立」)。
fn required_field(body: &serde_json::Value, key: &str, why: &str) -> Result<String, PaymentError> {
    str_field(body, key)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| PaymentError::BadResponse(format!("code=10000 但缺 {key},{why}")))
}

/// 验证**原始异步通知 form 报文**并解析([`PayNotify`])——验签前置门。
///
/// [公开文档直核 W-50]:《自行实现验签》(https://opendocs.alipay.com/common/02mse7)
/// 异步通知规则:剔除 `sign` 与 `sign_type` 后其余参数 url_decode、按参数名 ASCII
/// 升序拼 `key=value` 以 `&` 连接([`crate::signing::canonical_notify_string`]),
/// 用支付宝公钥验签(**验签先行,验不过的报文绝不解析入账**,fail-closed)。
/// form 解码语义:非 sign 参数按标准表单规则(`+` → 空格后再百分号解码);sign
/// 参数只做百分号解码(base64 的 `+` 不能被空格替换吞掉)。
pub fn verify_pay_notify(
    raw: &str,
    verifier: &dyn SignatureVerifier,
) -> Result<PayNotify, PaymentError> {
    let mut pairs: Vec<(&str, String)> = Vec::new();
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').ok_or_else(|| {
            PaymentError::BadResponse(format!("通知报文表单项缺 `=`: {pair:.60}"))
        })?;
        pairs.push((key, value.to_string()));
    }
    let sign_encoded = pairs
        .iter()
        .find(|(k, _)| *k == "sign")
        .map(|(_, v)| v.clone())
        .ok_or_else(|| {
            PaymentError::BadResponse("通知报文缺 sign,拒绝采信(fail-closed)".to_string())
        })?;
    let sign = percent_decode(&sign_encoded, false);
    let sig_bytes = B64
        .decode(sign.as_bytes())
        .map_err(|e| PaymentError::BadResponse(format!("通知 sign 不是合法 base64: {e}")))?;

    // 官方要求先 url_decode 再拼待验签串——原始(编码态)参数不参与验签。
    let decoded: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| {
            (
                percent_decode(k, true),
                if *k == "sign" {
                    sign.clone()
                } else {
                    percent_decode(v, true)
                },
            )
        })
        .collect();
    // 验签用「解码后」参数拼的规范化串(官方:url_decode 后字典序拼接)。
    let decoded_borrowed: Vec<(&str, &str)> = decoded
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let canonical = canonical_notify_string(&decoded_borrowed)
        .map_err(|e| PaymentError::BadResponse(format!("通知参数规范化失败: {e}")))?;
    if !verifier.verify(&canonical, &sig_bytes) {
        return Err(PaymentError::BadResponse(
            "通知验签不过:报文与签名不匹配,拒绝解析入账(fail-closed)".to_string(),
        ));
    }

    let lookup = |key: &str| -> Option<String> {
        decoded
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    let out_request_no = lookup("out_trade_no")
        .ok_or_else(|| PaymentError::BadResponse("通知缺 out_trade_no,无法对账".to_string()))?;
    let trade_no = lookup("trade_no")
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| PaymentError::BadResponse("通知缺 trade_no,无法对账".to_string()))?;
    let total_amount = lookup("total_amount")
        .ok_or_else(|| PaymentError::BadResponse("通知缺 total_amount,无法对账".to_string()))?;
    let amount_cents = yuan_to_cents(&total_amount)?;
    let status = match lookup("trade_status").as_deref() {
        Some("TRADE_SUCCESS") | Some("TRADE_FINISHED") => PayStatus::Success,
        Some("WAIT_BUYER_PAY") => PayStatus::Pending,
        Some("TRADE_CLOSED") => PayStatus::Failed,
        // 官方触发通知类型表之外的取值:不猜语义,fail-closed。
        other => {
            return Err(PaymentError::BadResponse(format!(
                "未知 trade_status {other:?},拒绝猜测语义(fail-closed)"
            )))
        }
    };
    Ok(PayNotify {
        out_request_no,
        trade_no,
        status,
        amount_cents,
    })
}

/// 百分号解码。`plus_as_space`:标准表单语义(`+` → 空格,先替换再解码);
/// sign 用 `false`(base64 的 `+` 是有效字符)。
fn percent_decode(value: &str, plus_as_space: bool) -> String {
    let replaced = if plus_as_space {
        value.replace('+', " ")
    } else {
        value.to_string()
    };
    let src = replaced.as_bytes();
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'%' && i + 3 <= src.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&src[i + 1..i + 3]).unwrap_or("zz"), 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(src[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ── precreate 探针(W-52:channel-test L2「网关探针」的报文半边) ────────────
//
// `alipay.trade.precreate`(当面付·扫码支付预下单)= 官方接口里最小「真签名、
// 真网关、零资金移动」的探针([公开文档直核 W-52]:预下单只生成二维码,买家不
// 扫码即不产生资金流;字段面来源 target/w52/precreate-direct-verify.md,官方
// Java SDK + v3-openapi 双源直核)。探针回答的问题 = **服务器认不认签名**
// (W-50「待实签」清单第一格);业务权限被拒也是已验签响应,同样回答这个问题。
// L2 过 ≠ 能扣款(过账本不豁免,见 [`AlipayBackend::probe_precreate`])。

/// 构建 `alipay.trade.precreate` 请求:与 [`build_trade_pay_request`] 同一条
/// 签名管线([`sign_gateway_request`]),**不另写字段面**(W-52 任务书);
/// 平台参数同一组,唯独不带 `notify_url`——探针是一次性报文,不留异步回调面。
///
/// 业务参数([公开文档直核 W-52]):`out_trade_no`(官方字符集与长度由
/// [`sanitize_out_trade_no`] 统一把关)/ `total_amount`(探针固定 1 分 = 官方
/// 最小值 0.01 元)/ `subject`(官方必填 ≤256 字符,禁 `/`、`=`、`&` 特殊字符)/
/// `product_code` = [`PRECREATE_PRODUCT_CODE`]。
pub fn build_precreate_request(
    cfg: &AlipayRealConfig,
    out_trade_no: &str,
    amount_cents: u64,
    subject: &str,
    timestamp: &str,
    signer: &dyn MessageSigner,
) -> Result<OutgoingPayRequest, PaymentError> {
    let out_trade_no = sanitize_out_trade_no(out_trade_no)?;
    if amount_cents == 0 {
        return Err(PaymentError::InvalidRequest(
            "探针金额必须为正:0 分不构成一笔可预下单的交易(total_amount 官方最小 0.01 元)"
                .to_string(),
        ));
    }
    let subject = subject.trim();
    if subject.is_empty() {
        return Err(PaymentError::InvalidRequest(
            "subject 不能为空:官方必填字段,探针报文同样要带商品标题".to_string(),
        ));
    }
    let subject_len = subject.chars().count();
    if subject_len > 256 {
        return Err(PaymentError::InvalidRequest(format!(
            "subject 超长:{subject_len} 字符 > 官方上限 256"
        )));
    }
    if subject.contains('/') || subject.contains('=') || subject.contains('&') {
        return Err(PaymentError::InvalidRequest(
            "subject 含官方禁止的特殊字符(/、=、&):换文案重试,绝不静默改写报文".to_string(),
        ));
    }
    let biz = serde_json::json!({
        "out_trade_no": out_trade_no,
        "product_code": PRECREATE_PRODUCT_CODE,
        "subject": subject,
        "total_amount": cents_to_yuan_amount(amount_cents),
    })
    .to_string();

    let params: Vec<(&'static str, String)> = vec![
        ("app_id", cfg.app_id.clone()),
        ("charset", "utf-8".to_string()),
        ("method", "alipay.trade.precreate".to_string()),
        ("sign_type", "RSA2".to_string()),
        ("timestamp", timestamp.to_string()),
        ("version", "1.0".to_string()),
    ];
    sign_gateway_request(&cfg.gateway, params, &biz, signer)
}

/// precreate 探针的结果(已验签响应里与探针相关的字段 + 已发出的请求原文)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecreateProbe {
    /// 响应回传的商户订单号(必须与请求一致,不一致 = 拒绝采信)。
    pub out_trade_no: String,
    /// 支付宝交易号(预下单成立的凭证)。
    pub prepay_id: String,
    /// 二维码链接(买家扫码才产生资金流;探针只看签名,不扫码)。
    pub qr_code: Option<String>,
    /// 短码(可选,官方字段 `share_code`)。
    pub share_code: Option<String>,
    /// 已验签的 `alipay_trade_precreate_response` 成员原文(逐字节,落取证档用;
    /// 密钥不在此列,信封里没有任何密钥材料)。
    pub verified_body: String,
    /// 已发出的请求 URL 原文(query 含 `app_id` 与 `sign`——不是密钥材料,但
    /// channel-test 取证档按自己的脱敏纪律打码;本层不做脱敏决策)。
    pub request_url: String,
    /// 已发出的请求体原文(`biz_content=<URL 编码 JSON>`)。
    pub request_body: String,
}

/// 解析**已签名**的 precreate 同步响应:信封提取(RawValue 保真)→ 验签
/// ([`verify_envelope_response`] 共用)→ `code` 语义 → `out_trade_no` 对账。
fn parse_precreate_response(
    raw: &str,
    requested_out_trade_no: &str,
    verifier: &dyn SignatureVerifier,
) -> Result<PrecreateProbe, PaymentError> {
    #[derive(Deserialize)]
    struct PrecreateEnvelope<'a> {
        #[serde(rename = "alipay_trade_precreate_response", borrow)]
        response: &'a serde_json::value::RawValue,
        #[serde(rename = "sign")]
        sign: String,
    }
    let envelope: PrecreateEnvelope = serde_json::from_str(raw).map_err(|e| {
        PaymentError::BadResponse(format!(
            "同步响应缺少 alipay_trade_precreate_response/sign 形态,拒绝解析: {e};原文: {raw:.200}"
        ))
    })?;
    let inner = envelope.response.get();
    verify_envelope_response(inner, &envelope.sign, verifier)?;

    let body: serde_json::Value = serde_json::from_str(inner).map_err(|e| {
        PaymentError::BadResponse(format!("同步响应正文解析失败: {e};原文: {inner:.200}"))
    })?;
    let code = body
        .get("code")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PaymentError::BadResponse("同步响应缺 code".to_string()))?;
    if code != "10000" {
        // 网关明确拒绝(响应可信:已验签)。对探针而言这也是答案:签名类
        // sub_code = 请求签名没过;参数/权限类 = 签名已过、业务被拒。码值
        // 原样带出,逐条落审计/取证档追因。
        return Err(PaymentError::GatewayRejected {
            code: code.to_string(),
            sub_code: str_field(&body, "sub_code"),
            sub_msg: str_field(&body, "sub_msg"),
        });
    }
    let out_trade_no = required_field(&body, "out_trade_no", "无法对账")?;
    if out_trade_no != requested_out_trade_no {
        return Err(PaymentError::BadResponse(format!(
            "渠道回传 out_trade_no 与请求不符:请求 {requested_out_trade_no} / 响应 {out_trade_no}(拒绝采信)"
        )));
    }
    let prepay_id = required_field(&body, "prepay_id", "预下单未成立")?;
    Ok(PrecreateProbe {
        out_trade_no,
        prepay_id,
        qr_code: str_field(&body, "qr_code"),
        share_code: str_field(&body, "share_code"),
        verified_body: inner.to_string(),
        // parse 层拿不到请求原文,由调用方(probe_precreate)补上——见下。
        request_url: String::new(),
        request_body: String::new(),
    })
}

/// 把请求 URL 的 query 拆回参数对(percent 解码)。`form_encode` 从不产生
/// `+`(空格编成 `%20`),故这里 `+` 不按空格解——与签名串的字节原样一致,
/// channel-test L1 用它复算待签串做「签名自测」往返。
pub fn query_pairs_of(url: &str) -> Vec<(String, String)> {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or(url);
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) => (percent_decode(key, false), percent_decode(value, false)),
            None => (percent_decode(pair, false), String::new()),
        })
        .collect()
}

/// L2 起的真实路径护栏:env `WANNING_ALLOW_REAL_SPEND` 必须为 `1`——与 W-07
/// 护栏同一个开关、同一个常量 [`crate::guard::ENV_ALLOW_REAL_SPEND`],探针不
/// 另立开关(过账本不豁免护栏)。刻意不走 [`check_real_spend`]:那要求 GLM/
/// 京东四密钥,语义是「全链真实消费」的护栏;探针只碰支付宝网关,app_id 与
/// 密钥槽位缺失由 [`AlipayBackend::from_snapshot_probe`] / 槽位门逐项点名。
fn require_allow_real_spend(env: &EnvSnapshot) -> Result<(), PaymentError> {
    if env.get(crate::guard::ENV_ALLOW_REAL_SPEND) == Some("1") {
        Ok(())
    } else {
        Err(PaymentError::GuardBlocked(
            "真实路径护栏未开:WANNING_ALLOW_REAL_SPEND 必须为 1(W-07 护栏;channel-test L2 起必需)"
                .to_string(),
        ))
    }
}

/// 支付宝 adapter(免密代扣 = **协议内扣款**,不是裸转账——见模块文档)。
///
/// `Debug` 手写:凭证在 [`RealSpendConfig`] 打码、协议号在 [`AlipayRealConfig`]
/// 打码、签名/验签槽位只显「已接入/未接入」——打印不泄密(有测试);签名私钥
/// 只存在于 [`MessageSigner`] 槽位,本结构体绝不持有密钥材料。
pub struct AlipayBackend {
    endpoint: String,
    credentials: RealSpendConfig,
    transport: Arc<dyn ApiTransport + Send + Sync>,
    /// `Some` = 真实路径(官方模板报文);`None` = mock 路径(本地契约报文)。
    real_config: Option<AlipayRealConfig>,
    signer: Option<Arc<dyn MessageSigner + Send + Sync>>,
    verifier: Option<Arc<dyn SignatureVerifier + Send + Sync>>,
}

fn env_value(env: &EnvSnapshot, key: &str) -> Option<String> {
    env.get(key)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// 签名/验签槽位的引用对([`AlipayBackend::require_slots`] 的返回形状;type
/// complexity 收口,W-50 `CapturedRequests` 同款先例)。
type SlotRefs<'a> = (
    &'a Arc<dyn MessageSigner + Send + Sync>,
    &'a Arc<dyn SignatureVerifier + Send + Sync>,
);

impl AlipayBackend {
    /// 本地 mock 用:测试假凭证 + 本地地址(零外网)。
    pub fn new_mock(endpoint: &str, transport: Arc<dyn ApiTransport + Send + Sync>) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            credentials: RealSpendConfig {
                glm_key: String::new(),
                jd_app_key: "mock-app-key".to_string(),
                jd_app_secret: "mock-app-secret".to_string(),
                jd_access_token: "mock-access-token".to_string(),
            },
            transport,
            real_config: None,
            signer: None,
            verifier: None,
        }
    }

    /// 真实路径:护栏(env 全齐)→ 支付宝配置(app_id + 签约协议号必填)→ ureq 传输。
    ///
    /// 网关默认官方地址 [`ALIPAY_GATEWAY`]([公开文档直核 W-50]),`WANNING_ALIPAY_ENDPOINT`
    /// 可覆盖(测试打本地替身用;生产不要覆盖)。**fail-closed 链**:护栏缺项 →
    /// GuardBlocked;`WANNING_ALIPAY_APP_ID` / `WANNING_ALIPAY_AGREEMENT_NO` 缺失 →
    /// Config 点名(协议内扣款语义:没有协议号绝不发扣款)。
    pub fn from_snapshot_real(env: &EnvSnapshot) -> Result<Self, PaymentError> {
        let credentials = check_real_spend(env)
            .map_err(|denied| PaymentError::GuardBlocked(denied.to_string()))?;
        let app_id = env_value(env, "WANNING_ALIPAY_APP_ID").ok_or_else(|| {
            PaymentError::Config(
                "缺少 WANNING_ALIPAY_APP_ID:支付宝开放平台应用 app_id(所有者在开放平台创建应用后填入)"
                    .to_string(),
            )
        })?;
        let agreement_no = env_value(env, "WANNING_ALIPAY_AGREEMENT_NO").ok_or_else(|| {
            PaymentError::Config(
                "缺少 WANNING_ALIPAY_AGREEMENT_NO:用户签约协议号(协议内扣款凭证;没有协议号的扣款=裸转账,绝不发)"
                    .to_string(),
            )
        })?;
        let real_config = AlipayRealConfig {
            gateway: env_value(env, "WANNING_ALIPAY_ENDPOINT")
                .unwrap_or_else(|| ALIPAY_GATEWAY.to_string()),
            app_id,
            agreement_no,
            notify_url: env_value(env, "WANNING_ALIPAY_NOTIFY_URL"),
        };
        Ok(Self {
            endpoint: real_config.gateway.clone(),
            credentials,
            transport: Arc::new(UreqApiTransport),
            real_config: Some(real_config),
            signer: None,
            verifier: None,
        })
    }

    /// 从当前进程环境构建真实 adapter。
    pub fn from_env_real() -> Result<Self, PaymentError> {
        Self::from_snapshot_real(&EnvSnapshot::from_process_env())
    }

    /// L2 网关探针配置(`wanning channel-test` 用):与 [`Self::from_snapshot_real`]
    /// 同一 env 槽位、同一条报文管线,**唯独不要求签约协议号**——探针只做
    /// precreate(零资金移动),没有扣款语义。护栏只查
    /// `WANNING_ALLOW_REAL_SPEND=1`(W-07 同一开关,见 [`require_allow_real_spend`]);
    /// `WANNING_ALIPAY_APP_ID` 缺失在这里点名。**探针配置拿去扣款会在构建层
    /// fail-closed**([`build_trade_pay_request`] 无协议号绝不发扣款,零网络)。
    pub fn from_snapshot_probe(env: &EnvSnapshot) -> Result<Self, PaymentError> {
        require_allow_real_spend(env)?;
        let app_id = env_value(env, "WANNING_ALIPAY_APP_ID").ok_or_else(|| {
            PaymentError::Config(
                "缺少 WANNING_ALIPAY_APP_ID:支付宝开放平台应用 app_id(所有者在开放平台创建应用后填入)"
                    .to_string(),
            )
        })?;
        let real_config = AlipayRealConfig {
            gateway: env_value(env, "WANNING_ALIPAY_ENDPOINT")
                .unwrap_or_else(|| ALIPAY_GATEWAY.to_string()),
            app_id,
            // 探针无协议号:扣款语义被构建层守卫挡死(协议内扣款,不是裸转账)。
            agreement_no: String::new(),
            notify_url: None,
        };
        Ok(Self {
            endpoint: real_config.gateway.clone(),
            // 探针不消费 GLM/京东凭证(全链护栏语义与探针不同构,见
            // require_allow_real_spend);留空串,Debug 打码照常生效。
            credentials: RealSpendConfig {
                glm_key: String::new(),
                jd_app_key: String::new(),
                jd_app_secret: String::new(),
                jd_access_token: String::new(),
            },
            transport: Arc::new(UreqApiTransport),
            real_config: Some(real_config),
            signer: None,
            verifier: None,
        })
    }

    /// 换传输(测试把真实路径指向本地替身/录制的唯一入口)。
    pub fn with_transport(mut self, transport: Arc<dyn ApiTransport + Send + Sync>) -> Self {
        self.transport = transport;
        self
    }

    /// 接签名槽位(商户应用私钥;TODO(账户开通后实签):经 env 注入位接入)。
    pub fn with_signer(mut self, signer: Arc<dyn MessageSigner + Send + Sync>) -> Self {
        self.signer = Some(signer);
        self
    }

    /// 接验签槽位(支付宝公钥;TODO(账户开通后实签):经 env 注入位接入)。
    pub fn with_verifier(mut self, verifier: Arc<dyn SignatureVerifier + Send + Sync>) -> Self {
        self.verifier = Some(verifier);
        self
    }

    /// 当前网关端点(默认 [`ALIPAY_GATEWAY`],env 可覆盖)。
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    // -- mock 路径(W-11 本地契约,原样保留) --------------------------------

    fn pay_body(&self, request: &PayRequest) -> String {
        serde_json::json!({
            "out_request_no": request.out_request_no(),
            "order_id": request.order_id,
            "amount_cents": request.amount_cents,
            "delegation_id": request.delegation_id,
            "intent_nonce": request.intent_nonce,
            // 占位标记「协议内扣款」语义;真实报文见 build_trade_pay_request。
            "scene": "agreement"
        })
        .to_string()
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        // mock 契约用 Bearer 演示管线;真实路径报文是表单+签名,不用 Bearer。
        vec![(
            "Authorization".to_string(),
            format!("Bearer {}", self.credentials.jd_access_token),
        )]
    }

    fn post(
        &self,
        url: &str,
        body: &str,
        headers: Vec<(String, String)>,
    ) -> Result<String, PaymentError> {
        self.transport
            .post_json(url, body, &headers)
            .map_err(map_transport_failure)
    }

    fn call(&self, body: &str) -> Result<String, PaymentError> {
        let headers = self.auth_headers();
        self.post(&self.endpoint, body, headers)
    }

    // -- 真实路径(W-50 官方模板) -------------------------------------------

    fn trigger_pay_real(
        &mut self,
        cfg: &AlipayRealConfig,
        request: &PayRequest,
    ) -> Result<PayResult, PaymentError> {
        // fail-closed:签名/验签槽位缺一即拒,绝不出网(没有签名 = 报文不可信;
        // 没有验签 = 响应无法采信)。与 precreate 探针共用同一道门。
        let (signer, verifier) = self.require_slots()?;
        let timestamp = beijing_timestamp(SystemClock.now());
        let outgoing = build_trade_pay_request(cfg, request, &timestamp, signer.as_ref())?;
        let headers = vec![(
            "Content-Type".to_string(),
            outgoing.content_type.to_string(),
        )];
        let raw = self.post(&outgoing.url, &outgoing.body, headers)?;
        parse_trade_pay_response(&raw, request, verifier.as_ref())
    }

    /// 真实路径的签名/验签槽位门(pay 与 precreate 探针共用,第二个使用方
    /// 出现才抽)。返回槽位引用,调用方拿到即证明门已过。
    fn require_slots(&self) -> Result<SlotRefs<'_>, PaymentError> {
        let signer = self.signer.as_ref().ok_or_else(|| {
            PaymentError::Config(
                "真实路径必须接签名槽位:商户应用私钥(经 with_signer 接入;账户开通后实签)"
                    .to_string(),
            )
        })?;
        let verifier = self.verifier.as_ref().ok_or_else(|| {
            PaymentError::Config(
                "真实路径必须接验签槽位:支付宝公钥(经 with_verifier 接入;验不过的响应绝不采信)"
                    .to_string(),
            )
        })?;
        Ok((signer, verifier))
    }

    /// L2 网关探针:构建 precreate 报文(env 注入的真密钥签名)→ 真网关 →
    /// 已验签响应解析。**零资金移动**(预下单只生成二维码,买家不扫码即无
    /// 资金流),但仍过账本——探针的消费意图由 channel-test 走 WAL 审计行,
    /// 过账本不豁免(W-52 任务书)。签名/验签槽位缺失 = [`require_slots`]
    /// 拒绝,零网络。
    pub fn probe_precreate(
        &mut self,
        out_trade_no: &str,
        amount_cents: u64,
        subject: &str,
    ) -> Result<PrecreateProbe, PaymentError> {
        // clone 出配置再进真实路径(同 trigger_pay 的借用拆法;探针配置仅含
        // 非密钥标识,克隆廉价)。
        let cfg = self.real_config.clone().ok_or_else(|| {
            PaymentError::Config(
                "precreate 探针只走真实路径(mock 路径没有网关可探;用 from_snapshot_probe 构建)"
                    .to_string(),
            )
        })?;
        let (signer, verifier) = self.require_slots()?;
        // 先净化一次给响应对账用;build_precreate_request 内部再净化是无害的
        // 幂等操作(净化输出只含 [A-Za-z0-9_],再净化逐字节不变)。
        let out_trade_no = sanitize_out_trade_no(out_trade_no)?;
        let timestamp = beijing_timestamp(SystemClock.now());
        let outgoing = build_precreate_request(
            &cfg,
            &out_trade_no,
            amount_cents,
            subject,
            &timestamp,
            signer.as_ref(),
        )?;
        let headers = vec![(
            "Content-Type".to_string(),
            outgoing.content_type.to_string(),
        )];
        let raw = self.post(&outgoing.url, &outgoing.body, headers)?;
        let mut probe = parse_precreate_response(&raw, &out_trade_no, verifier.as_ref())?;
        // 请求原文随探针带回(W-52 取证档用;parse 层拿不到请求,这里补上)。
        probe.request_url = outgoing.url;
        probe.request_body = outgoing.body;
        Ok(probe)
    }
}

fn map_transport_failure(failure: HttpFailure) -> PaymentError {
    if failure.timeout {
        PaymentError::Timeout(failure.message)
    } else {
        match failure.status {
            Some(status) => PaymentError::Http {
                status,
                message: failure.message,
            },
            None => PaymentError::BadResponse(format!("无状态的传输故障: {}", failure.message)),
        }
    }
}

impl std::fmt::Debug for AlipayBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlipayBackend")
            .field("endpoint", &self.endpoint)
            .field("credentials", &self.credentials)
            .field("real_config", &self.real_config)
            .field("signer", &self.signer.as_ref().map(|_| "<已接入>"))
            .field("verifier", &self.verifier.as_ref().map(|_| "<已接入>"))
            .finish()
    }
}

impl PaymentChannel for AlipayBackend {
    fn trigger_pay(&mut self, request: &PayRequest) -> Result<PayResult, PaymentError> {
        request.validate()?;
        // clone 出配置再进真实路径:避免 &mut self 与 config 借用冲突
        // (配置仅含非密钥标识,克隆廉价)。
        match self.real_config.clone() {
            None => {
                // mock 路径(W-11 本地契约)。
                let raw = self.call(&self.pay_body(request))?;
                #[derive(Deserialize)]
                struct MockPayResponse {
                    trade_no: String,
                    status: PayStatus,
                    amount_cents: u64,
                }
                let parsed: MockPayResponse = serde_json::from_str(&raw).map_err(|e| {
                    PaymentError::BadResponse(format!(
                        "trigger_pay 响应解析失败: {e};原文: {raw:.200}"
                    ))
                })?;
                if parsed.trade_no.trim().is_empty() {
                    return Err(PaymentError::BadResponse(
                        "trigger_pay 返回了空 trade_no".to_string(),
                    ));
                }
                if parsed.amount_cents != request.amount_cents {
                    return Err(PaymentError::BadResponse(format!(
                        "渠道回传金额与请求不符:请求 {} 分 / 渠道 {} 分",
                        request.amount_cents, parsed.amount_cents
                    )));
                }
                Ok(PayResult {
                    out_request_no: request.out_request_no(),
                    trade_no: parsed.trade_no,
                    status: parsed.status,
                    amount_cents: parsed.amount_cents,
                })
            }
            Some(cfg) => self.trigger_pay_real(&cfg, request),
        }
    }
}
