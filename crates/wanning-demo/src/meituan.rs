//! 美团 adapter:**契约占位**(W-39,依赖 W-38 调研结论)。
//!
//! **为什么是占位**:W-38 四问调研(docs/research/meituan-openplatform.md)结论——
//! ①查不到美团有面向第三方的**用户侧**免密/代扣开放 API(两轮独立检索 + GitHub
//! 检索,唯一「免密支付/委托代扣 API」文档命中是微信支付的);②美团支付相关开放
//! 能力的真实形态 = 收银/收单/金融 ISV + 收银硬件(**商户收银台方向**,与 C 端
//! agent 消费语义正交);③外卖 API 面(第三方 SDK 镜像,标 [摘要]:门店/类目/
//! 菜品/订单/退款/配送)是**商户 ERP 授权**方向(替商家管单,不是替用户花钱),
//! 官方原文文档全在 JS 渲染的 SPA 后面,**没有任何 [直核] 报文字段**。
//!
//! **为什么照建**:渠道矩阵结构完整性(四渠道 trait 面统一,见任务书 W-39 验收);
//! 将来「老板拿 ISV 资质」或「美团公布用户侧支付 API」触发再评估时,传输/错误
//! 映射/解析/护栏管线已经是测好的,只需换契约体。
//!
//! 合规边界:本模块报文是**本地 mock 契约**(wanning-demo 自定义字段,连第三方
//! SDK 镜像的字段名都不借用——[摘要] 不当字段依据);真路径必须过 W-07 护栏
//! ([`crate::guard`]),且真实端点 URL **不存在**(调研结论),端点 env 只留给
//! 将来评估,绝不臆造。
//!
//! TODO(再评估触发后)清单:
//! 1. 确认美团侧对口产品线(用户侧支付授权 vs 商户 ERP 授权——W-38 结论是后者,
//!    若仍是后者则本 trait 面本身不对口,应过任务书重新立项而不是填字段);
//! 2. 官方 API 网关 URL、签名算法、凭证字段(当前 [`RealSpendConfig`] 里没有
//!    美团专属字段,是刻意不臆造);
//! 3. search/create_order 真实请求/响应字段映射;
//! 4. 错误码语义映射。

use std::sync::Arc;

use serde::Deserialize;

use crate::guard::{check_real_spend, EnvSnapshot, RealSpendConfig};
pub use crate::http::{ApiTransport as MeituanTransport, UreqApiTransport as UreqMeituanTransport};
use crate::jd::{
    BackendError, CommerceBackend, CreateOrderRequest, OrderRef, Product, SearchRequest,
};

/// 美团 adapter 骨架(**契约占位**,见模块文档)。
///
/// `Debug` 手写打码:凭证在 [`RealSpendConfig`] 里本来就打码(有 jd 侧测试实证),
/// 这里再加一层结构级测试锁端点/凭证不泄进日志面。
#[derive(Debug)]
pub struct MeituanBackend {
    endpoint: String,
    credentials: RealSpendConfig,
    transport: Arc<dyn MeituanTransport + Send + Sync>,
}

