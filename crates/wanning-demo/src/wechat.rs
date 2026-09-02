//! 微信支付 adapter(免密代扣 = **委托代扣 papay**):[`WechatBackend`],
//! [`PaymentChannel`] trait 的第二个实现(W-27)。
//!
//! **今晚不碰真端点**:所有测试打本地 mock(tests/common 的 TcpListener);
//! 真端点调用必须先过 W-07 护栏([`crate::guard`])。
//!
//! 报文口径(零编造,两级标注,全文见 `docs/research/wechat-daikou.md` W-24 调研):
//! - **[直核] 可落代码的**:受理扣款接口 = `POST /v3/papay/pay/transactions/apply`,
//!   必填 `appid` / `out_trade_no` / `description` / `transaction_notify_url` /
//!   `contract_id` / `amount{total(分,整数), currency(仅 CNY)}`;系统受理后
//!   **异步扣款**,结果走 `transaction_notify_url` 回调;「扣费失败可再次调用本接口
//!   发起重试扣费……直到扣费成功或者可扣费期结束」→ 重试用同一 `out_trade_no`
//!   (幂等键不变,见测试);回调交易类型枚举 **PAP:委托代扣**、状态枚举
//!   **SUCCESS:支付成功**。这些语义在下面直接落成代码与测试。
//! - **本地 mock 契约**(wanning-demo 自定义,不是微信报文):响应体/回调体的
//!   字段结构与 `trade_state` 的取值(`pending`/`success`/`failed`)。真实响应体
//!   字段本次调研**未直核**,绝不臆造,账户开通后按官方文档替换(见 TODO 2)。
//! - **错误码**:调研直核到的 400/401/403/429/500 清单来自「预约扣费」API 页面
//!   (服务商版),**不是**受理扣款接口的错误码清单——绝不挪用;非 2xx 一律
//!   `Http{status}` 上抛,语义映射列 TODO 3。
//!
//! 合规边界(docs/compliance-redlines.md)——**先读这里再动这个文件**:
//! - 免密代扣 = **协议内扣款**:用户本人与微信侧签约拿委托代扣协议 ID
//!   `contract_id`,扣款凭协议发起。**没有 `contract_id` 的扣款是裸转账**,本
//!   adapter 从构造到发单四道门都强制有协议才走(fail-closed);
//! - 资金零沉淀:Wanning 不碰钱、不代收代付、不做二清(刑事红线,无豁免);
//!   资金流走微信侧既有协议产品,从签约到扣款都在官方协议产品内。
//!
//! TODO(账户开通后)清单(见各处注释):
//! 1. V3 请求签名与**回调验签**:微信 V3 的证书/公钥机制本次调研未直核细节,
//!    绝不臆造;验签不过的报文绝不进入 [`crate::channel::apply_pay_notify`](fail-closed)
//! 2. 受理扣款响应体与支付结果通知的真实字段映射(替换本地 mock 契约;
//!    真实 `trade_state` 枚举全集本次仅直核 SUCCESS)
//! 3. 错误码语义映射(以官方受理扣款文档为准,不挪用预约扣费清单)
//! 4. `out_trade_no` 的长度/字符约束(调研未直核;当前复用共用幂等键派生)
//! 5. 渠道侧**解约**接口(对应 kill switch 的渠道半边;调研仅 [摘要] 级)——
//!    是否进 trait 面与老板确认后再扩,不在本次臆造
//! 6. 多用户 `contract_id` 映射(demo 单用户:构造时配置一份;多用户 = 按
//!    delegation 映射,接线时扩展)
//! 7. 护栏凭证按渠道拆分(W-07 护栏是真实消费总闸,密钥清单含京东/智谱;
//!    微信商户凭证账户开通后接入,届时与老板确认清单拆分方式)

use std::fmt;
use std::sync::Arc;

use serde::Deserialize;

