//! 支付宝 adapter:[`AlipayBackend`] 骨架(PaymentChannel trait 的第一个实现)。
//!
//! **今晚不碰真端点**:所有测试打本地 mock(tests/common 的 TcpListener);
//! 真端点调用必须先过 W-07 护栏([`crate::guard`]),真实报文与产品形态以 W-13 调研为准。
//!
//! 合规边界(docs/compliance-redlines.md)——**先读这里再动这个文件**:
//! - 免密代扣的语义是「**协议内扣款**」:用户事先与收款方签约(代扣/委托扣款协议),
//!   扣款发生在协议约定的额度与场景之内。**它不是无协议的裸转账**,Wanning 绝不
//!   实现、也不封装任何绕过签约协议的转账能力;
//! - 资金零沉淀:Wanning 不碰钱、不代收代付、不做二清(刑事红线,无豁免);
//!   授权动作走闸([`crate::guard`] + wanning-core 的 delegation/gate),资金流走
//!   支付宝侧既有通道,从签约到扣款都在官方协议产品内;
//! - 本模块的报文是**本地 mock 契约**(wanning-demo 自定义字段),不是支付宝报文;
//!   真实产品/接口/字段名在 W-13 调研 + 账户开通后填充,绝不臆造。
//!
//! 共用类型(trait/请求/回调幂等/错误)在 [`crate::channel`];这里 `pub use` 再导出,
//! 既有引用路径(`wanning_demo::alipay::…`,W-11 测试与 whitepaper 引用)保持有效。
//!
//! TODO(账户开通后)清单(见各处注释):
//! 1. 真实产品形态与网关:周期扣款在支付宝开放平台的对应产品与接口名(W-13 调研)
//! 2. 请求签名与**回调验签**:验签不过的报文绝不进入 [`crate::channel::apply_pay_notify`](fail-closed)。
//!    签名/验签**槽位已备**([`crate::signing`] 的 [`crate::signing::MessageSigner`] /
//!    [`crate::signing::SignatureVerifier`],W-28):规范化规则是本地 mock 契约,
//!    账户开通后逐条与官方《接口签名规范》核对再接真密钥(env 注入位,零接触真密钥)
//! 3. trigger_pay 的真实请求/响应字段映射(替换本地 mock 契约)
//! 4. 错误码语义映射(协议不存在/超限/风控等;以官方文档为准,不臆造错误码)

use std::sync::Arc;

use serde::Deserialize;

use crate::guard::{check_real_spend, EnvSnapshot, RealSpendConfig};
use crate::http::{ApiTransport, HttpFailure, UreqApiTransport};

// 共用类型从 [`crate::channel`] 引入并**整体再导出**:既有引用路径
// (`wanning_demo::alipay::…`,W-11 测试与 whitepaper 引用)保持有效。
pub use crate::channel::{
    apply_pay_notify, PayNotify, PayRequest, PayResult, PayStatus, PaymentChannel, PaymentError,
    TradeState,
};

/// 支付宝 adapter 骨架(免密代扣 = **协议内扣款**,不是裸转账——见模块文档)。
///
/// `Debug` 由 derive 生成,凭证在 [`RealSpendConfig`] 里是手写打码的,打印不泄密(有测试)。
#[derive(Debug)]
pub struct AlipayBackend {
    endpoint: String,
    credentials: RealSpendConfig,
    transport: Arc<dyn ApiTransport + Send + Sync>,
}

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
        }
    }

    /// 真实路径:护栏(env 全齐)→ 真端点 env → ureq 传输。
    /// **任何一步缺失即拒**;今晚支付宝账户未开通,必然停在护栏或端点缺失上。
    pub fn from_snapshot_real(env: &EnvSnapshot) -> Result<Self, PaymentError> {
        let credentials = check_real_spend(env)
            .map_err(|denied| PaymentError::GuardBlocked(denied.to_string()))?;
        let endpoint = env
            .get("WANNING_ALIPAY_ENDPOINT")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                PaymentError::Config(
                    "缺少 WANNING_ALIPAY_ENDPOINT:支付宝真实网关 URL 与产品形态以 W-13 调研为准,账户开通前不臆造"
                        .to_string(),
                )
            })?
            .to_string();
        Ok(Self {
            endpoint,
            credentials,
            transport: Arc::new(UreqApiTransport),
        })
    }

    /// 从当前进程环境构建真实 adapter。
    pub fn from_env_real() -> Result<Self, PaymentError> {
        Self::from_snapshot_real(&EnvSnapshot::from_process_env())
    }

    // TODO(账户开通后,W-13 调研):替换为真实产品/接口名/参数/签名。
    // 下面 body 与响应解析用的是**本地 mock 契约**(wanning-demo 自定义字段),
    // 仅用于把传输/错误映射/幂等/解析的管线测起来,不代表支付宝报文。
    fn pay_body(&self, request: &PayRequest) -> String {
        serde_json::json!({
            "out_request_no": request.out_request_no(),
            "order_id": request.order_id,
            "amount_cents": request.amount_cents,
            "delegation_id": request.delegation_id,
            "intent_nonce": request.intent_nonce,
            // 占位标记「协议内扣款」语义;真实产品/字段名以 W-13 调研为准。
            "scene": "agreement"
        })
        .to_string()
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        // TODO(账户开通后):换成支付宝要求的签名/网关头(目前 mock 契约用 Bearer 演示管线)。
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

impl PaymentChannel for AlipayBackend {
    fn trigger_pay(&mut self, request: &PayRequest) -> Result<PayResult, PaymentError> {
        request.validate()?;

        let raw = self.call(&self.pay_body(request))?;
        #[derive(Deserialize)]
        struct MockPayResponse {
            trade_no: String,
            status: PayStatus,
            amount_cents: u64,
        }

        let parsed: MockPayResponse = serde_json::from_str(&raw).map_err(|e| {
            PaymentError::BadResponse(format!("trigger_pay 响应解析失败: {e};原文: {raw:.200}"))
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
}
