//! W-53a 人在环待支付:五段事件链 + 三钉 + 回放对账(先红后绿)。
//!
//! 五段:①意图 + ②审批共用既有 Decide 行(意图与判定原子一行,W-53 决策记录在档);
//! ③Pending 行(审批额 + TTL)→ ④Confirm 行(幂等 + 支付凭证)→ ⑤Terminal 行(完成 /
//! TTL 过期作废)。三钉:金额一致 / 幂等 / TTL。
//! 待支付单 = 账本里的一行状态:**零通道 API、零网络、零外联**;金额全整数(分);
//! 每一行都过 W-21 完整性链,回放可逐段对账。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use wanning_core::clock::{Clock, MockClock, SystemClock};
use wanning_core::delegation::Delegation;
use wanning_core::error::CoreError;
use wanning_core::gate::{DenyReason, GateDecision};
use wanning_core::intent::SpendIntent;
use wanning_core::pending::{PayMode, PendingError, PendingOutcome, PendingState};
use wanning_core::state::WanningState;
use wanning_core::wal::{read_verified, Wal, WalDecision, WalRecord};

fn tmp_wal(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join("wanning-pending-tests");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    // pid + 原子序号 + 纳秒:裸 pid 跨轮运行会撞残留账本(W-21 教训,W-43b 轮补齐)。
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!(
        "{tag}-{}-{}-{nanos}.jsonl",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    ))
}

/// 委托窗口开到很远的将来:TTL 钉的测试要能自由推时钟。
fn delegation() -> Delegation {
    Delegation::new(
        "d1",
        "所有者",
        "claude-code",
        1000,
        1000,
        1_000_000,
        "agent:claude-code",
    )
}

fn intent(nonce: u64, amount_cents: u64) -> SpendIntent {
    SpendIntent::new("d1", nonce, amount_cents, "jd:shop-1", "grocery", "测试")
}

/// 续跑测试专用:开单与确认是两个真实进程(系统时钟),委托窗口必须盖住墙钟。
fn long_lived_delegation() -> Delegation {
    Delegation::new(
        "d1",
        "所有者",
        "claude-code",
        1000,
        1000,
        SystemClock.now().saturating_add(86_400),
        "agent:claude-code",
    )
}

/// 开账本并注册委托,同时交回时钟句柄(MockClock 的 clone 共享同一原子时刻,
/// 推时间靠它,绝不 sleep)。
fn state_with_wal(wal: &Path) -> (WanningState, MockClock) {
    let clock = MockClock::new(1500);
    let mut state = WanningState::with_wal(Arc::new(clock.clone()), wal).expect("开账本");
    state.register_delegation(delegation()).expect("注册委托");
    (state, clock)
}

/// 造一笔已放行的待支付单(①②③三行:注册 / 判定 / 待支付)。
fn open_pending(wal: &Path) -> (WanningState, MockClock, String) {
    let (mut state, clock) = state_with_wal(wal);
    let (verdict, receipt) = state
        .decide_opening_pending(&intent(1, 400), 900)
        .expect("放行并开待支付单");
    assert!(verdict.is_allow());
    let receipt = receipt.expect("pending_pay 档位放行必开单");
    (state, clock, receipt.pending_id)
}

fn wal_line_count(wal: &Path) -> usize {
    std::fs::read_to_string(wal)
        .expect("读账本")
        .lines()
        .count()
}

// ── 通道档位 ─────────────────────────────────────────────────────────────

#[test]
fn pay_mode_defaults_to_pending_pay_and_serializes_snake_case() {
    assert_eq!(PayMode::default(), PayMode::PendingPay);
    for (mode, name) in [
        (PayMode::PendingPay, "pending_pay"),
        (PayMode::AutoDebit, "auto_debit"),
        (PayMode::Manual, "manual"),
    ] {
        assert_eq!(
            serde_json::to_value(mode).unwrap(),
            serde_json::Value::String(name.to_string()),
            "档位落审计/配置要人能读懂"
        );
    }
    // 只有第一形态开待支付单;manual / auto_debit 不改闸判定面。
    assert!(PayMode::PendingPay.opens_pending());
    assert!(!PayMode::AutoDebit.opens_pending());
    assert!(!PayMode::Manual.opens_pending());
}

// ── ①②③:放行开单 / 拒绝不开单 ─────────────────────────────────────────