use crate::channel::{PayRequest, PayResult, PayStatus, PaymentChannel, PaymentError};
use crate::guard::{check_real_spend, EnvSnapshot, RealSpendConfig};
use crate::http::{ApiTransport, HttpFailure, UreqApiTransport};

/// 微信支付 adapter 骨架(委托代扣 papay;协议内扣款,不是裸转账——见模块文档)。
///
/// `Debug` 手写:[`RealSpendConfig`] 自带打码,`contract_id` 是**用户授权凭证**
/// (拿住就能对用户发起扣款),同样绝不进 Debug(有测试)。
pub struct WechatBackend {
    endpoint: String,
    appid: String,
    /// 委托代扣协议 ID:用户本人签约后微信返回。没有它绝不发起扣款。
    contract_id: String,
    /// 支付结果回调地址(`transaction_notify_url`,受理扣款必填字段)。
    notify_url: String,
    credentials: RealSpendConfig,
    transport: Arc<dyn ApiTransport + Send + Sync>,
}

impl fmt::Debug for WechatBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WechatBackend")
            .field("endpoint", &self.endpoint)
            .field("appid", &self.appid)
            .field("contract_id", &"***(打码)")
            .field("notify_url", &self.notify_url)
            .field("credentials", &self.credentials)
            .field("transport", &self.transport)
            .finish()
    }
}

impl WechatBackend {
    /// 本地 mock 用:测试假凭证 + 本地地址(零外网)。
    pub fn new_mock(endpoint: &str, transport: Arc<dyn ApiTransport + Send + Sync>) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            appid: "wx-mock-appid".to_string(),
            contract_id: "mock-contract-id".to_string(),
            notify_url: "https://mock.local/notify".to_string(),
            credentials: RealSpendConfig {
                glm_key: String::new(),
                jd_app_key: "mock-app-key".to_string(),
                jd_app_secret: "mock-app-secret".to_string(),
                jd_access_token: "mock-access-token".to_string(),
            },
            transport,
        }
    }

    /// 真实路径:护栏(env 全齐)→ 端点 → appid → **签约协议** → 回调地址 → ureq。
    /// **任何一步缺失即拒**;今晚微信账户未开通,必然停在护栏或配置缺失上。
    /// 配置门一条不少的原因:无 `contract_id` 的扣款是裸转账(见模块文档合规边界)。
    pub fn from_snapshot_real(env: &EnvSnapshot) -> Result<Self, PaymentError> {
        let credentials = check_real_spend(env)
            .map_err(|denied| PaymentError::GuardBlocked(denied.to_string()))?;

        let endpoint = required_env(env, "WANNING_WECHAT_ENDPOINT", |name| {
            format!(
                "缺少 {name}:微信支付受理扣款网关(api.mch.weixin.qq.com)与产品形态以 \
                 W-24 调研为准,账户开通前不臆造"
            )
        })?;
        let appid = required_env(env, "WANNING_WECHAT_APPID", |name| {
            format!("缺少 {name}:受理扣款必填字段(W-24 [直核]),账户开通前不臆造")
        })?;
        let contract_id = required_env(env, "WANNING_WECHAT_CONTRACT_ID", |name| {
            format!(
                "缺少 {name}:没有委托代扣协议(用户本人签约拿 contract_id)绝不发起扣款——\
                 协议内扣款,不是裸转账;协议由用户本人与微信侧签约,账户开通后经签约流程取得"
            )
        })?;
        let notify_url = required_env(env, "WANNING_WECHAT_NOTIFY_URL", |name| {
            format!("缺少 {name}:受理扣款必填字段 transaction_notify_url(W-24 [直核])")
        })?;

        Ok(Self {
            endpoint,
            appid,
            contract_id,
            notify_url,
            credentials,
            transport: Arc::new(UreqApiTransport),
        })
    }

    /// 从当前进程环境构建真实 adapter。
    pub fn from_env_real() -> Result<Self, PaymentError> {
        Self::from_snapshot_real(&EnvSnapshot::from_process_env())
    }

    // 报文:必填字段是 W-24 [直核];delegation_id/intent_nonce 是 mock 契约附加的
    // 审计关联(真实微信报文没有这两个字段,接入时改挂到商户侧单据/备注,TODO 2)。
    fn pay_body(&self, request: &PayRequest) -> String {
        serde_json::json!({
            "appid": self.appid,
            "out_trade_no": request.out_request_no(),
            "description": format!(
                "Wanning 订单 {order}(委托 {delegation}#{nonce})",
                order = request.order_id,
                delegation = request.delegation_id,
                nonce = request.intent_nonce
            ),
            "transaction_notify_url": self.notify_url,
            "contract_id": self.contract_id,
            "amount": { "total": request.amount_cents, "currency": "CNY" },
            "delegation_id": request.delegation_id,
            "intent_nonce": request.intent_nonce,
        })
        .to_string()
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        // TODO(账户开通后):换成微信 V3 要求的签名头(本次调研未直核签名机制细节,
        // 绝不臆造;目前 mock 契约用 Bearer 演示管线)。
        vec![(
            "Authorization".to_string(),
            format!("Bearer {}", self.credentials.jd_access_token),
        )]
    }

    fn call(&self, body: &str) -> Result<String, PaymentError> {
        let headers = self.auth_headers();
        self.transport
            .post_json(&self.endpoint, body, &headers)
            .map_err(|failure: HttpFailure| {
                if failure.timeout {
                    PaymentError::Timeout(failure.message)
                } else {
                    match failure.status {
                        Some(status) => PaymentError::Http {
                            status,
                            message: failure.message,
                        },
                        None => PaymentError::BadResponse(format!(
                            "无状态的传输故障: {}",
                            failure.message
                        )),
                    }
                }
            })
    }
}