impl MeituanBackend {
    /// 本地 mock 用:测试假凭证 + 本地地址(零外网)。
    pub fn new_mock(endpoint: &str, transport: Arc<dyn MeituanTransport + Send + Sync>) -> Self {
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

    /// 真实路径:W-07 护栏(env 全齐)→ 端点 env → ureq 传输。
    ///
    /// 端点报错点名 W-38 结论:真实端点 URL **不存在**(查不到用户侧可达 API),
    /// `WANNING_MEITUAN_ENDPOINT` 只留给将来再评估,绝不臆造 URL。
    pub fn from_snapshot_real(env: &EnvSnapshot) -> Result<Self, BackendError> {
        let credentials = check_real_spend(env)
            .map_err(|denied| BackendError::GuardBlocked(denied.to_string()))?;
        let endpoint = env
            .get("WANNING_MEITUAN_ENDPOINT")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                BackendError::Config(
                    "缺少 WANNING_MEITUAN_ENDPOINT:W-38 调研结论=美团查不到面向第三方的\
                     用户侧免密/代扣开放 API(商户 ERP 授权方向与 C 端 agent 消费不对口),\
                     真实端点 URL 不存在,绝不臆造;再评估触发条件见 \
                     docs/research/meituan-openplatform.md 第四节"
                        .to_string(),
                )
            })?
            .to_string();
        Ok(Self {
            endpoint,
            credentials,
            transport: Arc::new(UreqMeituanTransport),
        })
    }

    /// 从当前进程环境构建真实 adapter。
    pub fn from_env_real() -> Result<Self, BackendError> {
        Self::from_snapshot_real(&EnvSnapshot::from_process_env())
    }

    // 本地 mock 契约体(wanning-demo 自定义字段;连第三方 SDK 镜像的字段名都不借,
    // [摘要] 不当字段依据——与 jd.rs 的「真实字段待调研」同一纪律,且更严:
    // 美团侧连「哪个产品线对口」都未定)。
    fn search_body(&self, request: &SearchRequest) -> String {
        serde_json::json!({
            "keyword": request.keyword,
            "max_price_cents": request.max_price_cents,
            "limit": request.limit,
        })
        .to_string()
    }

    fn order_body(&self, request: &CreateOrderRequest) -> String {
        serde_json::json!({
            "sku_id": request.sku_id,
            "quantity": request.quantity,
            "amount_cents": request.amount_cents,
            "delegation_id": request.delegation_id,
            "intent_nonce": request.intent_nonce,
        })
        .to_string()
    }

    fn call(&self, body: &str) -> Result<String, BackendError> {
        // TODO(再评估触发后):换成美团要求的签名头(mock 契约用 Bearer 演示管线)。
        let headers = vec![(
            "Authorization".to_string(),
            format!("Bearer {}", self.credentials.jd_access_token),
        )];
        self.transport
            .post_json(&self.endpoint, body, &headers)
            .map_err(|failure| {
                if failure.timeout {
                    BackendError::Timeout(failure.message)
                } else {
                    match failure.status {
                        Some(status) => BackendError::Http {
                            status,
                            message: failure.message,
                        },
                        None => BackendError::BadResponse(format!(
                            "无状态的传输故障: {}",
                            failure.message
                        )),
                    }
                }
            })
    }
}

impl CommerceBackend for MeituanBackend {
    fn search(&mut self, request: &SearchRequest) -> Result<Vec<Product>, BackendError> {
        if request.keyword.trim().is_empty() {
            return Err(BackendError::InvalidRequest("keyword 不能为空".to_string()));
        }
        if request.limit == 0 {
            return Err(BackendError::InvalidRequest("limit 必须为正".to_string()));
        }

        let raw = self.call(&self.search_body(request))?;
        #[derive(Deserialize)]
        struct MockSearchResponse {
            products: Vec<MockProduct>,
        }
        #[derive(Deserialize)]
        struct MockProduct {
            sku_id: String,
            title: String,
            price_cents: u64,
            merchant_id: String,
        }

        let parsed: MockSearchResponse = serde_json::from_str(&raw).map_err(|e| {
            BackendError::BadResponse(format!("search 响应解析失败: {e};原文: {raw:.200}"))
        })?;
        Ok(parsed
            .products
            .into_iter()
            .map(|p| Product {
                sku_id: p.sku_id,
                title: p.title,
                price_cents: p.price_cents,
                merchant_id: p.merchant_id,
            })
            .collect())
    }

    fn create_order(&mut self, request: &CreateOrderRequest) -> Result<OrderRef, BackendError> {
        request.validate()?;

        let raw = self.call(&self.order_body(request))?;
        #[derive(Deserialize)]
        struct MockOrderResponse {
            order_id: String,
            sku_id: String,
            amount_cents: u64,
        }

        let parsed: MockOrderResponse = serde_json::from_str(&raw).map_err(|e| {
            BackendError::BadResponse(format!("create_order 响应解析失败: {e};原文: {raw:.200}"))
        })?;
        if parsed.order_id.trim().is_empty() {
            return Err(BackendError::BadResponse(
                "create_order 返回了空 order_id".to_string(),
            ));
        }
        Ok(OrderRef {
            order_id: parsed.order_id,
            sku_id: parsed.sku_id,
            amount_cents: parsed.amount_cents,
        })
    }
}