#[test]
fn allow_opens_pending_row_with_approval_amount_and_ttl() {
    let wal = tmp_wal("allow-opens");
    let (mut state, _clock) = state_with_wal(&wal);
    let (verdict, receipt) = state
        .decide_opening_pending(&intent(1, 400), 900)
        .expect("放行并开单");

    assert_eq!(
        verdict,
        GateDecision::Allow {
            budget_after_cents: 400
        }
    );
    let receipt = receipt.expect("放行必开待支付单");
    assert_eq!(receipt.approved_amount_cents, 400, "审批额 = 意图额");
    assert_eq!(receipt.expires_ts, 1500 + 900, "TTL 从审批时刻起算");
    assert!(
        receipt.pending_id.starts_with("p-"),
        "待支付单 id 形状: {}",
        receipt.pending_id
    );
    assert_eq!(receipt.wal_line, Some(3), "行1=注册 行2=判定 行3=待支付");

    // ③待支付是一行账本状态,不是通道请求:WAL 恰三行,行3 kind=pending。
    let records = read_verified(&wal).expect("验链读回").records;
    assert_eq!(records.len(), 3);
    assert_eq!(records[2].1.kind(), "pending");
    match &records[2].1 {
        WalRecord::Pending {
            ts,
            pending_id,
            delegation_id,
            intent: row_intent,
            approved_amount_cents,
            expires_ts,
        } => {
            assert_eq!(*ts, 1500);
            assert_eq!(pending_id, &receipt.pending_id);
            assert_eq!(delegation_id, "d1");
            assert_eq!(row_intent.amount_cents, 400);
            assert_eq!(*approved_amount_cents, 400, "待支付行自带审批额");
            assert_eq!(*expires_ts, 2400);
        }
        other => panic!("行3 应为 pending 行: {other:?}"),
    }
}

#[test]
fn deny_writes_no_pending_row() {
    let wal = tmp_wal("deny-no-pending");
    let (mut state, _clock) = state_with_wal(&wal);
    // 2000 分 > 上限 1000 分 → over_budget。
    let (verdict, receipt) = state
        .decide_opening_pending(&intent(1, 2000), 900)
        .expect("判定本身不算错");
    assert!(matches!(verdict, GateDecision::Deny { reason: _ }));
    assert!(receipt.is_none(), "拒绝不开待支付单");
    assert!(state.pendings().is_empty());
    assert_eq!(wal_line_count(&wal), 2, "行1=注册 行2=拒绝,一行不多");
}

#[test]
fn ttl_zero_is_rejected_before_any_row_is_written() {
    let wal = tmp_wal("ttl-zero");
    let (mut state, _clock) = state_with_wal(&wal);
    let err = state
        .decide_opening_pending(&intent(1, 400), 0)
        .expect_err("TTL=0 的待支付单没有存在意义,fail-closed");
    assert!(matches!(
        err,
        CoreError::Pending(PendingError::InvalidTtl { .. })
    ));
    assert!(state.pendings().is_empty());
    // 只剩 state_with_wal 的注册行:TTL 非法在判定与开单之前就被拒
    // (API 误用零审计噪音,W-25 先例),否则会出现「已记账却开不出单」的中间世界。
    assert_eq!(wal_line_count(&wal), 1, "拒绝不开单也不写行");
}

#[test]
fn pending_ids_are_unique_across_orders() {
    let wal = tmp_wal("unique-ids");
    let (mut state, _clock) = state_with_wal(&wal);
    let (_, first) = state
        .decide_opening_pending(&intent(1, 100), 900)
        .expect("第一单");
    let (_, second) = state
        .decide_opening_pending(&intent(2, 100), 900)
        .expect("第二单(同一秒内连开,撞 id 必须被重试逻辑避开)");
    assert_ne!(
        first.unwrap().pending_id,
        second.unwrap().pending_id,
        "同一时钟刻连开两单,单号也不得重复"
    );
}

// ── ④⑤:人确认 → 终态 ──────────────────────────────────────────────────

