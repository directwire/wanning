//! 决策回路:谁在提出消费意图、闸怎么接住它。
//!
//! 两个 [`DecisionSource`] 实现:
//! - [`ScriptedSource`]——内置确定性离线脚本(下单→超额→撤销后下单),输出必须标注
//!   「离线脚本场景」;今晚所有演示走它。
//! - [`GlmSource`]——智谱 GLM(chat/completions),把闸的当前状态交给模型,要求返回
//!   结构化意图 JSON;解析失败重试 1 次后报错,**绝不编造意图**。今晚无 key:只做实现
//!   + 本地 mock server 测试,真调路径被 W-07 护栏挡住。
//!
//! 安全设计(与闸的 fail-closed 同源):**delegation_id 与 nonce 由闸侧注入,模型无权
//! 指定**——模型只决定金额/商户/类别/备注,越权字段一律以闸侧为准。
//! 端点与响应形状依据智谱官方文档(2026-09 核实):
//! `POST https://open.bigmodel.cn/api/paas/v4/chat/completions`,`Authorization: Bearer <key>`,
//! 回复取 `choices[0].message.content`。

use std::sync::Arc;

use serde_json::Value;

use wanning_core::error::CoreError;
use wanning_core::gate::{DenyReason, GateDecision};
use wanning_core::intent::SpendIntent;
use wanning_core::state::WanningState;

use crate::guard::EnvSnapshot;

// ---------------------------------------------------------------------------
// 决策源 trait 与上下文
// ---------------------------------------------------------------------------

/// 决策源失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionError {
    /// 脚本走完(正常结束,不是故障)。
    Exhausted,
    /// 来源故障(网络/HTTP/解析)。**绝不编造意图顶替**。
    Source(String),
}

impl std::fmt::Display for DecisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecisionError::Exhausted => write!(f, "决策源已无更多意图(脚本走完)"),
            DecisionError::Source(m) => write!(f, "决策源故障: {m}"),
        }
    }
}

impl std::error::Error for DecisionError {}

/// 上一笔意图的判定结果(喂回决策源,让它「看得见」闸说了什么)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastOutcome {
    pub allowed: bool,
    pub deny_reason: Option<DenyReason>,
}

/// 决策源可见的闸状态快照(只读、最小化;模型/脚本据此决定下一笔)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionContext {
    pub delegation_id: String,
    pub budget_cap_cents: u64,
    pub spent_cents: u64,
    pub remaining_cents: u64,
    /// 闸侧分配的 nonce(决策源无权自选)。
    pub next_nonce: u64,
    /// 本笔是第几笔意图(0-based)。
    pub step_index: usize,
    pub last_outcome: Option<LastOutcome>,
}

/// 决策源:提出下一笔消费意图。
pub trait DecisionSource {
    /// 来源名(输出必须标注,离线/在线一目了然)。
    fn name(&self) -> &'static str;

    /// 下一笔意图。脚本走完返回 [`DecisionError::Exhausted`];来源故障返回
    /// [`DecisionError::Source`]——调用方不得把它当成「没有要花的」而静默继续。
    fn next_intent(&mut self, ctx: &DecisionContext) -> Result<SpendIntent, DecisionError>;
}

// ---------------------------------------------------------------------------
// 决策回路(闸 + 决策源 + 可选 kill switch 演示点)
// ---------------------------------------------------------------------------

/// 回路配置。
#[derive(Debug, Clone)]
pub struct LoopConfig {
    pub delegation_id: String,
    /// 最多几笔意图(防失控;决策源提前 Exhausted 就自然结束)。
    pub max_steps: usize,
    /// 第 N 笔意图判定完成后执行 kill switch(老板收权演示)。None = 不撤销。
    pub revoke_after_n_intents: Option<usize>,
}

