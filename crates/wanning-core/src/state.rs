//! 闸的完整运行状态([`WanningState`]):闸 + 审计日志 + 时钟。
//!
//! 这是 demo / 未来 MCP server 实际持有的对象。职责只有一条:
//! **每一条决策都必须先落审计,再落账本**(write-ahead)——审计写不进去,这笔消费
//! 就不能发生。这样「崩溃后的世界」只会比实时状态**更严格**(多扣不会出现,少扣可能),
//! 永远不会出现「花了钱却查无此账」。
//!
//! 回放([`WanningState::replay`]):从 WAL 逐行重建状态,用记录里的 ts 驱动注入时钟,
//! 并**重算每一条决策**与记录对账;任何不一致立即 fail-closed 报错。回放是确定性的:
//! 同一份 WAL 回放两遍,state hash 相同。

use std::path::Path;
use std::sync::Arc;

use crate::clock::{MockClock, SharedClock, SystemClock};
use crate::delegation::Delegation;
use crate::error::CoreError;
use crate::gate::{Gate, GateDecision};
use crate::intent::SpendIntent;
use crate::wal::{fnv1a_64, Wal, WalDecision, WalRecord};

/// 闸 + 审计日志 + 时钟的运行时状态。
#[derive(Debug)]
pub struct WanningState {
    gate: Gate,
    wal: Option<Wal>,
}

impl WanningState {
    /// 纯内存状态(无审计落盘)。回放与测试用。
    pub fn new(clock: SharedClock) -> Self {
        Self {
            gate: Gate::new(clock),
            wal: None,
        }
    }

    /// 带审计落盘的状态。WAL 打开为追加模式,绝不截断。
    pub fn with_wal(clock: SharedClock, wal_path: impl AsRef<Path>) -> Result<Self, CoreError> {
        Ok(Self {
            gate: Gate::new(clock),
            wal: Some(Wal::open(wal_path)?),
        })
    }

    /// 生产状态:系统时钟 + 审计落盘。
    ///
    /// **注意:不回放已有 WAL**——闸从空开始,只往后追加。适合「一次进程一次新账」
    /// 的 demo 场景;长期服务重启要接续旧账,用 [`WanningState::live_resuming`]。
    pub fn live(wal_path: impl AsRef<Path>) -> Result<Self, CoreError> {
        Self::with_wal(Arc::new(SystemClock), wal_path)
    }

    /// 断点续跑:先整体回放已有 WAL 对账(损坏/篡改/不一致 → fail-closed 拒启),
    /// 再换回系统时钟、继续往**同一份 WAL** 追加。
    ///
    /// 长期服务(MCP server)重启时用它:账本、撤销、nonce 登记全部从审计接续,
    /// 绝不带着一张空账本接着判——否则重启会把 nonce 洗白、把撤销掉的授权复活。
    ///
    /// 同一份 WAL 同时至多一个**活着的写进程**(`Wal::open` 自动持单写者锁):
    /// 第二个进程 fail-closed 拒启(`CoreError::WalLocked`)。两个平台并挂同一份
    /// WAL(`.mcp.json` + `.trae/mcp.json`)就是真实场景——并发双闸的内存账本
    /// 互不知情,预算硬上限会被合力突破(实测见 `tests/single_writer.rs`)。
    ///
    /// 与 [`WanningState::replay`] 的区别:replay 冻结在「过去的世界」(注入时钟停在
    /// 最后一条记录的 ts、不挂 WAL);本方法校验过后回到「现在的世界」(系统时钟,
    /// 继续写审计)。
    pub fn live_resuming(wal_path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let path = wal_path.as_ref();
        // 先开 WAL(不存在则创建;append-only,绝不截断)——空文件是合法起点。
        let wal = Wal::open(path)?;
        let resumed = Self::replay(path)?;
        Ok(Self {
            gate: resumed.gate.with_clock(Arc::new(SystemClock)),
            wal: Some(wal),
        })
    }

