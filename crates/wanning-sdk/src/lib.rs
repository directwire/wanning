//! # Wanning 嵌入 SDK(wanning-sdk)
//!
//! 平台把 Wanning 闸**嵌进自己进程**的门面(P2「SDK」)。MCP
//! server(`wanning-mcp`)是给 agent 平台挂的 **stdio 面**;本 crate 是给宿主
//! 程序(如 ANAI 的执行层)直接调用的**进程内面**——同一个闸,两种接法。
//!
//! ## 为什么要有这个 crate:把硬语义变成类型系统强制
//!
//! 直接用 `wanning-core`(`WanningState` + `Gate`)当然可行,但四条硬语义
//! 今天只靠调用方纪律(demo 回路/MCP server 的代码)维持;嵌进别人的进程,
//! 纪律会丢,类型不会:
//!
//! 1. **开机必续放**——[`Wanning::open`] 是唯一入口,内部就是
//!    `WanningState::live_resuming`(先整体回放对账、fail-closed 拒启,再接续
//!    旧账)。core 里的 `live`/`with_wal`(不回放,W-17 的 nonce 洗白/撤销
//!    复活 bug 的根源)在 SDK 面**不存在**。
//! 2. **闸侧注入**——[`SpendRequest`] 只有金额/商户/类别/备注,根本没有
//!    `delegation_id`/`nonce` 字段;委托 id 由宿主显式指定,nonce 由闸按
//!    nonce 作用域单调分配。模型给的越权字段在类型上进不来(语义对齐
//!    demo 决策回路「delegation_id 与 nonce 由闸侧注入,模型无权指定」)。
//! 3. **无审计不服务**——open 必带 WAL 路径,SDK 没有「无审计的闸」;
//!    每一笔判定(放行与拒绝)都 write-ahead 落审计后才对调用方可见。
//! 4. **零网络零消费**——SDK 面只有判定/撤销/审计读取,没有 HTTP、没有
//!    渠道 adapter、没有支付工具(与 MCP 工具面同一纪律:支付在闸放行后
//!    由渠道侧另行执行,永远不在闸的面里)。
//!
//! ## 五步接入
//!
//! ```rust
//! use wanning_sdk::{Delegation, SpendRequest, Wanning};
//!
//! // 1. 开闸(必带 WAL;开机即续放:已有旧账先回放对账,fail-closed 拒启)
//! let wal = std::env::temp_dir().join(format!("wanning-sdk-doctest-{}.jsonl", std::process::id()));
//! let _ = std::fs::remove_file(&wal); // doctest 可重入:清掉上次残留
//! let mut gate = Wanning::open(&wal).expect("开闸");
//!
//! // 2. 注册委托(用户 → agent 的一次授权:预算/有效期/nonce 作用域)
//! let delegation = Delegation::new(
//!     "d1", "boss", "claude-code",
//!     1000,          // 总预算 ¥10.00(单位:分,u64,全程禁浮点)
//!     1000,          // 生效时刻(Unix 秒)
//!     u64::MAX - 1,  // 失效时刻(不含)
//!     "agent:claude-code",
//! );
//! gate.authorize(delegation).expect("注册");
//!
//! // 3. 判定一笔消费意图(委托 id 宿主给,nonce 闸注入)
//! let verdict = gate
//!     .decide("d1", SpendRequest::new(500, "jd:shop-1", "grocery", "午饭"))
//!     .expect("判定");
//! assert!(verdict.decision.is_allow(), "预算内应放行");
//! assert_eq!(verdict.nonce, 1, "闸注入的 nonce 从 1 起");
//! assert_eq!(verdict.wal_line, 2, "判定落在审计第 2 行(第 1 行是注册)");
//!
//! // 4. kill switch(授权者动作;撤销后永不允许)
//! gate.revoke("d1").expect("撤销");
//! assert!(gate.is_revoked("d1"));
//!
//! // 5. 审计自证:验链 + 回放对账(读侧独立重算)
//! let report = gate.self_check().expect("诚实账本自证通过");
//! assert_eq!(report.wal_line_count, 3, "注册 + 放行 + 撤销,各恰一行");
//!
//! drop(gate);
//! let _ = std::fs::remove_file(&wal);
//! ```
//!
//! ## 面的边界(诚实声明)
//!
//! - **宿主传错委托 id 是嵌入方 bug** → [`Wanning::decide`] 返回
//!   `Err(UnknownDelegation)`,不落审计(没有判定发生,审计里不该长出假意图);
//!   模型侧的越权尝试走 MCP 面,那边逐条留痕。两个面的调用方可信度不同,
//!   未知委托的语义因此不同——这是设计,不是不一致。
//! - **意图自身非法(金额 0 / 商户空白)是业务拒绝**,照闸的阶段 0 判过、
//!   落审计,不是 `Err`——门面忠实于 core 口径。
//! - **并发**——一个 [`Wanning`] 句柄一把闸,不做内部锁;同一份 WAL 的第二个
//!   写进程/写句柄在 [`Wanning::open`] 就 fail-closed(`CoreError::WalLocked`,
//!   单写者锁,见 `wanning-core` W-18)。
//! - **撤销/锚点/审计页导出不在这个面**——撤销是授权者动作(agent 无权,
//!   MCP 面同样不设);锚点是所有者侧 CLI(`wanning-demo --anchor-sign`);
//!   给人看的回放页是 `--export-audit`。SDK 只提供 [`Wanning::audit_tail`] 与
//!   [`Wanning::self_check`] 供宿主自查。
//!
//! | crate | 面 | 调用方 |
//! |---|---|---|
//! | `wanning-core` | 闸语义本体 | 库 |
//! | `wanning-mcp` | stdio(JSON-RPC/MCP) | agent 平台 |
//! | `wanning-sdk`(本 crate) | 进程内 | 宿主程序 |
//! | `wanning-demo` | CLI 样板间 + 渠道 adapter + 锚点/审计页 | 人 |

