//! 人在环待支付(W-53a,第一形态)。
//!
//! 产品分层(所有者 2026-09-03 拍板):**第一形态 = 人在环待支付**(个人默认,
//! 零开户——AI 把单下到「待支付」,人按指纹确认);第二形式 = 免密代扣(平台侧,
//! 全自动,W-50 模板 + W-52 钥匙验证);`manual` = 纯闸,只判定不开单。
//!
//! **待支付单是账本里的一行状态,不是通道请求**:零网络、零外联、零通道 API。
//! 一笔人在环消费的五段事件链(全部落 WAL,一行不缺,回放页可逐段回放):
//!
//! ①意图 + ②审批 → 既有 `Decide` 行(意图与判定原子一行;闸判定面零改动)
//! ③待支付 → `Pending` 行(审批额 + TTL)
//! ④人确认 → `Confirm` 行(关联 pending id,幂等,带支付凭证)
//! ⑤终态 → `Terminal` 行(完成 / TTL 过期作废)
//!
//! 三钉(fail-closed,被拒的确认**一行都不落**):
//! 1. **金额一致**:确认额必须等于审批额——审批 400 确认 500 = 拒(防夹带,
//!    这是「限制 AI」的本体语义);
//! 2. **幂等**:同一单只能确认一次,二次确认 = 拒;
//! 3. **TTL**:待支付半开窗口 `[created, expires)`,过期作废,确认过期单 = 拒
//!    (作废本身是账本事实,落一行 `Terminal{ExpiredVoid}` 再拒)。
//!
//! 完整性不豁免:每一行都过 W-21 完整性链;回放侧([`crate::state::WanningState::replay`])
//! 与实时侧共用同一套 [`PendingLedger`] 应用逻辑——链合法但语义不通的账本
//! (没有放行的待支付、确认额与审批额不符、没有确认就完成)回放一律 fail-closed。
//!
//! 崩溃窗口的诚实边界:④确认行与⑤终态行之间崩溃,重放后单处于 `Confirmed`
//! 态(确认已发生、终态行缺失)——回放按「已确认未了结」接受,不视为损坏;
//! 补一行终态(或人重新确认会被幂等钉拒)即可对账。
//!
//! 金额一律 u64 分,禁浮点(钱,与闸同一纪律)。

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::intent::SpendIntent;

/// 支付形态档位(挂在接入面:W-53 产品分层)。
///
/// 档位只决定「闸放行之后做什么」,**不改闸判定面**——预算/撤销/重放/策略
/// 四道门在任何档位下都先到先拒。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayMode {
    /// 第一形态:人在环待支付(个人默认,零开户)。闸放行后开待支付单,
    /// 人确认后才算消费落地。
    #[default]
    PendingPay,
    /// 第二形式:免密代扣(平台侧,全自动)。闸放行即消费落地,通道半边走
    /// W-50 报文模板 + W-52 钥匙验证(商户号是接入平台自己的事)。
    AutoDebit,
    /// 纯闸:只判定不开单(demo / 内嵌闸面)。
    Manual,
}

impl PayMode {
    /// 人可读形态名(审计/终端输出用)。
    pub fn label(self) -> &'static str {
        match self {
            PayMode::PendingPay => "人在环待支付",
            PayMode::AutoDebit => "免密代扣",
            PayMode::Manual => "纯闸",
        }
    }

    /// 该档位放行后是否开待支付单(只有第一形态开)。
    pub fn opens_pending(self) -> bool {
        matches!(self, PayMode::PendingPay)
    }
}

/// 待支付单的状态机:`Open → Confirmed → Completed`,任意时刻可 `Voided`(过期作废)。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingState {
    /// 待支付:等人确认。
    Open,
    /// 人已确认(④确认行已落);⑤终态行之前的过渡态。
    Confirmed,
    /// 已完成(⑤终态行 = 完成)。
    Completed,
    /// 已作废(TTL 过期,⑤终态行 = 过期作废)。
    Voided,
}

/// ⑤终态行的两种结局。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingOutcome {
    /// 人确认后的完成。
    Completed,
    /// TTL 过期作废(无人确认)。
    ExpiredVoid,
}

/// 一笔待支付单(账本行状态的内存映像;行本身在 WAL,这里是回放/实时共用的账)。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOrder {
    /// 待支付单号(`p-` 前缀 + 64 位十六进制指纹,由委托/nonce/时刻派生)。
    pub pending_id: String,
    /// 所属委托。
    pub delegation_id: String,
    /// ①意图(原样入账,回放可对账)。
    pub intent: SpendIntent,
    /// ②审批额(= 意图额;确认时三钉之一就是和它比)。
    pub approved_amount_cents: u64,
    /// 开单时刻(Unix 秒)。
    pub created_ts: u64,
    /// 过期时刻(开单时刻 + TTL;半开窗口 `[created, expires)`)。
    pub expires_ts: u64,
    /// 当前状态。
    pub state: PendingState,
    /// ④支付凭证(交易号;人确认时给的,回放可对账)。
    pub proof: Option<String>,
    /// ④确认时刻。
    pub confirmed_ts: Option<u64>,
}