/// 回路里的一步(审计形状,渲染成时间线)。
#[derive(Debug, Clone)]
pub enum StepEvent {
    Spend {
        intent: SpendIntent,
        decision: GateDecision,
        /// 该判定落审计的 WAL 行号(1-based)。
        wal_line: u64,
    },
    /// 老板收权(kill switch)——撤销是授权者的动作,agent 无权发起。
    BossRevoke {
        delegation_id: String,
        wal_line: u64,
    },
}

impl StepEvent {
    pub fn wal_line(&self) -> u64 {
        match self {
            StepEvent::Spend { wal_line, .. } | StepEvent::BossRevoke { wal_line, .. } => *wal_line,
        }
    }
}

/// 回路结果。
#[derive(Debug)]
pub struct LoopReport {
    pub source_name: &'static str,
    pub events: Vec<StepEvent>,
    /// 决策源是否自然走完(false = 被回路上限截断)。
    pub exhausted_naturally: bool,
}

/// 回路错误(核心错误 / 决策源故障)。
#[derive(Debug)]
pub enum LoopError {
    Core(CoreError),
    Decision(DecisionError),
}

impl std::fmt::Display for LoopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoopError::Core(e) => write!(f, "{e}"),
            LoopError::Decision(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LoopError {}

impl From<CoreError> for LoopError {
    fn from(e: CoreError) -> Self {
        LoopError::Core(e)
    }
}

impl From<DecisionError> for LoopError {
    fn from(e: DecisionError) -> Self {
        LoopError::Decision(e)
    }
}

/// 跑决策回路:构建上下文 → 决策源出意图 → 闸判定(先审计后扣账)→ 可选 kill switch。
pub fn run_decision_loop(
    state: &mut WanningState,
    source: &mut dyn DecisionSource,
    config: &LoopConfig,
) -> Result<LoopReport, LoopError> {
    // 回路强制要求审计落盘:没有审计的决策回路不该存在(fail-closed)。
    if state.wal_path().is_none() {
        return Err(LoopError::Core(CoreError::WalIo(
            "决策回路要求挂 WAL(每笔决策必须落审计),拒绝在无审计状态下跑".to_string(),
        )));
    }

    let budget_cap_cents = match state.gate().delegation(&config.delegation_id) {
        Some(delegation) => delegation.budget_cap_cents,
        None => {
            return Err(LoopError::Core(CoreError::UnknownDelegation(
                config.delegation_id.clone(),
            )))
        }
    };

    let mut events: Vec<StepEvent> = Vec::new();
    let mut exhausted_naturally = false;

    for step_index in 0..config.max_steps {
        let spent_cents = state.gate().spent_cents(&config.delegation_id).unwrap_or(0);
        let last_outcome = match events.last() {
            Some(StepEvent::Spend { decision, .. }) => Some(LastOutcome {
                allowed: decision.is_allow(),
                deny_reason: decision.deny_reason(),
            }),
            _ => None,
        };
        let ctx = DecisionContext {
            delegation_id: config.delegation_id.clone(),
            budget_cap_cents,
            spent_cents,
            remaining_cents: budget_cap_cents.saturating_sub(spent_cents),
            next_nonce: spend_count(&events) as u64 + 1,
            step_index,
            last_outcome,
        };

        let intent = match source.next_intent(&ctx) {
            Ok(intent) => intent,
            Err(DecisionError::Exhausted) => {
                exhausted_naturally = true;
                break;
            }
            Err(e) => return Err(LoopError::Decision(e)),
        };

        let decision = state.decide(&intent)?;
        let wal_line = state.last_wal_line().expect("挂了 WAL 必有行号");
        events.push(StepEvent::Spend {
            intent,
            decision,
            wal_line,
        });

        if let Some(after_n) = config.revoke_after_n_intents {
            if spend_count(&events) == after_n {
                let delegation_id = config.delegation_id.clone();
                state.revoke(&delegation_id)?;
                let wal_line = state.last_wal_line().expect("挂了 WAL 必有行号");
                events.push(StepEvent::BossRevoke {
                    delegation_id,
                    wal_line,
                });
            }
        }
    }

    Ok(LoopReport {
        source_name: source.name(),
        events,
        exhausted_naturally,
    })
}

/// 已发生的意图笔数(nonce 从 1 起单调递增)。
fn spend_count(events: &[StepEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, StepEvent::Spend { .. }))
        .count()
}