use std::path::{Path, PathBuf};

use wanning_core::error::CoreError;
use wanning_core::intent::SpendIntent;
use wanning_core::state::WanningState;
use wanning_core::wal::{read_verified, WalRecord};

// 精选再导出:嵌入方只需要这几个类型,不必直接依赖 wanning-core。
pub use wanning_core::delegation::Delegation;
pub use wanning_core::gate::{DenyReason, GateDecision};
pub use wanning_core::policy::{QuietWindow, SpendPolicy, VelocityLimit};
pub use wanning_core::wal::WalChainLink;

/// 一笔消费请求(agent 宿主提交给闸判定)。
///
/// 刻意**没有** `delegation_id` 与 `nonce` 字段:委托 id 由宿主在
/// [`Wanning::decide`] 显式指定,nonce 由闸按作用域单调注入——「模型无权
/// 指定」从纪律变成类型(见 crate 级文档「闸侧注入」)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpendRequest {
    /// 本笔金额,单位分。0 非法(会得到业务拒绝 `InvalidAmount`,落审计)。
    pub amount_cents: u64,
    /// 商户 id(开放平台侧语义,闸不解析;空白 → `InvalidIntent` 拒绝)。
    pub merchant_id: String,
    /// 消费类别(自由文本标签,落审计用)。
    pub category: String,
    /// 备注(人类可读,落审计)。
    pub memo: String,
}

impl SpendRequest {
    pub fn new(
        amount_cents: u64,
        merchant_id: impl Into<String>,
        category: impl Into<String>,
        memo: impl Into<String>,
    ) -> Self {
        Self {
            amount_cents,
            merchant_id: merchant_id.into(),
            category: category.into(),
            memo: memo.into(),
        }
    }
}

/// 一笔判定的完整回执:闸的判定 + 闸注入的 nonce + 审计行号。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpendVerdict {
    /// 闸判定(放行携带判后累计消费;拒绝携带原因)。
    pub decision: GateDecision,
    /// 闸注入的 nonce(审计里就是它;被拒不耗号,重试会用同一个)。
    pub nonce: u64,
    /// 本笔判定落审计的 WAL 行号(1-based)。
    pub wal_line: u64,
}

/// 审计尾的一行(行号 + 记录本体)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditLine {
    /// 物理行号(1-based),即审计证据里的「WAL 偏移」。
    pub line_no: u64,
    /// 记录本体(注册/撤销/判定;`kind()`/`ts()` 可读)。
    pub record: WalRecord,
}

/// [`Wanning::self_check`] 的回执:三条独立口径对上了。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelfCheckReport {
    /// 审计行数(实时侧计数 == 读侧重读计数)。
    pub wal_line_count: u64,
    /// 完整性链尾(实时侧 == 读侧逐行独立重算)。
    pub chain_tail: u64,
    /// 状态指纹(实时态 == 回放重建态;FNV-1a 64,非密码学,仅对账用)。
    pub state_hash: u64,
}

