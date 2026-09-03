//! 支付通道共用类型:[`PaymentChannel`] trait、请求/结果/回调类型、回调幂等纯函数。
//!
//! 与 `http.rs` 同一先例:第二个支付通道出现(支付宝 W-11 → 微信 W-27),渠道无关的
//! 类型就从 `alipay.rs` 抽成共用层;**渠道差异(报文/端点/错误语义/验签)留在各自模块**,
//! 不为统一而过早抽象(决策记录)。
//!
//! 合规边界(docs/compliance-redlines.md)——**先读这里再动渠道 adapter**:
//! - 免密代扣的语义是「**协议内扣款**」:用户事先与收款方签约(代扣/委托扣款协议),
//!   扣款发生在协议约定的额度与场景之内。**它不是无协议的裸转账**,Wanning 绝不
//!   实现、也不封装任何绕过签约协议的转账能力;
//! - 资金零沉淀:Wanning 不碰钱、不代收代付、不做二清(刑事红线,无豁免);
//!   授权动作走闸([`crate::guard`] + wanning-core 的 delegation/gate),资金流走
//!   渠道侧既有协议产品,从签约到扣款都在官方协议产品内。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 支付通道 trait:Wanning 的自有接口(类型是本仓的,不绑任何渠道报文)。
/// 实现者:支付宝 [`crate::alipay::AlipayBackend`]、微信 [`crate::wechat::WechatBackend`]。
pub trait PaymentChannel {
    /// 对一笔订单发起「协议内扣款」。幂等键由 (delegation_id, intent_nonce, order_id)
    /// 派生——同一意图重复触发不会产生第二笔扣款(见各实现)。
    fn trigger_pay(&mut self, request: &PayRequest) -> Result<PayResult, PaymentError>;
}

// ---------------------------------------------------------------------------
// 渠道无关类型(最小面;金额一律 u64 分)
// ---------------------------------------------------------------------------

/// 扣款请求:一笔**已过闸**的消费意图对应的订单。
/// `delegation_id` + `intent_nonce` 是审计关联(意图 ↔ 订单 ↔ 扣款),必填。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayRequest {
    pub order_id: String,
    pub amount_cents: u64,
    pub delegation_id: String,
    /// 闸的 nonce 从 1 起;0 意味着这笔钱没有对应任何已放行意图,直接拒。
    pub intent_nonce: u64,
}

impl PayRequest {
    /// 发起扣款前的本地校验(fail-closed):非法请求不出网。
    pub fn validate(&self) -> Result<(), PaymentError> {
        if self.order_id.trim().is_empty() {
            return Err(PaymentError::InvalidRequest(
                "order_id 不能为空(扣款必须挂在一笔订单上)".to_string(),
            ));
        }
        if self.amount_cents == 0 {
            return Err(PaymentError::InvalidRequest(
                "amount_cents 必须为正(0 元扣款没有意义且不可审计)".to_string(),
            ));
        }
        if self.delegation_id.trim().is_empty() {
            return Err(PaymentError::InvalidRequest(
                "delegation_id 不能为空(扣款必须挂在一份授权下)".to_string(),
            ));
        }
        if self.intent_nonce == 0 {
            return Err(PaymentError::InvalidRequest(
                "intent_nonce 不能为 0(闸的 nonce 从 1 起)".to_string(),
            ));
        }
        Ok(())
    }

    /// 幂等键:由授权上下文 + 订单确定性派生。
    /// 同一 (委托, 意图, 订单) 无论重试多少次,键都相同 → 上游幂等,不重复扣款。
    pub fn out_request_no(&self) -> String {
        format!(
            "w-{delegation_id}-{nonce}-{order}",
            delegation_id = self.delegation_id,
            nonce = self.intent_nonce,
            order = self.order_id
        )
    }
}

/// 扣款状态。`Pending` 是发起后的常态:异步通道里,终态由回调通知(见
/// [`apply_pay_notify`]),不是同步返回。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayStatus {
    Pending,
    Success,
    Failed,
}

impl fmt::Display for PayStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            PayStatus::Pending => "pending(处理中)",
            PayStatus::Success => "success(成功)",
            PayStatus::Failed => "failed(失败)",
        };
        f.write_str(label)
    }
}

/// 发起扣款的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayResult {
    /// 幂等键(本地派生,回传用于对账)。
    pub out_request_no: String,
    /// 渠道侧交易号(本地 mock 契约;真实字段名以各渠道调研为准)。
    pub trade_no: String,
    pub status: PayStatus,
    pub amount_cents: u64,
}

/// 渠道侧回调通知(已解析;**原始报文必须先验签**,见各渠道模块 TODO)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayNotify {
    pub out_request_no: String,
    pub trade_no: String,
    pub status: PayStatus,
    pub amount_cents: u64,
}