// ---------------------------------------------------------------------------
// 实现一:ScriptedSource(离线脚本场景)
// ---------------------------------------------------------------------------

/// 内置确定性离线脚本:下单 → 超额 → (老板收权) → 撤销后下单。
/// 输出必须标注「离线脚本场景」——这不是模型在决策,是排好的戏。
#[derive(Debug)]
pub struct ScriptedSource {
    label: &'static str,
    steps: Vec<SpendIntent>,
    cursor: usize,
}

impl ScriptedSource {
    /// 四卖点脚本(配套 [`LoopConfig::revoke_after_n_intents`] = 2)。
    pub fn selling_points_script(delegation_id: &str) -> Self {
        Self::new(
            "离线脚本场景(scripted,非模型决策)",
            vec![
                SpendIntent::new(
                    delegation_id,
                    0, // nonce 由回路注入,这里占位
                    500,
                    "jd:shop-1",
                    "grocery",
                    "四卖点①:预算内放行(¥5.00)",
                ),
                SpendIntent::new(
                    delegation_id,
                    0,
                    900,
                    "jd:shop-2",
                    "grocery",
                    "四卖点②:超额请求(累计 ¥14.00 > 上限 ¥10.00)",
                ),
                SpendIntent::new(
                    delegation_id,
                    0,
                    100,
                    "jd:shop-3",
                    "grocery",
                    "四卖点③:撤销后再请求(¥1.00)",
                ),
            ],
        )
    }

    pub fn new(label: &'static str, steps: Vec<SpendIntent>) -> Self {
        Self {
            label,
            steps,
            cursor: 0,
        }
    }
}

impl DecisionSource for ScriptedSource {
    fn name(&self) -> &'static str {
        self.label
    }

    fn next_intent(&mut self, ctx: &DecisionContext) -> Result<SpendIntent, DecisionError> {
        let mut intent = self
            .steps
            .get(self.cursor)
            .ok_or(DecisionError::Exhausted)?
            .clone();
        self.cursor += 1;
        // nonce 与 delegation_id 由闸侧注入,脚本里的占位一律覆盖。
        intent.nonce = ctx.next_nonce;
        intent.delegation_id = ctx.delegation_id.clone();
        Ok(intent)
    }
}

// ---------------------------------------------------------------------------
// 实现二:GlmSource(智谱 GLM,在线决策)
// ---------------------------------------------------------------------------

/// 智谱官方端点(2026-09 依据 docs.bigmodel.cn 核实;base URL 可 env 覆盖)。
pub const GLM_DEFAULT_BASE_URL: &str = "https://open.bigmodel.cn/api/paas/v4";
/// 默认模型(env `WANNING_GLM_MODEL` 可覆盖)。
pub const GLM_DEFAULT_MODEL: &str = "glm-4-flash";
/// 首次 + 重试 1 次 = 2 次;再失败就报错,不编造。
pub const GLM_MAX_ATTEMPTS: u32 = 2;

/// HTTP 传输故障。
#[derive(Debug, Clone)]
pub struct TransportError {
    /// HTTP 状态码(连接层故障时为 None)。
    pub status: Option<u16>,
    pub message: String,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => write!(f, "HTTP {status}: {message}", message = self.message),
            None => write!(f, "连接失败: {message}", message = self.message),
        }
    }
}

impl std::error::Error for TransportError {}

/// GLM HTTP 传输层:生产用 [`UreqTransport`],测试注入本地 mock server 或假传输。
pub trait ChatTransport: std::fmt::Debug {
    fn post_chat(
        &self,
        endpoint: &str,
        api_key: &str,
        request_body: &str,
    ) -> Result<String, TransportError>;
}