/// 闸句柄:进程内嵌入的唯一入口。
///
/// 一个句柄 = 一个闸 + 一份 append-only 审计(WAL)+ 单写者锁。判定、撤销、
/// 审计读取之外的面(网络/支付/锚点)不存在于本类型。
#[derive(Debug)]
pub struct Wanning {
    state: WanningState,
}

impl Wanning {
    /// 打开(或创建)一个闸实例。
    ///
    /// 唯一的打开方式,**内部就是 `WanningState::live_resuming`**:先整体回放
    /// 已有 WAL 对账(损坏/篡改/不一致 → fail-closed 拒启),再换系统时钟接续
    /// 同一份 WAL。账本、撤销、nonce 登记全部从审计接续——重启绝不洗白 nonce、
    /// 绝不复活已撤销的授权(W-17 的教训,在这里结构性不可复现)。
    ///
    /// 同一份 WAL 同时至多一个活着的写进程/写句柄(单写者锁):第二个
    /// [`Wanning::open`] fail-closed 返回 `CoreError::WalLocked`。
    pub fn open(wal_path: impl AsRef<Path>) -> Result<Self, CoreError> {
        Ok(Self {
            state: WanningState::live_resuming(wal_path)?,
        })
    }

    /// 注册委托(用户 → agent 的一次授权):先确认必成,再写审计,再入闸。
    ///
    /// 重复 id → `Err(DuplicateDelegation)`(零审计噪音:改预算 = 篡改审计,
    /// 绝不以「再注册一次」实现)。
    pub fn authorize(&mut self, delegation: Delegation) -> Result<(), CoreError> {
        self.state.register_delegation(delegation)
    }

    /// 判定一笔消费意图:闸注入 nonce → evaluate → 写审计 → 放行才扣账。
    ///
    /// 返回完整回执(判定 + 注入的 nonce + 审计行号)。失败语义:
    /// - `delegation_id` 未注册 → `Err(UnknownDelegation)`,**不落审计**
    ///   (宿主 bug,没有判定发生;见 crate 级文档「面的边界」);
    /// - 审计写失败 → `Err`,状态零变更(这笔消费没有发生,也不能发生);
    /// - 意图自身非法(金额 0 等)→ **业务拒绝**,落审计(`SpendVerdict`)。
    pub fn decide(
        &mut self,
        delegation_id: &str,
        request: SpendRequest,
    ) -> Result<SpendVerdict, CoreError> {
        // 闸侧注入的前提是委托存在:nonce 作用域挂在委托上。
        // 未知委托在这里就拒(Err,零审计噪音),不制造一条「没人提过的意图」。
        let delegation = match self.state.gate().delegation(delegation_id) {
            Some(delegation) => delegation,
            None => return Err(CoreError::UnknownDelegation(delegation_id.to_string())),
        };
        let nonce = self.next_nonce(&delegation.nonce_scope);
        let intent = SpendIntent::new(
            delegation_id,
            nonce,
            request.amount_cents,
            request.merchant_id,
            request.category,
            request.memo,
        );
        let decision = self.state.decide(&intent)?;
        let wal_line = self.state.last_wal_line().expect("SDK 必挂 WAL,判定必落行");
        Ok(SpendVerdict {
            decision,
            nonce,
            wal_line,
        })
    }

    /// 撤销委托(kill switch,授权者动作):先确认必成,再写审计,再撤销。
    ///
    /// 单向(没有解除);重复撤销幂等成功且**每次都落审计**(记账完备)。
    /// 未知委托 → `Err(UnknownDelegation)`。
    pub fn revoke(&mut self, delegation_id: &str) -> Result<(), CoreError> {
        self.state.revoke(delegation_id)
    }

    /// 已注册委托(只读)。
    pub fn delegation(&self, delegation_id: &str) -> Option<&Delegation> {
        self.state.gate().delegation(delegation_id)
    }

    /// 某委托已累计消费(分);未知委托 None。
    pub fn spent_cents(&self, delegation_id: &str) -> Option<u64> {
        self.state.gate().spent_cents(delegation_id)
    }

    /// 某委托剩余预算(分);未知委托 None。
    pub fn remaining_cents(&self, delegation_id: &str) -> Option<u64> {
        self.state.gate().remaining_cents(delegation_id)
    }

    /// 某委托是否已被撤销(kill switch 生效中)。
    pub fn is_revoked(&self, delegation_id: &str) -> bool {
        self.state.gate().is_revoked(delegation_id)
    }

    /// 审计 WAL 路径(SDK 必挂 WAL,恒有值)。
    pub fn wal_path(&self) -> &Path {
        self.state
            .wal_path()
            .expect("SDK 必挂 WAL(无审计路径不存在)")
    }