/// 读必填配置项(fail-closed):缺失/空白即 `Config`,报错点名变量与原因。
fn required_env(
    env: &EnvSnapshot,
    name: &str,
    message: impl Fn(&str) -> String,
) -> Result<String, PaymentError> {
    env.get(name)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| PaymentError::Config(message(name)))
}

/// mock 契约的金额子对象(真实响应体字段未直核,TODO 2)。
#[derive(Deserialize)]
struct MockAmount {
    total: u64,
    currency: String,
}

/// mock 契约的受理响应(字段结构 wanning-demo 自定义,非微信报文)。
#[derive(Deserialize)]
struct MockApplyResponse {
    out_trade_no: String,
    transaction_id: String,
    trade_type: String,
    trade_state: String,
    amount: MockAmount,
}

/// mock 契约的 `trade_state` → 渠道无关 [`PayStatus`]。
/// 真实枚举全集未直核(仅 SUCCESS:[直核]),未知值一律拒,不臆造映射。
fn map_trade_state(trade_state: &str) -> Result<PayStatus, PaymentError> {
    match trade_state {
        "pending" => Ok(PayStatus::Pending),
        "success" | "SUCCESS" => Ok(PayStatus::Success),
        "failed" => Ok(PayStatus::Failed),
        other => Err(PaymentError::BadResponse(format!(
            "未知的 trade_state「{other}」:真实枚举全集未直核,拒绝臆造映射"
        ))),
    }
}

