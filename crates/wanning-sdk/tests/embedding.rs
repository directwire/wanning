//! 嵌入契约测试:SDK 门面必须把四条硬语义从「调用方纪律」变成「类型系统强制」。
//!
//! 每条测试锁一条语义,口径与 core 侧测试一致;任何一条红 = 门面漏了它本该
//! 结构性挡住的东西。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use wanning_core::clock::{Clock, SystemClock};
use wanning_core::error::CoreError;
use wanning_core::gate::{DenyReason, GateDecision};
use wanning_sdk::{Delegation, SpendRequest, Wanning};

/// 进程内原子序号:临时 WAL 名绝不用裸 pid/纳秒(Windows 时钟粒度同 tick 撞名,
/// 两用例抢同一把单写者锁会让输方 WalLocked panic——见 W-21 教训)。
static SEQ: AtomicU64 = AtomicU64::new(0);

fn tmp_wal(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("wanning-sdk-tests");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    // pid + 原子序号 + 纳秒:序号每轮从 0 重新计数,pid 被复用时跨轮仍会撞名,
    // 纳秒补上跨轮唯一性(W-21 教训,W-43b 轮补齐)。
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!("{tag}-{}-{n}-{nanos}.jsonl", std::process::id()))
}

/// ¥10 总预算、系统时钟下长期有效、nonce 作用域 agent:x 的委托。
fn delegation(id: &str) -> Delegation {
    Delegation::new(
        id,
        "boss",
        "claude-code",
        1000,
        1000,
        SystemClock.now().checked_add(86_400).expect("有效期溢出"),
        "agent:x",
    )
}

fn request(amount_cents: u64) -> SpendRequest {
    SpendRequest::new(amount_cents, "jd:shop-1", "grocery", "嵌入测试")
}

// ---------------------------------------------------------------------------
// ① 开机必续放:open 是唯一入口,且永远先回放对账再接续旧账
//    (W-17 的真 bug——`live` 不回放、nonce 洗白、撤销复活——在 SDK 面
//    必须不可再写出)
// ---------------------------------------------------------------------------

#[test]
fn open_always_resumes_prior_state_and_keeps_writing_same_wal() {
    let path = tmp_wal("resume");
    {
        let mut gate = Wanning::open(&path).expect("首开");
        gate.authorize(delegation("d1")).expect("注册");
        let v = gate.decide("d1", request(500)).expect("判定");
        assert!(v.decision.is_allow());
        gate.revoke("d1").expect("撤销");
    } // drop = 进程「重启」

    let lines_after_first_life = wanning_core::wal::read_records(&path).expect("读回").len() as u64;

    let mut gate = Wanning::open(&path).expect("重开必须接续旧账");
    // 账本接续:累计消费 500、剩余 500。
    assert_eq!(gate.spent_cents("d1"), Some(500), "账本必须跨重启存活");
    assert_eq!(gate.remaining_cents("d1"), Some(500));
    // 撤销接续(kill switch 绝不复活)。
    assert!(gate.is_revoked("d1"), "撤销必须跨重启存活");
    // nonce 登记接续:重开后的下一个注入 nonce 不回到 1。
    let verdict = gate.decide("d1", request(100)).expect("判定");
    assert_eq!(
        verdict.decision,
        GateDecision::Deny {
            reason: DenyReason::Revoked
        },
        "重开不得复活已撤销的委托"
    );
    // 续写同一份 WAL(行数只增不减,续跑判定照常落审计)。
    let lines_now = wanning_core::wal::read_records(&path).expect("读回").len() as u64;
    assert_eq!(
        lines_now,
        lines_after_first_life + 1,
        "续跑判定必须落同一份审计"
    );
    assert_eq!(gate.last_wal_line(), Some(lines_now));
}

// ---------------------------------------------------------------------------
// ② 闸侧注入:SpendRequest 根本没有 delegation_id/nonce 字段(类型上无法
//    把模型给的越权字段带进来);注入的 nonce 单调,且「拒绝不耗号」
//    (core 语义经门面保持:被拒的 nonce 下次原样复用)
// ---------------------------------------------------------------------------

#[test]
fn injected_nonce_is_monotonic_and_denied_intent_does_not_consume_it() {
    let path = tmp_wal("nonce");
    let mut gate = Wanning::open(&path).expect("开闸");
    gate.authorize(delegation("d1")).expect("注册");

    let v1 = gate.decide("d1", request(100)).expect("判定");
    assert_eq!(v1.nonce, 1, "首个注入 nonce 从 1 起");
    assert!(v1.decision.is_allow());

    let v2 = gate.decide("d1", request(9000)).expect("判定");
    assert_eq!(v2.nonce, 2, "注入 nonce 单调");
    assert_eq!(
        v2.decision,
        GateDecision::Deny {
            reason: DenyReason::OverBudget
        }
    );

    let v3 = gate.decide("d1", request(100)).expect("判定");
    assert_eq!(
        v3.nonce, 2,
        "上一笔被拒不耗号:同一 nonce 原样复用(修好后重发合法)"
    );
    assert!(v3.decision.is_allow());

    let v4 = gate.decide("d1", request(100)).expect("判定");
    assert_eq!(v4.nonce, 3, "放行后才消耗,下一个 nonce 才前进");

    // WAL 行数 = 注册 1 + 判定 4(拒绝也落审计)。
    assert_eq!(
        wanning_core::wal::read_records(&path).expect("读回").len(),
        5
    );
}