#[test]
fn human_confirm_writes_confirm_and_terminal_rows() {
    let wal = tmp_wal("confirm-happy");
    let (mut state, _clock, pending_id) = open_pending(&wal);

    let order = state
        .confirm_pending(&pending_id, 400, "TRADE-20260903-0001")
        .expect("人确认成功");
    assert_eq!(order.state, PendingState::Completed);
    assert_eq!(order.proof.as_deref(), Some("TRADE-20260903-0001"));
    assert_eq!(order.confirmed_ts, Some(1500));

    // 五段全在:注册 / 判定 / 待支付 / 确认 / 终态(完成)。
    let records = read_verified(&wal).expect("验链读回").records;
    let kinds: Vec<&str> = records.iter().map(|(_, r)| r.kind()).collect();
    assert_eq!(
        kinds,
        vec![
            "register_delegation",
            "decide",
            "pending",
            "confirm",
            "terminal"
        ],
        "五段事件链一行不缺: {kinds:?}"
    );
    match &records[3].1 {
        WalRecord::Confirm {
            ts,
            pending_id: row_id,
            amount_cents,
            proof,
        } => {
            assert_eq!(*ts, 1500);
            assert_eq!(row_id, &pending_id);
            assert_eq!(*amount_cents, 400, "确认额必须等于审批额");
            assert_eq!(proof, "TRADE-20260903-0001", "支付凭证入账,回放可对账");
        }
        other => panic!("行4 应为 confirm 行: {other:?}"),
    }
    match &records[4].1 {
        WalRecord::Terminal {
            ts,
            pending_id: row_id,
            outcome,
        } => {
            assert_eq!(*ts, 1500);
            assert_eq!(row_id, &pending_id);
            assert_eq!(*outcome, PendingOutcome::Completed);
        }
        other => panic!("行5 应为 terminal 行: {other:?}"),
    }
}

#[test]
fn confirm_at_the_last_moment_before_ttl_still_succeeds() {
    let wal = tmp_wal("confirm-just-in-time");
    let (mut state, clock, pending_id) = open_pending(&wal);
    // 半开窗口 [created, expires):恰在 expires 前一秒仍是有效确认。
    clock.set_now(2399);
    state
        .confirm_pending(&pending_id, 400, "T-1")
        .expect("TTL 窗口内确认有效");
}

// ── 三钉 ────────────────────────────────────────────────────────────────

#[test]
fn nail_amount_mismatch_is_rejected_and_writes_nothing() {
    let wal = tmp_wal("nail-amount");
    let (mut state, _clock, pending_id) = open_pending(&wal);
    let before = wal_line_count(&wal);

    let err = state
        .confirm_pending(&pending_id, 500, "TRADE-1")
        .expect_err("审批 400 确认 500 = 拒(防夹带)");
    assert!(matches!(
        err,
        CoreError::Pending(PendingError::AmountMismatch { .. })
    ));
    assert_eq!(wal_line_count(&wal), before, "被拒的确认一行都不落");
    assert_eq!(
        state.pendings().get(&pending_id).expect("单还在").state,
        PendingState::Open
    );
}

#[test]
fn nail_double_confirm_is_rejected_and_writes_nothing() {
    let wal = tmp_wal("nail-idempotent");
    let (mut state, _clock, pending_id) = open_pending(&wal);
    state
        .confirm_pending(&pending_id, 400, "TRADE-1")
        .expect("第一次确认成功");
    let before = wal_line_count(&wal);

    let err = state
        .confirm_pending(&pending_id, 400, "TRADE-1")
        .expect_err("同一单只能确认一次(幂等)");
    assert!(matches!(
        err,
        CoreError::Pending(PendingError::NotOpen { .. })
    ));
    assert_eq!(wal_line_count(&wal), before, "二次确认零落账");
}

#[test]
fn nail_expired_confirm_voids_the_order_and_is_rejected() {
    let wal = tmp_wal("nail-ttl");
    let (mut state, clock, pending_id) = open_pending(&wal);
    // 半开窗口 [1500, 2400):恰在 expires 时刻 = 已过期,fail-closed。
    clock.set_now(2400);
    let before = wal_line_count(&wal);

    let err = state
        .confirm_pending(&pending_id, 400, "TRADE-1")
        .expect_err("过期确认 = 拒");
    assert!(matches!(
        err,
        CoreError::Pending(PendingError::Expired { .. })
    ));

    // 过期作废要落 ⑤终态行(过期作废),账本行状态同步为 Voided。
    assert_eq!(wal_line_count(&wal), before + 1, "只多一行作废行");
    let records = read_verified(&wal).expect("验链读回").records;
    match &records.last().expect("有行").1 {
        WalRecord::Terminal { outcome, .. } => assert_eq!(*outcome, PendingOutcome::ExpiredVoid),
        other => panic!("末行应为过期作废 terminal 行: {other:?}"),
    }
    assert_eq!(
        state.pendings().get(&pending_id).expect("单还在").state,
        PendingState::Voided
    );
}