/// ureq 实现(真实路径;今晚被 W-07 护栏挡住,不会出网)。
#[derive(Debug)]
pub struct UreqTransport;

impl ChatTransport for UreqTransport {
    fn post_chat(
        &self,
        endpoint: &str,
        api_key: &str,
        request_body: &str,
    ) -> Result<String, TransportError> {
        let response = ureq::post(endpoint)
            .timeout(std::time::Duration::from_secs(30))
            .set("Authorization", &format!("Bearer {api_key}"))
            .set("Content-Type", "application/json")
            .send_string(request_body)
            .map_err(|e| {
                // ureq 2.x:HTTP 状态错误 = Error::Status(code, response);连接层 = Transport。
                let status = match &e {
                    ureq::Error::Status(code, _) => Some(*code),
                    ureq::Error::Transport(_) => None,
                };
                TransportError {
                    status,
                    message: e.to_string(),
                }
            })?;
        let status = response.status();
        response.into_string().map_err(|e| TransportError {
            status: Some(status),
            message: format!("读响应体失败: {e}"),
        })
    }
}

/// 智谱 GLM 决策源。
#[derive(Debug)]
pub struct GlmSource {
    api_key: String,
    base_url: String,
    model: String,
    transport: Arc<dyn ChatTransport + Send + Sync>,
}