    pub fn gate(&self) -> &Gate {
        &self.gate
    }

    pub fn wal_path(&self) -> Option<&Path> {
        self.wal.as_ref().map(Wal::path)
    }

    /// WAL 当前行数;无 WAL 时为 None。审计证据的「WAL 偏移」即行号。
    pub fn wal_line_count(&self) -> Option<u64> {
        self.wal.as_ref().map(Wal::line_count)
    }

    /// 最近一次追加的 WAL 行号(1-based);无 WAL 时为 None。
    pub fn last_wal_line(&self) -> Option<u64> {
        self.wal_line_count()
    }

    /// 审计完整性链的链尾值(最后一条记录的链值;无 WAL 时为 None)。
    ///
    /// 对账证据之一:实时侧这个值,与读侧 [`read_verified`](crate::wal::read_verified)
    /// 独立重算的链尾必须相等——逐行成链,改历史行而不重算后续整条链,当场现形。
    pub fn audit_chain_tail(&self) -> Option<u64> {
        self.wal.as_ref().map(Wal::chain_tail)
    }

    /// 注册委托:先确认必成,再写审计,再入闸(write-ahead)。
    pub fn register_delegation(&mut self, delegation: Delegation) -> Result<(), CoreError> {
        // 预检与 Gate::register_delegation 同一套规则;先确认「必然成功」,
        // 保证审计记录永远不会描述一次没发生的注册。
        delegation.validate()?;
        if self.gate.delegation(&delegation.id).is_some() {
            return Err(CoreError::DuplicateDelegation(delegation.id));
        }
        let record = WalRecord::RegisterDelegation {
            ts: self.now(),
            delegation: delegation.clone(),
        };
        if let Some(wal) = self.wal.as_mut() {
            wal.append(&record)?;
        }
        self.gate.register_delegation(delegation)
    }

    /// 撤销委托(kill switch):先确认必成,再写审计,再撤销。
    pub fn revoke(&mut self, delegation_id: &str) -> Result<(), CoreError> {
        if self.gate.delegation(delegation_id).is_none() {
            return Err(CoreError::UnknownDelegation(delegation_id.to_string()));
        }
        let record = WalRecord::Revoke {
            ts: self.now(),
            delegation_id: delegation_id.to_string(),
        };
        if let Some(wal) = self.wal.as_mut() {
            wal.append(&record)?;
        }
        self.gate.revoke(delegation_id)
    }

    /// 判定一笔消费意图:evaluate → 写审计 → commit(write-ahead)。
    ///
    /// 返回闸的判定。注意失败语义:
    /// - 审计写失败 → `Err`,**状态零变更**(这笔消费没有发生,也不能发生);
    /// - 审计写成功但 commit 失败(理论不可达)→ `Err`,WAL 领先于账本,
    ///   回放侧只会更严格,不会放水。
    pub fn decide(&mut self, intent: &SpendIntent) -> Result<GateDecision, CoreError> {
        // 时钟只读一次:评估、WAL 记录 ts、落地扣减(含速率窗口时刻)用同一 `now`。
        // 若各读各的,跨秒边界时实时侧速率窗口时刻会漂离 WAL 记录 ts,回放对账
        // 会把诚实账本误判为不一致——单次读是回放可重建的前提。
        let ts = self.now();
        let verdict = self.gate.evaluate_at(intent, ts);
        let spent_after = match verdict {
            // Allow 携带的就是「扣减后的累计消费」,直接取用,不重算。
            GateDecision::Allow { budget_after_cents } => budget_after_cents,
            GateDecision::Deny { .. } => self.gate.spent_cents(&intent.delegation_id).unwrap_or(0),
        };
        let record = WalRecord::Decide {
            ts,
            decision: match verdict {
                GateDecision::Allow { .. } => WalDecision::Allow,
                GateDecision::Deny { .. } => WalDecision::Deny,
            },
            delegation_id: intent.delegation_id.clone(),
            intent: intent.clone(),
            reason: verdict.deny_reason(),
            budget_after_cents: spent_after,
        };
        if let Some(wal) = self.wal.as_mut() {
            wal.append(&record)?;
        }
        match verdict {
            GateDecision::Allow { budget_after_cents } => {
                let after = self.gate.commit_at(intent, ts)?;
                debug_assert_eq!(after, budget_after_cents);
                Ok(GateDecision::Allow {
                    budget_after_cents: after,
                })
            }
            deny => Ok(deny),
        }
    }