// ── 其余 fail-closed 路径(零噪音) ──────────────────────────────────────

#[test]
fn unknown_pending_id_is_rejected_with_zero_rows() {
    let wal = tmp_wal("unknown-pending");
    let (mut state, _clock, _) = open_pending(&wal);
    let before = wal_line_count(&wal);
    let err = state
        .confirm_pending("p-does-not-exist", 400, "TRADE-1")
        .expect_err("未知单号 = 拒");
    assert!(matches!(
        err,
        CoreError::Pending(PendingError::UnknownPending { .. })
    ));
    assert_eq!(wal_line_count(&wal), before);
}

#[test]
fn empty_proof_is_rejected_with_zero_rows() {
    let wal = tmp_wal("empty-proof");
    let (mut state, _clock, pending_id) = open_pending(&wal);
    let before = wal_line_count(&wal);
    for bad in ["", "   "] {
        let err = state
            .confirm_pending(&pending_id, 400, bad)
            .expect_err("没有支付凭证的确认 = 拒");
        assert!(matches!(err, CoreError::Pending(PendingError::EmptyProof)));
    }
    assert_eq!(wal_line_count(&wal), before);
}

// ── 回放对账 + 完整性链 ─────────────────────────────────────────────────

#[test]
fn replay_reconstructs_pendings_and_hash_matches_live() {
    let wal = tmp_wal("replay-hash");
    let (mut state, _clock, pending_id) = open_pending(&wal);

    // 未确认的单:实时态 == 回放态。
    let live_hash = state.state_hash();
    let replayed = WanningState::replay(&wal).expect("回放");
    assert_eq!(replayed.state_hash(), live_hash, "未确认单也要进指纹");

    // 确认后的五段链:实时态 == 回放态;确认前后的指纹必须不同
    // (待支付单的状态演化必须进 state_hash,否则「重启洗掉确认」对账不出来)。
    state
        .confirm_pending(&pending_id, 400, "TRADE-1")
        .expect("确认");
    assert_ne!(
        state.state_hash(),
        live_hash,
        "确认改变待支付单状态,state_hash 必须跟着变"
    );
    let replayed = WanningState::replay(&wal).expect("回放");
    assert_eq!(replayed.state_hash(), state.state_hash());

    let order = replayed
        .pendings()
        .get(&pending_id)
        .expect("回放后待支付单可见");
    assert_eq!(order.state, PendingState::Completed);
    assert_eq!(order.proof.as_deref(), Some("TRADE-1"));
}

#[test]
fn live_resuming_carries_pendings_across_restart_then_human_confirms() {
    let wal = tmp_wal("resume-confirm");
    // 真实旅程用系统时钟:开单进程与确认进程是两个真实进程,时刻是墙钟,
    // TTL 从审批时刻起算——人随后确认,落在窗口内。
    let mut first = WanningState::live_resuming(&wal).expect("第一次进程");
    first
        .register_delegation(long_lived_delegation())
        .expect("注册委托");
    let (_, receipt) = first
        .decide_opening_pending(&intent(1, 400), 900)
        .expect("放行并开单");
    let pending_id = receipt.expect("pending_pay 档位放行必开单").pending_id;
    drop(first);

    // 「重启」:新进程从审计接续,待支付单跨重启仍在,人确认照常。
    let mut resumed = WanningState::live_resuming(&wal).expect("续跑");
    let order = resumed
        .confirm_pending(&pending_id, 400, "TRADE-RESUME")
        .expect("重启后人确认照常");
    assert_eq!(order.state, PendingState::Completed);
    assert_eq!(order.proof.as_deref(), Some("TRADE-RESUME"));
    assert_eq!(
        WanningState::replay(&wal).expect("回放").state_hash(),
        resumed.state_hash(),
        "续跑落账后仍可回放对账"
    );
}