impl GlmSource {
    /// 从护栏同一套 env 快照构建(`WANNING_GLM_KEY` 必填,base/model 可覆盖)。
    pub fn from_snapshot(env: &EnvSnapshot) -> Result<Self, DecisionError> {
        let api_key = env
            .get("WANNING_GLM_KEY")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                DecisionError::Source(
                    "缺少 WANNING_GLM_KEY(GLM 决策源拒绝在无密钥时构建,fail-closed)".to_string(),
                )
            })?
            .to_string();
        let base_url = env
            .get("WANNING_GLM_BASE_URL")
            .map(str::to_string)
            .unwrap_or_else(|| GLM_DEFAULT_BASE_URL.to_string());
        let model = env
            .get("WANNING_GLM_MODEL")
            .map(str::to_string)
            .unwrap_or_else(|| GLM_DEFAULT_MODEL.to_string());
        Ok(Self {
            api_key,
            base_url,
            model,
            transport: Arc::new(UreqTransport),
        })
    }

    /// 从当前进程环境构建。
    pub fn from_env() -> Result<Self, DecisionError> {
        Self::from_snapshot(&EnvSnapshot::from_process_env())
    }

    /// 测试/注入用:显式指定各部件。
    pub fn with_parts(
        api_key: &str,
        base_url: &str,
        model: &str,
        transport: Arc<dyn ChatTransport + Send + Sync>,
    ) -> Self {
        Self {
            api_key: api_key.to_string(),
            base_url: base_url.to_string(),
            model: model.to_string(),
            transport,
        }
    }

    pub fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// 组请求体(model + 两条 message)。
    fn request_body(&self, ctx: &DecisionContext) -> String {
        let system = "你是购物 agent 的决策模块。只输出一个 JSON 对象,禁止 markdown、禁止解释。\
                      字段:amount_cents(正整数,单位分)、merchant_id(字符串)、category(字符串)、\
                      memo(字符串,不超过 20 字)。金额不得超过剩余预算。";
        let mut user = format!(
            "当前委托 {} 的预算上限 {} 分,已消费 {} 分,剩余 {} 分。这是第 {} 笔消费意图。",
            ctx.delegation_id,
            ctx.budget_cap_cents,
            ctx.spent_cents,
            ctx.remaining_cents,
            ctx.step_index + 1
        );
        if let Some(last) = ctx.last_outcome {
            match (last.allowed, last.deny_reason) {
                (false, Some(reason)) => user.push_str(&format!(
                    "上一笔被闸拒绝,原因:{}。请调整金额。",
                    crate::scenario::deny_reason_zh(&reason)
                )),
                (false, None) => user.push_str("上一笔被闸拒绝,请调整金额。"),
                (true, _) => user.push_str("上一笔已放行。"),
            }
        }
        serde_json::json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ],
            "temperature": 0.2,
            "stream": false
        })
        .to_string()
    }

    /// 从回复里剥出 JSON 并解析成意图。delegation_id/nonce 由闸侧注入;
    /// amount 必须为正整数,merchant_id 必须非空。
    fn parse_intent(
        &self,
        content: &str,
        ctx: &DecisionContext,
    ) -> Result<SpendIntent, DecisionError> {
        let trimmed = content.trim();
        let json_text = match (trimmed.find('{'), trimmed.rfind('}')) {
            (Some(start), Some(end)) if end > start => &trimmed[start..=end],
            _ => {
                return Err(DecisionError::Source(format!(
                    "GLM 回复里没有 JSON 对象: {content:.200}"
                )))
            }
        };
        let value: Value = serde_json::from_str(json_text).map_err(|e| {
            DecisionError::Source(format!("GLM 回复不是合法 JSON: {e};原文: {content:.200}"))
        })?;

        let amount_cents = value
            .get("amount_cents")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                DecisionError::Source(format!("GLM 回复缺少正整数 amount_cents: {content:.200}"))
            })?;
        if amount_cents == 0 {
            return Err(DecisionError::Source(
                "GLM 给出 amount_cents=0,闸不会接受,拒绝编造".to_string(),
            ));
        }
        let merchant_id = value
            .get("merchant_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                DecisionError::Source(format!("GLM 回复缺少 merchant_id: {content:.200}"))
            })?
            .to_string();
        let category = value
            .get("category")
            .and_then(Value::as_str)
            .unwrap_or("unspecified")
            .to_string();
        let memo = value
            .get("memo")
            .and_then(Value::as_str)
            .unwrap_or("GLM 决策")
            .to_string();

        // 越权字段以闸侧为准:delegation_id、nonce 一律来自 ctx。
        Ok(SpendIntent::new(
            &ctx.delegation_id,
            ctx.next_nonce,
            amount_cents,
            &merchant_id,
            &category,
            &memo,
        ))
    }

    /// 尝试一次完整调用(请求 → 回复 → 意图)。
    fn attempt_once(&self, ctx: &DecisionContext) -> Result<SpendIntent, DecisionError> {
        let endpoint = self.endpoint();
        let body = self.request_body(ctx);
        let raw = self
            .transport
            .post_chat(&endpoint, &self.api_key, &body)
            .map_err(|e| DecisionError::Source(format!("GLM 调用失败({endpoint}): {e}")))?;

        let content = serde_json::from_str::<Value>(&raw)
            .map_err(|e| {
                DecisionError::Source(format!("GLM 响应不是合法 JSON: {e};原文: {raw:.200}"))
            })?
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                DecisionError::Source(format!(
                    "GLM 响应缺少 choices[0].message.content: {raw:.200}"
                ))
            })?
            .to_string();

        self.parse_intent(&content, ctx)
    }
}

impl DecisionSource for GlmSource {
    fn name(&self) -> &'static str {
        "GLM 在线决策(智谱 open.bigmodel.cn)"
    }

    fn next_intent(&mut self, ctx: &DecisionContext) -> Result<SpendIntent, DecisionError> {
        let mut last_error = None;
        for _attempt in 1..=GLM_MAX_ATTEMPTS {
            match self.attempt_once(ctx) {
                Ok(intent) => return Ok(intent),
                Err(e) => last_error = Some(e),
            }
        }
        Err(DecisionError::Source(format!(
            "GLM 决策失败({GLM_MAX_ATTEMPTS} 次尝试后),拒绝编造意图。最后一次错误: {}",
            last_error.expect("至少尝试过一次")
        )))
    }
}