    /// 闸状态指纹(FNV-1a 64,非密码学,仅用于确定性对账)。
    ///
    /// 覆盖:委托集、账本、撤销集、nonce 登记集、策略运行时状态(W-27 速率
    /// 窗口时刻与类目台账——随 commit 演化的状态必须进指纹,否则「速率窗口跨
    /// 重启被洗掉」这类回放缺失对账不出来);全部按有序迭代序列化,
    /// 因此「同一份 WAL 回放两遍 hash 必相同」由构造保证。
    pub fn state_hash(&self) -> u64 {
        let snapshot = serde_json::json!({
            "delegations": self.gate.delegations().collect::<Vec<_>>(),
            "spent_cents": self.gate.ledger().entries().collect::<Vec<_>>(),
            "revoked": self.gate.revocations().iter().collect::<Vec<_>>(),
            "used_nonces": self.gate.replay_registry().iter().collect::<Vec<_>>(),
            "policy_states": self.gate.policy_states().collect::<Vec<_>>(),
        });
        fnv1a_64(snapshot.to_string().as_bytes())
    }

    fn now(&self) -> u64 {
        self.gate.clock().now()
    }

    /// 从 WAL 回放重建状态(确定性;损坏行 / 完整性链断裂 / 对账不一致 → fail-closed)。
    ///
    /// 返回的状态:
    /// - 时钟是被注入的 [`MockClock`],冻结在最后一条记录的 ts(回放是「过去的世界」,
    ///   不适合继续判定新意图——要续,就重新 `live()` 开一个新 WAL);
    /// - 未挂 WAL(回放不追加记录)。
    pub fn replay(wal_path: impl AsRef<Path>) -> Result<Self, CoreError> {
        // 读回即验完整性链(seq/prev 逐行核),再逐行重算对账。
        let records = crate::wal::read_verified(wal_path)?.records;
        let clock = MockClock::new(0);
        let mut state = WanningState::new(Arc::new(clock.clone()));
        for (line_no, record) in records {
            let record_ts = record.ts();
            clock.set_now(record_ts);
            match record {
                WalRecord::RegisterDelegation { delegation, .. } => state
                    .gate
                    .register_delegation(delegation)
                    .map_err(|e| CoreError::WalMismatch {
                        line: line_no,
                        message: format!("重放注册失败: {e}"),
                    })?,
                WalRecord::Revoke { delegation_id, .. } => state
                    .gate
                    .revoke(&delegation_id)
                    .map_err(|e| CoreError::WalMismatch {
                        line: line_no,
                        message: format!("重放撤销失败: {e}"),
                    })?,
                WalRecord::Decide {
                    decision,
                    intent,
                    reason,
                    budget_after_cents,
                    ..
                } => {
                    // 重算用记录自身的 ts(与 clock.set_now 同一时刻):速率窗口等
                    // 依赖「判定时刻」的检查必须在记录 ts 上复现,绝不能看回放进程
                    // 的真实时钟。
                    let ts = record_ts;
                    let verdict = state.gate.evaluate_at(&intent, ts);
                    match (verdict, decision, reason) {
                        (
                            GateDecision::Allow {
                                budget_after_cents: recomputed,
                            },
                            WalDecision::Allow,
                            None,
                        ) => {
                            if recomputed != budget_after_cents {
                                return Err(CoreError::WalMismatch {
                                    line: line_no,
                                    message: format!(
                                        "放行记录的累计消费与重算不一致:记录 {budget_after_cents} / 重算 {recomputed}"
                                    ),
                                });
                            }
                            state.gate.commit_at(&intent, ts).map_err(|e| {
                                CoreError::WalMismatch {
                                    line: line_no,
                                    message: format!("重放扣减失败: {e}"),
                                }
                            })?;
                        }
                        (
                            GateDecision::Deny { reason: recomputed },
                            WalDecision::Deny,
                            Some(recorded_reason),
                        ) if recomputed == recorded_reason => {
                            // 拒绝:状态零变更,只需口径一致。
                        }
                        (verdict, decision, reason) => {
                            return Err(CoreError::WalMismatch {
                                line: line_no,
                                message: format!(
                                    "重算判定与记录不一致:重算 {verdict:?} / 记录 {decision:?} reason={reason:?}"
                                ),
                            });
                        }
                    }
                }
            }
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Clock, MockClock};
    use crate::gate::DenyReason;

    fn tmp_wal(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("wanning-state-tests");
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir.join(format!("{tag}-{}.jsonl", std::process::id()))
    }

    fn delegation() -> Delegation {
        Delegation::new(
            "d1",
            "boss",
            "claude-code",
            1000,
            1000,
            2000,
            "agent:claude-code",
        )
    }

    /// 续跑测试专用:回放侧时钟停在记录 ts(如 1500),续跑后是真实「现在」——
    /// 委托窗口必须同时覆盖两个世界(1500 之前生效、系统时钟下未过期)。
    fn long_lived_delegation() -> Delegation {
        Delegation::new(
            "d1",
            "boss",
            "claude-code",
            1000,
            1000,
            SystemClock.now().checked_add(86_400).expect("有效期溢出"),
            "agent:claude-code",
        )
    }

    fn intent(nonce: u64, amount_cents: u64) -> SpendIntent {
        SpendIntent::new("d1", nonce, amount_cents, "jd:shop-1", "grocery", "测试")
    }

    #[test]
    fn allow_and_deny_are_both_recorded() {
        let path = tmp_wal("both");
        let clock = MockClock::new(1500);
        let mut state = WanningState::with_wal(Arc::new(clock.clone()), &path).expect("开 WAL");
        state.register_delegation(delegation()).expect("注册");

        // 放行
        let a = state.decide(&intent(1, 500)).expect("判定");
        assert!(a.is_allow());
        // 拒绝(超额)
        let d = state.decide(&intent(2, 9000)).expect("判定");
        assert_eq!(d.deny_reason(), Some(DenyReason::OverBudget));

        let records = crate::wal::read_records(&path).expect("读回");
        assert_eq!(records.len(), 3, "注册 + 放行 + 拒绝");
        let (_, first_decide) = &records[1];
        let (_, second_decide) = &records[2];
        match (first_decide.kind(), first_decide.ts(), second_decide.kind()) {
            ("decide", 1500, "decide") => {}
            other => panic!("记录形状不符: {other:?}"),
        }
        // deny 记录在案(带 reason、不带 budget 变化)
        let crate::wal::WalRecord::Decide {
            decision,
            reason,
            budget_after_cents,
            ..
        } = second_decide
        else {
            panic!("第二条决策记录应是 Decide");
        };
        assert_eq!(*decision, WalDecision::Deny);
        assert_eq!(*reason, Some(DenyReason::OverBudget));
        assert_eq!(*budget_after_cents, 500, "拒绝不改账本,累计消费仍是 500");
    }

    #[test]
    fn write_ahead_audit_failure_leaves_state_untouched() {
        // 审计写不进去 → 消费不能发生(状态零变更)。
        // 构造:占用目标路径为目录,使 WAL 打开即失败。
        let dir = std::env::temp_dir().join("wanning-state-tests");
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join(format!("dir-as-wal-{}.jsonl", std::process::id()));
        std::fs::create_dir_all(&path).expect("占位为目录");

        let err = WanningState::with_wal(Arc::new(MockClock::new(1500)), &path).unwrap_err();
        assert!(matches!(err, CoreError::WalIo(_)), "{err}");
    }

    #[test]
    fn replay_rebuilds_state_and_is_deterministic() {
        let path = tmp_wal("replay");
        let clock = MockClock::new(1500);
        let mut state = WanningState::with_wal(Arc::new(clock.clone()), &path).expect("开 WAL");
        state.register_delegation(delegation()).expect("注册");
        state.decide(&intent(1, 500)).expect("放行");
        state.decide(&intent(2, 9000)).expect("超额拒");
        state.decide(&intent(3, 100)).expect("再放行");
        state.revoke("d1").expect("撤销");
        state.decide(&intent(4, 100)).expect("撤销后拒");

        let live_hash = state.state_hash();
        assert_eq!(
            state.gate().spent_cents("d1"),
            Some(600),
            "实时累计消费 = 500 + 100"
        );

        // 回放两遍,hash 必须一致且等于实时状态。
        let replayed = WanningState::replay(&path).expect("回放");
        let hash_once = replayed.state_hash();
        let replayed_again = WanningState::replay(&path).expect("回放二遍");
        let hash_twice = replayed_again.state_hash();

        assert_eq!(hash_once, hash_twice, "回放两遍 hash 必相同(确定性)");
        assert_eq!(hash_once, live_hash, "回放态必须与实时态完全一致");
        assert_eq!(replayed.gate().spent_cents("d1"), Some(600));
        assert!(replayed.gate().is_revoked("d1"));
        assert!(
            replayed
                .gate()
                .replay_registry()
                .contains("agent:claude-code", 1),
            "重放登记也必须被重建"
        );
        assert_eq!(replayed.wal_line_count(), None, "回放态不追加记录");
    }

    #[test]
    fn replay_uses_recorded_ts_so_expiry_reproduces() {
        // 实时判定依赖时钟;回放若用真实时钟,过期委托会判成 Expired 与记录不符。
        // 这里验证回放按记录 ts 驱动,过期/未过期的判定都能精确复现。
        let path = tmp_wal("expiry");
        let clock = MockClock::new(1500);
        let mut state = WanningState::with_wal(Arc::new(clock.clone()), &path).expect("开 WAL");
        state.register_delegation(delegation()).expect("注册");
        state.decide(&intent(1, 100)).expect("放行");
        clock.set_now(2000); // 推到过期
        state.decide(&intent(2, 100)).expect("过期拒");

        let replayed = WanningState::replay(&path).expect("回放");
        assert_eq!(replayed.state_hash(), state.state_hash());
    }

    #[test]
    fn replay_fails_closed_on_corrupted_line() {
        let path = tmp_wal("replay-corrupt");
        let mut state = WanningState::with_wal(Arc::new(MockClock::new(1500)), &path).expect("开");
        state.register_delegation(delegation()).expect("注册");
        drop(state);
        // 追加半行
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("开");
        f.write_all(b"{\"kind\":\"decide\",\"ts\":1,\"dele\n")
            .expect("追加坏行");
        drop(f);

        match WanningState::replay(&path) {
            Err(CoreError::WalBadLine { line, .. }) => assert_eq!(line, 2),
            other => panic!("应 fail-closed 报错,实际 {other:?}"),
        }
    }

    #[test]
    fn replay_fails_closed_when_record_disagrees_with_recomputation() {
        // 手工构造一条与闸语义矛盾的记录:同一 nonce 两次「放行」。
        let path = tmp_wal("replay-tampered");
        let mut state = WanningState::with_wal(Arc::new(MockClock::new(1500)), &path).expect("开");
        state.register_delegation(delegation()).expect("注册");
        state.decide(&intent(1, 100)).expect("放行");
        drop(state);
        // 篡改:把同 nonce 的第二次放行直接写进 WAL(实时闸根本不可能放行它)。
        // 包裹形态与真实写入完全一致(seq 接续、prev = 前两行的链尾)——这正是链的
        // 已知边界:尾行内容没有后继行引用,链验不住,靠回放重算(语义对账)抓住。
        use std::io::Write;
        let verified = crate::wal::read_verified(&path).expect("读已有历史");
        let forged = crate::wal::WalLine {
            seq: verified.records.len() as u64 + 1,
            prev: verified.tail,
            rec: WalRecord::Decide {
                ts: 1500,
                decision: WalDecision::Allow,
                delegation_id: "d1".to_string(),
                intent: intent(1, 100),
                reason: None,
                budget_after_cents: 200,
            },
        };
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("开");
        f.write_all(serde_json::to_string(&forged).unwrap().as_bytes())
            .and_then(|()| f.write_all(b"\n"))
            .expect("追加");
        drop(f);

        match WanningState::replay(&path) {
            Err(CoreError::WalMismatch { line, message }) => {
                assert_eq!(line, 3, "不一致要指到行");
                assert!(message.contains("不一致"), "{message}");
            }
            other => panic!("篡改记录必须 fail-closed,实际 {other:?}"),
        }
    }

    #[test]
    fn audit_chain_tail_matches_independent_read_side_recompute() {
        // 完整性链对账证据:实时链尾 == 读侧逐行独立重算的链尾(两条路径各算各的)。
        let path = tmp_wal("chain-tail");
        let mut state = WanningState::with_wal(Arc::new(MockClock::new(1500)), &path).expect("开");
        state.register_delegation(delegation()).expect("注册");
        state.decide(&intent(1, 500)).expect("放行");
        state.decide(&intent(2, 9000)).expect("超额拒");
        state.revoke("d1").expect("撤销");

        let live_tail = state.audit_chain_tail().expect("必有 WAL");
        let verified = crate::wal::read_verified(&path).expect("读回验链");
        assert_eq!(verified.tail, live_tail, "读侧独立重算链尾 == 实时链尾");
        assert_eq!(
            WanningState::replay(&path).expect("回放").state_hash(),
            state.state_hash(),
            "链验过后,回放对账照常成立"
        );
    }

    #[test]
    fn live_resuming_fails_closed_on_broken_chain() {
        // 历史行被改(改的是不参与判定的 memo,语义对账抓不住)→ 链断 → 续跑拒启。
        // 至少三行:被改行必须有后继行引用它的链值,尾行是链的已知边界。
        let path = tmp_wal("resume-chain");
        {
            let mut state =
                WanningState::with_wal(Arc::new(MockClock::new(1500)), &path).expect("开");
            state.register_delegation(delegation()).expect("注册");
            state.decide(&intent(1, 100)).expect("放行");
            state.decide(&intent(2, 9000)).expect("超额拒");
        }
        let mut lines = crate::wal::raw_lines(&path).expect("读 WAL");
        let mut value: serde_json::Value = serde_json::from_str(&lines[1]).expect("行是 JSON");
        value["rec"]["intent"]["memo"] = serde_json::json!("被改写的备注");
        lines[1] = value.to_string();
        std::fs::write(&path, lines.join("\n") + "\n").expect("重写 WAL");

        match WanningState::live_resuming(&path) {
            Err(CoreError::WalChainBroken { line, .. }) => {
                assert_eq!(line, 3, "断链点 = 被改行的下一行(prev 对不上)")
            }
            other => panic!("链断裂必须拒启,实际 {other:?}"),
        }
    }

    #[test]
    fn state_hash_changes_when_state_changes() {
        let path = tmp_wal("hash");
        let clock = MockClock::new(1500);
        let mut state = WanningState::with_wal(Arc::new(clock.clone()), &path).expect("开");
        state.register_delegation(delegation()).expect("注册");
        let h0 = state.state_hash();
        state.decide(&intent(1, 100)).expect("放行");
        let h1 = state.state_hash();
        state.revoke("d1").expect("撤销");
        let h2 = state.state_hash();
        assert_ne!(h0, h1, "扣减后 hash 必变");
        assert_ne!(h1, h2, "撤销后 hash 必变");
    }

    #[test]
    fn empty_wal_replays_to_empty_state() {
        let path = tmp_wal("empty");
        std::fs::write(&path, "").expect("写空文件");
        let replayed = WanningState::replay(&path).expect("空 WAL 是合法状态");
        assert_eq!(
            replayed.state_hash(),
            WanningState::new(Arc::new(MockClock::new(0))).state_hash()
        );
    }

    // -----------------------------------------------------------------------
    // 断点续跑(live_resuming):长期服务重启必须从审计接续,绝不带空账本接着判
    // -----------------------------------------------------------------------

    #[test]
    fn live_resuming_carries_ledger_revocations_and_nonces() {
        let path = tmp_wal("resume");
        {
            let clock = MockClock::new(1500);
            let mut state = WanningState::with_wal(Arc::new(clock.clone()), &path).expect("开");
            state
                .register_delegation(long_lived_delegation())
                .expect("注册");
            state.decide(&intent(1, 500)).expect("放行");
            state.decide(&intent(2, 100)).expect("再放行");
            state.revoke("d1").expect("撤销");
        } // drop:进程「重启」

        let resumed = WanningState::live_resuming(&path).expect("续跑");
        // 账本/撤销/nonce 全部接续。
        assert_eq!(resumed.gate().spent_cents("d1"), Some(600));
        assert!(resumed.gate().is_revoked("d1"), "撤销必须跨重启存活");
        assert_eq!(
            resumed.state_hash(),
            WanningState::replay(&path).expect("回放").state_hash(),
            "续跑态与回放态必须一致"
        );
        // 时钟已回到「现在」:系统时钟,而非回放的冻结时刻 1500。
        assert!(
            resumed.gate().clock().now() > 1_700_000_000,
            "续跑必须用系统时钟,得到 {}",
            resumed.gate().clock().now()
        );

        // 续跑后的闸照常判定,且继续写同一份 WAL:撤销态下新意图被拒、旧 nonce 重放
        // 也被拒(闸口径 revoked 先于 replay,两条都落到拒),账本不动。
        let mut resumed = resumed;
        let deny = resumed.decide(&intent(3, 100)).expect("判定");
        assert_eq!(deny.deny_reason(), Some(DenyReason::Revoked));
        let replay_deny = resumed.decide(&intent(1, 100)).expect("判定");
        assert_eq!(replay_deny.deny_reason(), Some(DenyReason::Revoked));
        assert_eq!(resumed.gate().spent_cents("d1"), Some(600), "账本不动");
        let records = crate::wal::read_records(&path).expect("读回");
        assert_eq!(records.len(), 6, "注册+2 放行+撤销+续跑后 2 条拒绝");
    }

    #[test]
    fn live_resuming_on_fresh_wal_starts_empty() {
        let path = tmp_wal("resume-fresh");
        let mut state = WanningState::live_resuming(&path).expect("新 WAL 直接续跑=空账开张");
        assert_eq!(
            state.state_hash(),
            WanningState::replay(&path).expect("回放").state_hash()
        );
        state
            .register_delegation(long_lived_delegation())
            .expect("注册");
        assert!(state.decide(&intent(1, 100)).expect("判定").is_allow());
    }

    #[test]
    fn live_resuming_fails_closed_on_corrupted_wal() {
        let path = tmp_wal("resume-corrupt");
        {
            let mut state =
                WanningState::with_wal(Arc::new(MockClock::new(1500)), &path).expect("开");
            state.register_delegation(delegation()).expect("注册");
        }
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("开");
        f.write_all(b"{\"kind\":\"decide\",\"ts\":1,\"dele\n")
            .expect("追加坏行");
        drop(f);

        match WanningState::live_resuming(&path) {
            Err(CoreError::WalBadLine { line, .. }) => assert_eq!(line, 2),
            other => panic!("审计损坏必须拒启,实际 {other:?}"),
        }
    }
}