#[test]
fn w21_chain_holds_over_five_segment_rows_and_reopens() {
    let wal = tmp_wal("chain-five-segments");
    let (mut state, _clock, pending_id) = open_pending(&wal);
    let tail_after_three = state.audit_chain_tail().expect("链尾");
    state
        .confirm_pending(&pending_id, 400, "TRADE-CHAIN")
        .expect("确认");

    let verified = read_verified(&wal).expect("五段行整链可验");
    assert_eq!(verified.records.len(), 5);
    assert_eq!(verified.tail, state.audit_chain_tail().expect("链尾"));
    assert_ne!(verified.tail, tail_after_three, "确认与终态推进了链尾");

    // 带完整历史的账本可继续服务(Wal::open 先验全链再续写)。续跑态的时钟是
    // 系统时钟,mock 窗口的委托(1000..1_000_000)在它下面已过期——续写一条
    // 「过期拒」判定:拒绝对审计面同样是合法续写,链尾照常推进。
    // (旧进程先放单写者锁:锁只挡写进程,W-18。)
    drop(state);
    let mut reopened = WanningState::live_resuming(&wal).expect("验链后续跑");
    let verdict = reopened.decide(&intent(2, 100)).expect("续跑后闸照常服务");
    assert_eq!(
        verdict.deny_reason(),
        Some(DenyReason::Expired),
        "mock 窗口委托在系统时钟下过期,拒绝必须复现闸口径"
    );
    assert_eq!(reopened.wal_line_count(), Some(6), "五段链 + 续写一行");
    assert_eq!(
        read_verified(&wal).expect("续写后整链仍可验").records.len(),
        6
    );
}

// ── 过期作废的批量物化(sweep) ──────────────────────────────────────────

#[test]
fn void_expired_pendings_sweeps_only_expired_once() {
    let wal = tmp_wal("sweep");
    let (mut state, clock, expired_id) = open_pending(&wal);
    let (_, fresh_id) = state
        .decide_opening_pending(&intent(2, 100), 9000)
        .expect("第二单(未过期)");
    let fresh_id = fresh_id.expect("第二单必开单").pending_id;
    clock.set_now(2400);

    let voided = state.void_expired_pendings().expect("sweep");
    assert_eq!(voided, vec![expired_id.clone()], "只作废过期的单");
    assert_eq!(
        state.pendings().get(&expired_id).unwrap().state,
        PendingState::Voided
    );
    assert_eq!(
        state.pendings().get(&fresh_id).unwrap().state,
        PendingState::Open
    );
    // 幂等:再扫一遍,没有新作废行。
    assert!(state.void_expired_pendings().expect("再扫").is_empty());
    assert_eq!(
        WanningState::replay(&wal).expect("回放").state_hash(),
        state.state_hash()
    );
}

// ── 回放侧 fail-closed:链合法但语义不通的账本,回放必须拒 ────────────────
//
// 伪造手法 = 用真 Wal 追加伪造记录(链自己会算对,只有回放语义能抓住——这正是
// W-21 已知盲区「语义对账」层存在的意义)。

fn append_record(wal: &Path, record: &WalRecord) {
    let mut wal_handle = Wal::open(wal).expect("打开账本追加");
    wal_handle.append(record).expect("追加(链自动成链)");
}

fn forged_pending_record(ts: u64, nonce: u64, amount: u64) -> WalRecord {
    WalRecord::Pending {
        ts,
        pending_id: format!("p-forged-{nonce}-{amount}"),
        delegation_id: "d1".to_string(),
        intent: intent(nonce, amount),
        approved_amount_cents: amount,
        expires_ts: ts + 600,
    }
}

#[test]
fn replay_rejects_pending_row_without_a_matching_allow() {
    let wal = tmp_wal("forge-pending-no-allow");
    {
        let (mut state, _clock) = state_with_wal(&wal);
        // ②审批 = 拒绝(over_budget),随后伪造 ③待支付行 → 回放必须抓出。
        let (verdict, _) = state
            .decide_opening_pending(&intent(1, 2000), 900)
            .expect("拒绝也是判定");
        assert!(matches!(verdict, GateDecision::Deny { .. }));
    }
    // 活闸持着单写者锁,伪造追加前先放锁(锁只挡写进程,W-18)。
    append_record(&wal, &forged_pending_record(1500, 1, 2000));

    let err = WanningState::replay(&wal).expect_err("没有放行就没有待支付");
    assert!(matches!(err, CoreError::WalMismatch { .. }), "{err}");
}

#[test]
fn replay_rejects_pending_row_whose_amount_differs_from_the_allow() {
    let wal = tmp_wal("forge-pending-amount");
    {
        let (mut state, _clock) = state_with_wal(&wal);
        state
            .decide_opening_pending(&intent(1, 400), 900)
            .expect("放行");
    }
    // 放行 400,待支付行却写 500(夹带)。
    append_record(&wal, &forged_pending_record(1500, 1, 500));

    let err = WanningState::replay(&wal).expect_err("待支付行的审批额与放行不符");
    assert!(matches!(err, CoreError::WalMismatch { .. }), "{err}");
}