impl PayNotify {
    /// 从渠道无关报文解析(支付宝 mock 契约用)。渠道特有校验(如微信的 PAP
    /// 交易类型)在各自模块做。TODO(账户开通后):本函数只做解析,上游必须在调用
    /// [`apply_pay_notify`] 前完成验签;验签不过的报文一律丢弃并告警(fail-closed)。
    pub fn parse(raw: &str) -> Result<Self, PaymentError> {
        let notify: PayNotify = serde_json::from_str(raw).map_err(|e| {
            PaymentError::BadResponse(format!("回调报文解析失败: {e};原文: {raw:.200}"))
        })?;
        if notify.out_request_no.trim().is_empty() {
            return Err(PaymentError::BadResponse(
                "回调报文缺 out_request_no,无法对账".to_string(),
            ));
        }
        if notify.trade_no.trim().is_empty() {
            return Err(PaymentError::BadResponse(
                "回调报文缺 trade_no,无法对账".to_string(),
            ));
        }
        Ok(notify)
    }
}

/// 本地交易台账:一笔扣款在 Wanning 侧的终态轨迹(回调幂等应用的对象)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeState {
    pub out_request_no: String,
    /// 发起扣款时渠道回传的交易号。
    pub trade_no: String,
    /// 订单金额(分);回调金额必须与此一致,不符即拒。
    pub amount_cents: u64,
    pub status: PayStatus,
}

/// 把一条回调通知**幂等地**应用到台账上。
///
/// 返回 `Ok(true)` = 台账状态前进了;`Ok(false)` = 重复通知,无变化(幂等);
/// `Err` = 通知与本笔交易对不上/状态回退/金额不符,**一律不改台账**(fail-closed)。
///
/// 这就是「重复回调幂等」的核心:同一条 Success 通知投递两次,第二次是 no-op,
/// 绝不会重复入账、也不会把已终态的交易改写回去。
pub fn apply_pay_notify(state: &mut TradeState, notify: &PayNotify) -> Result<bool, PaymentError> {
    if notify.out_request_no != state.out_request_no {
        return Err(PaymentError::BadResponse(format!(
            "回调与本地交易不对应:台账 {} / 回调 {}(拒绝应用,不改台账)",
            state.out_request_no, notify.out_request_no
        )));
    }
    if notify.amount_cents != state.amount_cents {
        return Err(PaymentError::BadResponse(format!(
            "回调金额与订单不符:台账 {} 分 / 回调 {} 分(拒绝应用,不改台账)",
            state.amount_cents, notify.amount_cents
        )));
    }

    use PayStatus::{Failed, Pending, Success};
    match (state.status, notify.status) {
        // 同态重复投递:幂等 no-op。(Failed, Pending) 是失败的迟到 stale 通知,
        // 不构成任何状态变化,同样按幂等 no-op 处理(不改台账)。
        (Pending, Pending) | (Failed, Failed) | (Failed, Pending) | (Success, Success) => {
            if state.status == Success && notify.trade_no != state.trade_no {
                return Err(PaymentError::BadResponse(format!(
                    "已成功的交易收到不同交易号的回调:台账 {} / 回调 {}(拒绝应用,需人工核对)",
                    state.trade_no, notify.trade_no
                )));
            }
            Ok(false)
        }
        // 终态推进。
        (Pending, Success) | (Pending, Failed) => {
            state.status = notify.status;
            Ok(true)
        }
        // 状态回退:合法通道里不该出现,拒(fail-closed)。
        (Failed, Success) | (Success, Pending) | (Success, Failed) => {
            Err(PaymentError::BadResponse(format!(
                "回调状态回退:{current} → {next}(拒绝应用,需人工核对)",
                current = state.status,
                next = notify.status
            )))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaymentError {
    /// 请求参数非法(本地 fail-closed,不出网)。
    InvalidRequest(String),
    /// 上游 HTTP 非 2xx。
    Http { status: u16, message: String },
    /// 连接/读超时。
    Timeout(String),
    /// 2xx 但响应不符合契约(今晚=本地 mock 契约),或回调对不上账。
    BadResponse(String),
    /// 真端点路径未过 W-07 护栏(fail-closed)。
    GuardBlocked(String),
    /// 配置缺失(如真端点/签约协议未知)。
    Config(String),
}

impl fmt::Display for PaymentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PaymentError::InvalidRequest(m) => write!(f, "请求参数非法(fail-closed): {m}"),
            PaymentError::Http { status, message } => write!(f, "上游 HTTP {status}: {message}"),
            PaymentError::Timeout(m) => write!(f, "上游超时: {m}"),
            PaymentError::BadResponse(m) => write!(f, "响应不符合契约: {m}"),
            PaymentError::GuardBlocked(m) => write!(f, "真实路径被护栏挡下(fail-closed): {m}"),
            PaymentError::Config(m) => write!(f, "adapter 配置缺失: {m}"),
        }
    }
}

impl std::error::Error for PaymentError {}