#[test]
fn shared_nonce_scope_across_delegations_allocates_without_collision() {
    // 两份委托同一 nonce_scope:闸注入必须看「作用域」,不能看「委托」——
    // 否则第二份委托拿到已消耗的 nonce,白白吃一个 replay 拒绝。
    let path = tmp_wal("scope");
    let mut gate = Wanning::open(&path).expect("开闸");
    gate.authorize(delegation("d1")).expect("注册 d1");
    gate.authorize(delegation("d2")).expect("注册 d2");

    let a = gate.decide("d1", request(100)).expect("判定");
    assert_eq!(a.nonce, 1);
    assert!(a.decision.is_allow());

    let b = gate.decide("d2", request(100)).expect("判定");
    assert_eq!(b.nonce, 2, "同作用域跨委托:注入必须避开已消耗 nonce");
    assert!(
        b.decision.is_allow(),
        "第二份委托的第一笔必须放行,而不是撞 replay"
    );
}

// ---------------------------------------------------------------------------
// ③ 无审计不服务:open 必带 WAL(没有无审计路径);单写者锁跨进程跨句柄
//    fail-closed;重复注册/未知委托是 API 误用(Err,零审计噪音),
//    而非法金额是业务拒绝(判过、落审计)
// ---------------------------------------------------------------------------

#[test]
fn second_handle_on_same_wal_fails_closed() {
    let path = tmp_wal("double-open");
    let _first = Wanning::open(&path).expect("首开持锁");
    let second = Wanning::open(&path);
    match second {
        Err(CoreError::WalLocked { path: p, .. }) => {
            assert!(p.ends_with(".lock"), "报错要点名锁文件: {p}");
        }
        other => panic!("同一份 WAL 第二个句柄必须 fail-closed 拒启,实际 {other:?}"),
    }
}

#[test]
fn unknown_delegation_and_duplicate_authorize_are_api_misuse_without_audit_noise() {
    let path = tmp_wal("misuse");
    let mut gate = Wanning::open(&path).expect("开闸");
    gate.authorize(delegation("d1")).expect("注册");

    // 未知委托:Err,不落审计(没有判定发生,审计不该长出假意图)。
    let err = gate.decide("ghost", request(100)).unwrap_err();
    assert!(
        matches!(err, CoreError::UnknownDelegation(ref id) if id == "ghost"),
        "{err}"
    );
    // 重复注册:Err,不落审计(改预算 = 篡改审计,绝不以「再注册一次」实现)。
    let dup = gate.authorize(delegation("d1"));
    assert!(
        matches!(dup, Err(CoreError::DuplicateDelegation(ref id)) if id == "d1"),
        "{dup:?}"
    );
    // 上面三下动作后 WAL 恰 1 行(只有那次注册)。
    assert_eq!(
        wanning_core::wal::read_records(&path).expect("读回").len(),
        1,
        "API 误用不得产生审计行"
    );
}

#[test]
fn invalid_request_is_a_judged_deny_audited_not_an_error() {
    // 门面是忠实的:意图自身非法仍走闸的阶段 0,给出可审计的业务拒绝,
    // 与 core 的口径一致(非法意图不必看委托状态,但必须留痕)。
    let path = tmp_wal("stage0");
    let mut gate = Wanning::open(&path).expect("开闸");
    gate.authorize(delegation("d1")).expect("注册");

    let v = gate.decide("d1", request(0)).expect("判定");
    assert_eq!(
        v.decision,
        GateDecision::Deny {
            reason: DenyReason::InvalidAmount
        }
    );
    assert_eq!(
        wanning_core::wal::read_records(&path).expect("读回").len(),
        2,
        "注册 + 这笔拒绝(留痕)"
    );
    assert_eq!(gate.last_wal_line(), Some(2));
}