    /// 最近一次追加的审计行号(1-based);空账为 None。
    pub fn last_wal_line(&self) -> Option<u64> {
        self.state.last_wal_line()
    }

    /// 审计完整性链的链尾值;空账为 None。
    pub fn chain_tail(&self) -> Option<u64> {
        self.state.audit_chain_tail()
    }

    /// 状态指纹(FNV-1a 64,非密码学,仅对账用)。
    pub fn state_hash(&self) -> u64 {
        self.state.state_hash()
    }

    /// 逐行链节(读侧独立重算;给人看/给审计页用,不是照抄落盘行)。
    pub fn chain_links(&self) -> Result<Vec<WalChainLink>, CoreError> {
        Ok(read_verified(self.wal_path())?.links)
    }

    /// 读审计尾:最后 `lines` 行(请求超过现有行数时给全部)。
    ///
    /// 读路径不受单写者锁限制,但照常**逐行验完整性链**——账被改过就连读都
    /// 读不出来(fail-closed),绝不把被改的审计当好账读给宿主。
    pub fn audit_tail(&self, lines: usize) -> Result<Vec<AuditLine>, CoreError> {
        let verified = read_verified(self.wal_path())?;
        let start = verified.records.len().saturating_sub(lines);
        Ok(verified.records[start..]
            .iter()
            .map(|(line_no, record)| AuditLine {
                line_no: *line_no,
                record: record.clone(),
            })
            .collect())
    }

    /// 审计自证:验链 + 回放对账,三条独立口径全对上才发回执。
    ///
    /// 宿主可以随时调(比如每 N 笔判定后),证明「我内存里的账」与「磁盘上的
    /// 审计」仍然是同一本账:
    /// 1. 读侧逐行验完整性链(改历史行/删行/重排/复制 → `WalChainBroken`);
    /// 2. 实时链尾 == 读侧独立重算链尾;
    /// 3. 实时状态指纹 == 回放重建状态指纹(账本/撤销/nonce 全部对上)。
    ///
    /// 任一不过 → `Err`,fail-closed:**不可信的自证比不自证更危险**。
    pub fn self_check(&self) -> Result<SelfCheckReport, CoreError> {
        let path: PathBuf = self.wal_path().to_path_buf();
        let verified = read_verified(&path)?;
        let replayed = WanningState::replay(&path)?;

        let live_count = self.state.wal_line_count().expect("SDK 必挂 WAL");
        let live_tail = self.state.audit_chain_tail().expect("SDK 必挂 WAL");
        let last_line = live_count.max(1);

        if verified.records.len() as u64 != live_count {
            return Err(CoreError::WalMismatch {
                line: last_line,
                message: format!(
                    "审计行数对不上:实时 {live_count} / 磁盘重读 {}",
                    verified.records.len()
                ),
            });
        }
        if verified.tail != live_tail {
            return Err(CoreError::WalMismatch {
                line: last_line,
                message: format!(
                    "完整性链尾对不上:实时 {live_tail:#x} / 读侧重算 {:#x}",
                    verified.tail
                ),
            });
        }
        let replayed_hash = replayed.state_hash();
        let live_hash = self.state.state_hash();
        if replayed_hash != live_hash {
            return Err(CoreError::WalMismatch {
                line: last_line,
                message: format!("回放对账不一致:实时 {live_hash:#x} / 回放 {replayed_hash:#x}"),
            });
        }
        Ok(SelfCheckReport {
            wal_line_count: live_count,
            chain_tail: live_tail,
            state_hash: live_hash,
        })
    }

    /// 闸侧 nonce 分配:该作用域内已消耗的最大 nonce + 1(空作用域从 1 起)。
    ///
    /// - **拒绝不耗号**(core 语义):被拒的意图不进登记集,下一笔分配会拿到
    ///   同一个 nonce——「修好后用同一 nonce 重发」合法;
    /// - **跨委托共享作用域**:同一 agent 的多份委托共用一套 nonce 序列,
    ///   分配看作用域整体,第二份委托不会撞上已消耗的号;
    /// - **跨重启**:登记集从审计回放重建,重启后续接,不回 1。
    fn next_nonce(&self, nonce_scope: &str) -> u64 {
        self.state
            .gate()
            .replay_registry()
            .iter()
            .filter(|(scope, _)| scope == nonce_scope)
            .map(|(_, nonce)| *nonce)
            .max()
            .unwrap_or(0)
            + 1
    }
}
