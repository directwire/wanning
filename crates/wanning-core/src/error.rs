//! 核心错误类型。
//!
//! 语义分层,别混淆:
//! - **业务拒绝**不是错误——是 [`crate::gate::GateDecision::Deny`],属于闸的正常输出,
//!   必须落 WAL、必须给出 reason。
//! - [`CoreError`] 只表示**调用方用错了 API / 配置非法 / 状态被破坏**,此时闸拒绝继续,
//!   fail-closed(宁可停,不可放)。

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreError {
    /// 委托注册被拒:字段非法(空 id / 预算为 0 / 有效期倒挂等)。fail-closed:宁可注册失败。
    InvalidDelegation(String),
    /// 意图非法:金额或 nonce 为 0、必填字段为空。
    InvalidIntent(String),
    /// 引用了不存在的委托(撤销/查询)。
    UnknownDelegation(String),
    /// 委托 id 重复注册。
    DuplicateDelegation(String),
    /// 账本累计溢出(u64)。状态异常,fail-closed:宁可报错也不回绕把巨额记成小额。
    LedgerOverflow(String),
    /// 在闸未放行的情况下调用了 `commit`(API 误用,程序员错误)。
    /// 正常流程是 evaluate → (写审计) → commit;直接 commit 会被这里拦下。
    CommitRejected(String),
    /// WAL 打开/读写失败。
    WalIo(String),
    /// 单写者锁被占:同一份 WAL 已有另一个活着的写进程。fail-closed 拒启——
    /// 两个闸共写一份审计 = 预算硬上限失效、同一 nonce 跨进程放行两次。
    WalLocked { path: String, message: String },
    /// WAL 里有一行读不懂(半行 JSON / 空行 / 未知结构)。**绝不静默跳过**。
    WalBadLine { line: u64, message: String },
    /// WAL 完整性链断裂(`seq` 与物理行号不符 / `prev` 与按前文重算的链值不符)。
    /// 这是日志被改(改字段/删行/重排/复制)的痕迹,fail-closed 拒读拒续写。
    WalChainBroken { line: u64, message: String },
    /// 回放时重算结果与记录不一致(日志被改 / 实时与回放口径漂移)。fail-closed 停机。
    WalMismatch { line: u64, message: String },
    /// 审计锚点文件非法:格式/字段读不懂,或 MAC 与所有者密钥对不上(锚点不是
    /// 所有者签的,或锚点本身被改)。fail-closed:不可信的锚点比没有锚点更危险。
    AnchorInvalid(String),
    /// 当前 WAL 与合法锚点不符:整体截尾(行数不足)或被锚定的前缀内容被改。
    /// 这是 W-21 完整性链已知边界(尾行篡改/截尾本地验不住)的抓法。
    AnchorMismatch(String),
    /// 人在环待支付被拒(W-53a:三钉 / 单号不存在 / 凭证缺失 / 单号重复)。
    /// fail-closed:被拒的确认一行都不落账。
    Pending(crate::pending::PendingError),
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::InvalidDelegation(m) => write!(f, "委托注册被拒(非法配置): {m}"),
            CoreError::InvalidIntent(m) => write!(f, "消费意图非法: {m}"),
            CoreError::UnknownDelegation(id) => write!(f, "未知委托: {id}"),
            CoreError::DuplicateDelegation(id) => write!(f, "委托重复注册: {id}"),
            CoreError::LedgerOverflow(m) => write!(f, "账本溢出(状态异常,fail-closed): {m}"),
            CoreError::CommitRejected(m) => write!(f, "commit 被拒(闸未放行,API 误用): {m}"),
            CoreError::WalIo(m) => write!(f, "审计日志 IO 失败(fail-closed): {m}"),
            CoreError::WalLocked { path, message } => {
                write!(
                    f,
                    "审计日志单写者锁被占(fail-closed 拒启): {message}[锁文件: {path}]"
                )
            }
            CoreError::WalBadLine { line, message } => {
                write!(
                    f,
                    "审计日志第 {line} 行损坏(fail-closed,不静默跳过): {message}"
                )
            }
            CoreError::WalChainBroken { line, message } => {
                write!(
                    f,
                    "审计日志第 {line} 行完整性链断裂(fail-closed): {message}"
                )
            }
            CoreError::WalMismatch { line, message } => {
                write!(
                    f,
                    "审计日志第 {line} 行回放对账不一致(fail-closed): {message}"
                )
            }
            CoreError::AnchorInvalid(m) => {
                write!(f, "审计锚点文件不可信(fail-closed): {m}")
            }
            CoreError::AnchorMismatch(m) => {
                write!(f, "审计日志与所有者锚点不符——审计被动过(fail-closed): {m}")
            }
            CoreError::Pending(e) => write!(f, "人在环待支付被拒(fail-closed): {e}"),
        }
    }
}

impl std::error::Error for CoreError {}