/// 开单回执(`decide_opening_pending` 的第二返回值;放行才开单,拒绝时没有回执)。
///
/// 给接入面(MCP/CLI/SDK)的三件事:单号(给人看、给人确认)、审批额与过期时刻
/// (展示用)、审计行号(证据挂钩——「这张单在第几行」)。纯内存无 WAL 时行号为
/// `None`(回执诚实体现实时侧有没有落盘证据可挂)。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingReceipt {
    /// 待支付单号(`p-` 前缀),人确认时用它指认这一单。
    pub pending_id: String,
    /// ②审批额(= 意图额,分)。
    pub approved_amount_cents: u64,
    /// 过期时刻(Unix 秒;半开窗口 `[开单, 过期)`)。
    pub expires_ts: u64,
    /// ③待支付行的 WAL 行号(1-based);无 WAL 时为 `None`。
    pub wal_line: Option<u64>,
}

/// 人在环待支付被拒(三钉 / 单号不存在 / 凭证缺失)。fail-closed:宁可拒,不可放。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingError {
    /// 单号不存在(不是本闸开出的单)。
    UnknownPending { pending_id: String },
    /// 单不在 `Open` 态(已确认/已完成/已作废)——幂等钉:同一单只能确认一次。
    NotOpen {
        pending_id: String,
        state: PendingState,
    },
    /// 确认额 ≠ 审批额——金额一致钉(防夹带)。
    AmountMismatch {
        pending_id: String,
        approved_cents: u64,
        given_cents: u64,
    },
    /// 单已过 TTL——TTL 钉(过期作废,确认过期单 = 拒)。
    Expired {
        pending_id: String,
        expires_ts: u64,
        now_ts: u64,
    },
    /// TTL 非法(0 = 开出来就是死的单,没有存在意义)。
    InvalidTtl { ttl_secs: u64 },
    /// 支付凭证为空——没有凭证的确认不是可对账的确认。
    EmptyProof,
    /// 单未确认,没有「完成」可言(状态机误用/回放对账不一致)。
    NotConfirmed {
        pending_id: String,
        state: PendingState,
    },
    /// 还没到期就写作废(状态机误用/回放对账不一致)。
    NotYetExpired {
        pending_id: String,
        expires_ts: u64,
        now_ts: u64,
    },
    /// 单号重复(单号必须唯一;回放对账不一致时按 WalMismatch 上抛)。
    DuplicatePendingId { pending_id: String },
}

impl fmt::Display for PendingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PendingError::UnknownPending { pending_id } => {
                write!(f, "待支付单不存在: {pending_id}")
            }
            PendingError::NotOpen { pending_id, state } => write!(
                f,
                "待支付单 {pending_id} 不在待支付状态(当前 {state:?}),不能再次确认(幂等)"
            ),
            PendingError::AmountMismatch {
                pending_id,
                approved_cents,
                given_cents,
            } => write!(
                f,
                "待支付单 {pending_id} 金额不一致:审批 {approved_cents} 分,确认 {given_cents} 分(防夹带,拒)"
            ),
            PendingError::Expired {
                pending_id,
                expires_ts,
                now_ts,
            } => write!(
                f,
                "待支付单 {pending_id} 已过期(过期时刻 {expires_ts},当前 {now_ts}),确认被拒"
            ),
            PendingError::InvalidTtl { ttl_secs } => {
                write!(f, "待支付 TTL 非法: {ttl_secs} 秒(必须 > 0)")
            }
            PendingError::EmptyProof => {
                write!(f, "支付凭证为空:确认必须带交易号,回放才可对账")
            }
            PendingError::NotConfirmed { pending_id, state } => write!(
                f,
                "待支付单 {pending_id} 未处于已确认状态(当前 {state:?}),不能记完成"
            ),
            PendingError::NotYetExpired {
                pending_id,
                expires_ts,
                now_ts,
            } => write!(
                f,
                "待支付单 {pending_id} 还没到期(过期时刻 {expires_ts},当前 {now_ts}),不能作废"
            ),
            PendingError::DuplicatePendingId { pending_id } => {
                write!(f, "待支付单号重复: {pending_id}(单号必须唯一)")
            }
        }
    }
}

impl std::error::Error for PendingError {}

/// 待支付台账(实时态与回放态共用的应用逻辑——两边绝不各写一套)。
///
/// 内层用 `BTreeMap`:按单号有序迭代,`state_hash` 因此确定性(同一份账本
/// 回放两遍指纹必相同)。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingLedger {
    orders: BTreeMap<String, PendingOrder>,
}