/// 解析**委托代扣支付结果通知**(回调报文 → 渠道无关 [`crate::channel::PayNotify`])。
///
/// 微信侧特有校验(fail-closed,不过就不产生 [`PayNotify`]):
/// - 交易类型必须是 **PAP(委托代扣)**——W-24 [直核] 的枚举;不是 PAP = 挂错渠道
///   或伪造,拒;
/// - `out_trade_no` / `transaction_id` 必须在场(否则无法对账);
/// - 币种必须是 CNY(W-24 [直核]:委托代扣目前仅支持 CNY)。
///
/// TODO(账户开通后):上游必须在调用前完成**验签**(TODO 1);本函数只做解析与
/// 上述字段校验,台账应用走 [`crate::channel::apply_pay_notify`](幂等,渠道无关)。
pub fn parse_papay_notify(raw: &str) -> Result<crate::channel::PayNotify, PaymentError> {
    #[derive(Deserialize)]
    struct PapayNotifyReport {
        trade_type: String,
        out_trade_no: String,
        transaction_id: String,
        trade_state: String,
        amount: MockAmount,
    }

    let report: PapayNotifyReport = serde_json::from_str(raw).map_err(|e| {
        PaymentError::BadResponse(format!("委托代扣回调报文解析失败: {e};原文: {raw:.200}"))
    })?;

    if report.trade_type != "PAP" {
        return Err(PaymentError::BadResponse(format!(
            "回调交易类型不是委托代扣(PAP):{}(挂错渠道或伪造,拒绝应用)",
            report.trade_type
        )));
    }
    if report.out_trade_no.trim().is_empty() {
        return Err(PaymentError::BadResponse(
            "回调报文缺 out_trade_no,无法对账".to_string(),
        ));
    }
    if report.transaction_id.trim().is_empty() {
        return Err(PaymentError::BadResponse(
            "回调报文缺 transaction_id,无法对账".to_string(),
        ));
    }
    if report.amount.currency != "CNY" {
        return Err(PaymentError::BadResponse(format!(
            "回调币种不是 CNY(W-24 [直核]:委托代扣目前仅支持 CNY):{}(拒绝应用)",
            report.amount.currency
        )));
    }
    let status = map_trade_state(&report.trade_state)?;
    Ok(crate::channel::PayNotify {
        out_request_no: report.out_trade_no,
        trade_no: report.transaction_id,
        status,
        amount_cents: report.amount.total,
    })
}

impl PaymentChannel for WechatBackend {
    fn trigger_pay(&mut self, request: &PayRequest) -> Result<PayResult, PaymentError> {
        request.validate()?;

        // 防御性检查(两个构造器都已强制):无签约协议的扣款 = 裸转账,永远多挡一道。
        if self.contract_id.trim().is_empty() {
            return Err(PaymentError::Config(
                "缺少委托代扣协议 contract_id:没有签约协议的扣款是裸转账,绝不发起".to_string(),
            ));
        }

        let raw = self.call(&self.pay_body(request))?;
        let parsed: MockApplyResponse = serde_json::from_str(&raw).map_err(|e| {
            PaymentError::BadResponse(format!("trigger_pay 响应解析失败: {e};原文: {raw:.200}"))
        })?;

        // 对账四门(fail-closed,受理 ≠ 放心):
        if parsed.out_trade_no != request.out_request_no() {
            return Err(PaymentError::BadResponse(format!(
                "受理响应与请求的幂等键不符:请求 {} / 渠道 {}",
                request.out_request_no(),
                parsed.out_trade_no
            )));
        }
        if parsed.transaction_id.trim().is_empty() {
            return Err(PaymentError::BadResponse(
                "trigger_pay 返回了空 transaction_id".to_string(),
            ));
        }
        if parsed.trade_type != "PAP" {
            return Err(PaymentError::BadResponse(format!(
                "受理响应交易类型不是委托代扣(PAP):{}(挂错渠道,拒绝受理)",
                parsed.trade_type
            )));
        }
        if parsed.amount.total != request.amount_cents {
            return Err(PaymentError::BadResponse(format!(
                "渠道回传金额与请求不符:请求 {} 分 / 渠道 {} 分",
                request.amount_cents, parsed.amount.total
            )));
        }
        if parsed.amount.currency != "CNY" {
            return Err(PaymentError::BadResponse(format!(
                "渠道回传币种不是 CNY(W-24 [直核]:委托代扣目前仅支持 CNY):{}",
                parsed.amount.currency
            )));
        }
        let status = map_trade_state(&parsed.trade_state)?;

        Ok(PayResult {
            out_request_no: request.out_request_no(),
            trade_no: parsed.transaction_id,
            status,
            amount_cents: parsed.amount.total,
        })
    }
}
