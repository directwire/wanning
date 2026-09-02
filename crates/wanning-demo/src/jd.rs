//! 京东 adapter:[`CommerceBackend`] trait + [`JdBackend`] 骨架。
//!
//! **今晚不碰真端点**:所有测试打本地 mock(tests/common 的 TcpListener);
//! 真端点调用必须先过 W-07 护栏([`crate::guard`]),且真实报文格式以 W-12 调研为准。
//!
//! 合规边界(docs/compliance-redlines.md):
//! - 只走京东开放平台官方 API(企业购/商品/订单),**禁止 UI 自动化与爬虫**;
//! - 资金流不过闸的手:Wanning 只做意图授权与审计,支付走京东侧既有通道;
//! - 本模块描述的报文是**本地 mock 契约**(wanning-demo 自定义),不是京东报文;
//!   真实字段名在账户开通 + W-12 调研后填充,绝不臆造。
//!
//! TODO(账户开通后)清单(见各处注释):
//! 1. 真实网关 URL 与方法名(京东开放平台 201p 网关格式)
//! 2. 请求签名(app_key/secret 的签名算法)与 access_token 的获取/续期
//! 3. search/create_order 的真实请求/响应字段映射
//! 4. 错误码语义映射(限流/无权限/库存不足等)

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::guard::{check_real_spend, EnvSnapshot, RealSpendConfig};
pub use crate::http::{
    ApiTransport as JdTransport, HttpFailure, UreqApiTransport as UreqJdTransport,
};

/// 商城后端 trait:Wanning 的自有接口(类型是本仓的,不绑京东报文)。
pub trait CommerceBackend {
    fn search(&mut self, request: &SearchRequest) -> Result<Vec<Product>, BackendError>;
    fn create_order(&mut self, request: &CreateOrderRequest) -> Result<OrderRef, BackendError>;
}

// ---------------------------------------------------------------------------
// Wanning 自有类型(最小面;金额一律 u64 分)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub keyword: String,
    /// 预算约束:超过此价(分)的商品不进入候选。
    pub max_price_cents: Option<u64>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Product {
    /// 商品标识(京东侧 SKU,字段名以 W-12 调研为准)。
    pub sku_id: String,
    pub title: String,
    pub price_cents: u64,
    /// 商户/店铺标识(落闸审计的 merchant_id 来源)。
    pub merchant_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateOrderRequest {
    pub sku_id: String,
    pub quantity: u32,
    pub amount_cents: u64,
    /// 本单挂在哪份授权下(闸审计可回溯到委托)。
    pub delegation_id: String,
    /// 对应闸放行的意图 nonce(审计关联:意图 ↔ 订单)。
    pub intent_nonce: u64,
}

impl CreateOrderRequest {
    /// 下单前置校验(fail-closed):必填字段齐全、金额为正。
    pub fn validate(&self) -> Result<(), BackendError> {
        if self.sku_id.trim().is_empty() {
            return Err(BackendError::InvalidRequest("sku_id 不能为空".to_string()));
        }
        if self.quantity == 0 {
            return Err(BackendError::InvalidRequest(
                "quantity 必须为正".to_string(),
            ));
        }
        if self.amount_cents == 0 {
            return Err(BackendError::InvalidRequest(
                "amount_cents 必须为正(0 元下单没有意义且不可审计)".to_string(),
            ));
        }
        if self.delegation_id.trim().is_empty() {
            return Err(BackendError::InvalidRequest(
                "delegation_id 不能为空(订单必须挂在一份授权下)".to_string(),
            ));
        }
        if self.intent_nonce == 0 {
            return Err(BackendError::InvalidRequest(
                "intent_nonce 不能为 0(闸的 nonce 从 1 起)".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderRef {
    pub order_id: String,
    pub sku_id: String,
    pub amount_cents: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    /// 请求参数非法(本地 fail-closed,不出网)。
    InvalidRequest(String),
    /// 上游 HTTP 非 2xx。
    Http { status: u16, message: String },
    /// 连接/读超时。
    Timeout(String),
    /// 2xx 但响应不符合契约(今晚=本地 mock 契约)。
    BadResponse(String),
    /// 真端点路径未过 W-07 护栏(fail-closed)。
    GuardBlocked(String),
    /// 配置缺失(如真端点未知)。
    Config(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::InvalidRequest(m) => write!(f, "请求参数非法(fail-closed): {m}"),
            BackendError::Http { status, message } => write!(f, "上游 HTTP {status}: {message}"),
            BackendError::Timeout(m) => write!(f, "上游超时: {m}"),
            BackendError::BadResponse(m) => write!(f, "响应不符合契约: {m}"),
            BackendError::GuardBlocked(m) => write!(f, "真实路径被护栏挡下(fail-closed): {m}"),
            BackendError::Config(m) => write!(f, "adapter 配置缺失: {m}"),
        }
    }
}

impl std::error::Error for BackendError {}

// ---------------------------------------------------------------------------
// JdBackend(传输层见 crate::http,京东/支付宝共用)
// ---------------------------------------------------------------------------

/// 京东 adapter 骨架。
///
/// `Debug` 由 derive 生成,但凭证字段在 [`RealSpendConfig`] 里是手写打码的,
/// 所以打印 adapter 不会泄密(有测试实证)。
#[derive(Debug)]
pub struct JdBackend {
    endpoint: String,
    credentials: RealSpendConfig,
    transport: Arc<dyn JdTransport + Send + Sync>,
}

impl JdBackend {
    /// 本地 mock 用:测试假凭证 + 本地地址(零外网)。
    pub fn new_mock(endpoint: &str, transport: Arc<dyn JdTransport + Send + Sync>) -> Self {
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
    /// **任何一步缺失即拒**;今晚京东账户未开通,必然停在护栏或端点缺失上。
    pub fn from_snapshot_real(env: &EnvSnapshot) -> Result<Self, BackendError> {
        let credentials = check_real_spend(env)
            .map_err(|denied| BackendError::GuardBlocked(denied.to_string()))?;
        let endpoint = env
            .get("WANNING_JD_ENDPOINT")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                BackendError::Config(
                    "缺少 WANNING_JD_ENDPOINT:京东真实网关 URL 以 W-12 调研为准,账户开通前不臆造"
                        .to_string(),
                )
            })?
            .to_string();
        Ok(Self {
            endpoint,
            credentials,
            transport: Arc::new(UreqJdTransport),
        })
    }

    /// 从当前进程环境构建真实 adapter。
    pub fn from_env_real() -> Result<Self, BackendError> {
        Self::from_snapshot_real(&EnvSnapshot::from_process_env())
    }

    // TODO(账户开通后,W-12 调研):替换为京东开放平台真实方法名/参数/签名。
    // 下面两个 body/响应解析用的是**本地 mock 契约**(wanning-demo 自定义字段),
    // 仅用于把传输/错误映射/解析的管线测起来,不代表京东报文。
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

    fn auth_headers(&self) -> Vec<(String, String)> {
        // TODO(账户开通后):换成京东要求的签名头(目前 mock 契约用 Bearer 演示管线)。
        vec![(
            "Authorization".to_string(),
            format!("Bearer {}", self.credentials.jd_access_token),
        )]
    }

    fn call(&self, body: &str) -> Result<String, BackendError> {
        let headers = self.auth_headers();
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

impl CommerceBackend for JdBackend {
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
            #[serde(rename = "products")]
            products: Vec<MockProduct>,
        }
        #[derive(Deserialize)]
        struct MockProduct {
            #[serde(rename = "sku_id")]
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