impl PendingLedger {
    pub fn new() -> Self {
        Self {
            orders: BTreeMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }

    pub fn len(&self) -> usize {
        self.orders.len()
    }

    pub fn get(&self, pending_id: &str) -> Option<&PendingOrder> {
        self.orders.get(pending_id)
    }

    pub fn contains_key(&self, pending_id: &str) -> bool {
        self.orders.contains_key(pending_id)
    }

    /// 按单号有序迭代(确定性)。
    pub fn iter(&self) -> impl Iterator<Item = (&String, &PendingOrder)> {
        self.orders.iter()
    }

    /// 该委托 + nonce 是否已开过单(一个意图至多一张待支付单;nonce 在成功消费
    /// 时被闸登记,重复意图本就过不了闸,出现两张 = 账本语义不通)。
    pub fn contains_intent(&self, delegation_id: &str, nonce: u64) -> bool {
        self.orders
            .values()
            .any(|o| o.delegation_id == delegation_id && o.intent.nonce == nonce)
    }

    /// ③开单(行已落 WAL 之后调用;单号重复 = 调用方 bug / 账本语义不通)。
    pub(crate) fn apply_open(&mut self, order: PendingOrder) -> Result<(), CoreError> {
        if self.orders.contains_key(&order.pending_id) {
            return Err(CoreError::Pending(PendingError::DuplicatePendingId {
                pending_id: order.pending_id.clone(),
            }));
        }
        self.orders.insert(order.pending_id.clone(), order);
        Ok(())
    }

    /// 确认前的**纯检查**(零变更):单号存在 → 金额一致 → 状态 `Open` → 未过期。
    /// 实时侧先用它挡下被拒的确认(一行都不落),回放侧经由 [`PendingLedger::apply_confirm`]
    /// 走同一套检查——两边不会漂移。
    pub(crate) fn check_confirm(
        &self,
        pending_id: &str,
        amount_cents: u64,
        now_ts: u64,
    ) -> Result<(), PendingError> {
        let order = self
            .orders
            .get(pending_id)
            .ok_or_else(|| PendingError::UnknownPending {
                pending_id: pending_id.to_string(),
            })?;
        // 三钉顺序:金额一致 → 幂等(状态)→ TTL。金额不一致永远先报——
        // 「确认单金额对不上」是比「单已处理」更要紧的夹带信号。
        if order.approved_amount_cents != amount_cents {
            return Err(PendingError::AmountMismatch {
                pending_id: pending_id.to_string(),
                approved_cents: order.approved_amount_cents,
                given_cents: amount_cents,
            });
        }
        if order.state != PendingState::Open {
            return Err(PendingError::NotOpen {
                pending_id: pending_id.to_string(),
                state: order.state,
            });
        }
        if now_ts >= order.expires_ts {
            return Err(PendingError::Expired {
                pending_id: pending_id.to_string(),
                expires_ts: order.expires_ts,
                now_ts,
            });
        }
        Ok(())
    }

    /// ④确认(确认行已落 WAL 之后调用;内部先跑同一套纯检查)。
    pub(crate) fn apply_confirm(
        &mut self,
        pending_id: &str,
        amount_cents: u64,
        proof: &str,
        now_ts: u64,
    ) -> Result<(), CoreError> {
        self.check_confirm(pending_id, amount_cents, now_ts)
            .map_err(CoreError::Pending)?;
        let order = self
            .orders
            .get_mut(pending_id)
            .expect("check_confirm 已确认单存在");
        order.state = PendingState::Confirmed;
        order.proof = Some(proof.to_string());
        order.confirmed_ts = Some(now_ts);
        Ok(())
    }

    /// ⑤终态 = 完成(终态行已落 WAL 之后调用;必须已确认)。
    pub(crate) fn apply_complete(&mut self, pending_id: &str) -> Result<(), CoreError> {
        let order = self.orders.get_mut(pending_id).ok_or_else(|| {
            CoreError::Pending(PendingError::UnknownPending {
                pending_id: pending_id.to_string(),
            })
        })?;
        if order.state != PendingState::Confirmed {
            return Err(CoreError::Pending(PendingError::NotConfirmed {
                pending_id: pending_id.to_string(),
                state: order.state,
            }));
        }
        order.state = PendingState::Completed;
        Ok(())
    }

    /// ⑤终态 = 过期作废(作废行已落 WAL 之后调用;必须仍是 `Open` 且确已过期)。
    pub(crate) fn apply_void(&mut self, pending_id: &str, now_ts: u64) -> Result<(), CoreError> {
        let order = self.orders.get_mut(pending_id).ok_or_else(|| {
            CoreError::Pending(PendingError::UnknownPending {
                pending_id: pending_id.to_string(),
            })
        })?;
        if order.state != PendingState::Open {
            return Err(CoreError::Pending(PendingError::NotOpen {
                pending_id: pending_id.to_string(),
                state: order.state,
            }));
        }
        if now_ts < order.expires_ts {
            return Err(CoreError::Pending(PendingError::NotYetExpired {
                pending_id: pending_id.to_string(),
                expires_ts: order.expires_ts,
                now_ts,
            }));
        }
        order.state = PendingState::Voided;
        Ok(())
    }
}