#[test]
fn revoke_is_kill_switch_and_repeat_revoke_is_audited() {
    let path = tmp_wal("revoke");
    let mut gate = Wanning::open(&path).expect("开闸");
    gate.authorize(delegation("d1")).expect("注册");
    assert!(gate
        .decide("d1", request(100))
        .expect("判定")
        .decision
        .is_allow());

    gate.revoke("d1").expect("撤销");
    assert!(gate.is_revoked("d1"));
    let denied = gate.decide("d1", request(100)).expect("判定");
    assert_eq!(
        denied.decision,
        GateDecision::Deny {
            reason: DenyReason::Revoked
        }
    );
    assert_eq!(
        gate.spent_cents("d1"),
        Some(100),
        "kill switch 是止血,不是抹账"
    );

    let before = wanning_core::wal::read_records(&path).expect("读回").len();
    gate.revoke("d1").expect("重复撤销幂等成功");
    assert_eq!(
        wanning_core::wal::read_records(&path).expect("读回").len(),
        before + 1,
        "重复撤销也落审计(决策/撤销各恰一行,W-16 记账完备口径)"
    );

    // 未知委托撤销:Err(授权者拼错 id 不能悄悄吞掉)。
    assert!(matches!(
        gate.revoke("ghost"),
        Err(CoreError::UnknownDelegation(_))
    ));
}

// ---------------------------------------------------------------------------
// ④ 审计可自证:self_check = 验链 + 回放对账(读侧独立重算);
//    audit_tail 给行号;篡改当场现形
// ---------------------------------------------------------------------------

#[test]
fn self_check_passes_on_honest_ledger_and_catches_tampering() {
    let path = tmp_wal("self-check");
    let mut gate = Wanning::open(&path).expect("开闸");
    gate.authorize(delegation("d1")).expect("注册");
    gate.decide("d1", request(100)).expect("判定");
    gate.decide("d1", request(9000)).expect("超额拒");

    // 诚实账本:三条独立口径(行数/链尾/状态指纹)全对上。
    let report = gate.self_check().expect("诚实账本自证通过");
    assert_eq!(report.wal_line_count, 3);
    assert_eq!(
        report.chain_tail,
        gate.chain_tail().expect("必有链尾"),
        "读侧独立重算链尾 == 实时链尾"
    );
    assert_eq!(report.state_hash, gate.state_hash());

    // 账被改(句柄还活着时,外部直接编辑磁盘上的审计):
    // 改中间行 memo——语义对账抓不住的那类(不参与判定)。
    // 本 crate 零 serde 依赖:对已知确定性子串替换(该行是 decide 记录,
    // intent.memo 是行内唯一的「嵌入测试」)。
    let mut lines = wanning_core::wal::raw_lines(&path).expect("读 WAL");
    assert!(
        lines[1].contains(r#""memo":"嵌入测试""#),
        "被改行必须是那笔 decide 记录: {}",
        lines[1]
    );
    lines[1] = lines[1].replace(r#""memo":"嵌入测试""#, r#""memo":"被改写的备注""#);
    std::fs::write(&path, lines.join("\n") + "\n").expect("重写 WAL");

    // 活句柄的自证当场拒发回执(fail-closed:不可信的自证比不自证更危险)。
    let err = gate.self_check().unwrap_err();
    assert!(
        matches!(err, CoreError::WalChainBroken { line: 3, .. }),
        "断链点 = 被改行的下一行,实际 {err}"
    );

    // 重启路径:带病审计在 open 就拒启,绝不接续。
    drop(gate);
    match Wanning::open(&path) {
        Err(CoreError::WalChainBroken { line: 3, .. }) => {}
        other => panic!("带病审计必须拒启,实际 {other:?}"),
    }
}

#[test]
fn audit_tail_returns_lines_with_numbers() {
    let path = tmp_wal("tail");
    let mut gate = Wanning::open(&path).expect("开闸");
    gate.authorize(delegation("d1")).expect("注册");
    gate.decide("d1", request(100)).expect("判定");
    gate.decide("d1", request(9000)).expect("超额拒");

    let tail = gate.audit_tail(2).expect("读审计尾");
    assert_eq!(tail.len(), 2);
    assert_eq!(tail[0].line_no, 2);
    assert_eq!(tail[1].line_no, 3);
    assert_eq!(tail[1].record.kind(), "decide", "最后一行是那笔超额拒绝");

    let all = gate.audit_tail(100).expect("读全量");
    assert_eq!(all.len(), 3, "请求超过现有行数时给全部,不报错");
    assert_eq!(all[0].line_no, 1);

    // 逐行链节(读侧独立重算):首行 prev = 创世值 0,末行 value = 链尾。
    let links = gate.chain_links().expect("读链节");
    assert_eq!(links.len(), 3);
    assert_eq!(links[0].prev, 0, "首行 prev = 创世值 0");
    assert_eq!(links[0].seq, 1);
    assert_eq!(
        links[2].value,
        gate.chain_tail().expect("必有链尾"),
        "末行链值 == 链尾"
    );

    // 只读访问器:注册过的委托读得到(上限/作用域),未注册的 None。
    let d = gate.delegation("d1").expect("委托在");
    assert_eq!(d.budget_cap_cents, 1000);
    assert_eq!(d.nonce_scope, "agent:x");
    assert!(gate.delegation("ghost").is_none());
}