#[test]
fn replay_rejects_duplicate_pending_id() {
    let wal = tmp_wal("forge-duplicate-id");
    let (state, _clock, pending_id) = open_pending(&wal);
    drop(state);
    // 同一单号再来一行(同意图、同审批额——伪造的只是「把③开单行原样重交一次」):
    // 单号必须唯一,台账语义拒,回放 fail-closed。
    append_record(
        &wal,
        &WalRecord::Pending {
            ts: 1501,
            pending_id: pending_id.clone(),
            delegation_id: "d1".to_string(),
            intent: intent(1, 400),
            approved_amount_cents: 400,
            expires_ts: 2400,
        },
    );

    let err = WanningState::replay(&wal).expect_err("单号必须唯一");
    assert!(matches!(err, CoreError::WalMismatch { .. }), "{err}");
}

#[test]
fn replay_rejects_confirm_with_amount_that_differs_from_the_pending() {
    let wal = tmp_wal("forge-confirm-amount");
    let (state, _clock, pending_id) = open_pending(&wal);
    drop(state);
    append_record(
        &wal,
        &WalRecord::Confirm {
            ts: 1600,
            pending_id: pending_id.clone(),
            amount_cents: 500,
            proof: "TRADE-X".to_string(),
        },
    );
    let err = WanningState::replay(&wal).expect_err("确认额 ≠ 审批额,回放拒");
    assert!(matches!(err, CoreError::WalMismatch { .. }), "{err}");
}

#[test]
fn replay_rejects_second_confirm_row() {
    let wal = tmp_wal("forge-double-confirm");
    let (mut state, _clock, pending_id) = open_pending(&wal);
    state
        .confirm_pending(&pending_id, 400, "TRADE-1")
        .expect("第一次确认");
    drop(state);
    append_record(
        &wal,
        &WalRecord::Confirm {
            ts: 1601,
            pending_id: pending_id.clone(),
            amount_cents: 400,
            proof: "TRADE-2".to_string(),
        },
    );
    let err = WanningState::replay(&wal).expect_err("二次确认行,回放拒");
    assert!(matches!(err, CoreError::WalMismatch { .. }), "{err}");
}

#[test]
fn replay_rejects_completed_terminal_without_confirm() {
    let wal = tmp_wal("forge-terminal-no-confirm");
    let (state, _clock, pending_id) = open_pending(&wal);
    drop(state);
    append_record(
        &wal,
        &WalRecord::Terminal {
            ts: 1600,
            pending_id: pending_id.clone(),
            outcome: PendingOutcome::Completed,
        },
    );
    let err = WanningState::replay(&wal).expect_err("没有确认就没有完成");
    assert!(matches!(err, CoreError::WalMismatch { .. }), "{err}");
}

#[test]
fn replay_rejects_void_terminal_before_expiry() {
    let wal = tmp_wal("forge-void-early");
    let (state, _clock, pending_id) = open_pending(&wal);
    drop(state);
    // 还没到 expires_ts(2400)就写作废行。
    append_record(
        &wal,
        &WalRecord::Terminal {
            ts: 2000,
            pending_id: pending_id.clone(),
            outcome: PendingOutcome::ExpiredVoid,
        },
    );
    let err = WanningState::replay(&wal).expect_err("没到期不能作废");
    assert!(matches!(err, CoreError::WalMismatch { .. }), "{err}");
}

// ── Deny 行形状不受影响(回归钉) ────────────────────────────────────────

#[test]
fn plain_decide_still_writes_the_old_two_row_shape() {
    let wal = tmp_wal("plain-decide");
    let (mut state, _clock) = state_with_wal(&wal);
    let verdict = state.decide(&intent(1, 400)).expect("判定");
    assert!(verdict.is_allow());
    assert!(state.pendings().is_empty(), "纯闸路径不开单");
    assert_eq!(wal_line_count(&wal), 2, "注册 + 判定,行为不漂移");
    let records = read_verified(&wal).expect("验链").records;
    assert_eq!(records[1].1.kind(), "decide");
    match &records[1].1 {
        WalRecord::Decide { decision, .. } => assert_eq!(*decision, WalDecision::Allow),
        other => panic!("{other:?}"),
    }
}
