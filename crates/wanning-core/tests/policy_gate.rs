//! W-27 · 预算策略层测试(velocity / 类目预算 / 商户名单 / 禁止时段)。
//!
//! 语义口径(与 `gate.rs` / `policy.rs` 模块文档一致,测试逐条钉死):
//!
//! - 策略挂在**委托**上,随注册落审计(WAL 注册记录自带完整委托含策略,
//!   回放零新增记录类型);`SpendPolicy::default()` = 无附加策略,
//!   行为与本任务之前逐字节一致(四卖点场景回归锁定)。
//! - 阶段 5 内部顺序:**商户名单 → 禁止时段 → 速率 → 类目 → 总预算**(先到先拒);
//!   撤销与重放语义不变(仍先于一切策略检查)。
//! - 速率 = 滑动窗口整数秒;恰在窗口结束时刻,更早一笔不再计入(半开,口径与
//!   过期语义一致:阻塞在 `t + window` 时刻结束)。拒绝不计数、不耗号。
//! - 类目未知(不在表内)= 无类目预算,**fail-open**(总预算仍管)——任务书 W-27
//!   指定口径,理由落 master-plan 决策记录;空白类目按无类目处理。
//! - 类目上限 0 = 合法,语义为「禁该类目」(与总预算 0 拒收不同:类目 0 只关一个
//!   类目,是刻意的禁止表达)。
//! - 商户名单 deny 优先(同时在两份名单 = 拒);allow 空 = 不设白名单。
//! - 禁止时段 = `[from_ts, until_ts)` 绝对 Unix 秒,半开区间,无时区换算。
//!
//! 全程 MockClock 推时间,零 sleep;全部走公开 API(集成测试视角)。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use wanning_core::clock::{Clock, MockClock, SystemClock};
use wanning_core::delegation::Delegation;
use wanning_core::gate::{DenyReason, Gate, GateDecision};
use wanning_core::intent::SpendIntent;
use wanning_core::policy::{QuietWindow, SpendPolicy, VelocityLimit};
use wanning_core::state::WanningState;
use wanning_core::wal::WalDecision;

/// 临时 WAL 路径:纳秒 + pid + 进程内原子序号(W-21 教训:同 tick 撞名会抢同一把锁)。
fn tmp_wal(tag: &str) -> std::path::PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join("wanning-core-policy-tests");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间早于 Unix 纪元")
        .as_nanos();
    dir.join(format!(
        "{tag}-{}-{}-{}.jsonl",
        nanos,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

fn velocity_policy(max_spends: u32, window_secs: u64) -> SpendPolicy {
    SpendPolicy {
        velocity: Some(VelocityLimit {
            max_spends,
            window_secs,
        }),
        ..SpendPolicy::default()
    }
}

/// ¥10 总预算、有效期 [1000, 2000)、nonce 作用域 agent:claude-code 的样例闸。
fn gate_with(policy: SpendPolicy, now: u64) -> (Gate, MockClock) {
    let clock = MockClock::new(now);
    let mut gate = Gate::new(Arc::new(clock.clone()));
    gate.register_delegation(
        Delegation::new(
            "d1",
            "boss",
            "claude-code",
            1000,
            1000,
            2000,
            "agent:claude-code",
        )
        .with_policy(policy),
    )
    .expect("样例委托合法");
    (gate, clock)
}

fn intent(nonce: u64, amount_cents: u64) -> SpendIntent {
    SpendIntent::new("d1", nonce, amount_cents, "jd:shop-1", "grocery", "测试")
}

fn deny(reason: DenyReason) -> GateDecision {
    GateDecision::Deny { reason }
}

// ---------------------------------------------------------------------------
// 默认策略 = 行为不变(四卖点回归锁)
// ---------------------------------------------------------------------------

#[test]
fn default_policy_keeps_base_semantics() {
    let (mut gate, _clock) = gate_with(SpendPolicy::default(), 1500);
    assert!(gate.decide(&intent(1, 500)).is_allow());
    assert_eq!(
        gate.decide(&intent(2, 600)),
        deny(DenyReason::OverBudget),
        "总预算语义不受策略层影响"
    );
    assert_eq!(gate.decide(&intent(1, 100)), deny(DenyReason::Replay));
    // 无附加策略 ⇒ 策略运行时状态不产生任何数据。
    assert!(gate.velocity_stamps("d1").is_empty());
    assert_eq!(gate.category_spent_cents("d1", "grocery"), None);
}

// ---------------------------------------------------------------------------
// 速率限制(velocity)
// ---------------------------------------------------------------------------

#[test]
fn velocity_allows_up_to_limit_then_rate_limited_with_exact_window_boundary() {
    let (mut gate, clock) = gate_with(velocity_policy(2, 100), 1500);
    assert!(gate.decide(&intent(1, 100)).is_allow());
    assert!(gate.decide(&intent(2, 100)).is_allow());
    assert_eq!(
        gate.decide(&intent(3, 100)),
        deny(DenyReason::RateLimited),
        "窗口内第 3 笔必须被速率拒绝"
    );
    // 推到窗口内但更晚:更早两笔仍在窗口(1599-1500=99 < 100),仍拒。
    clock.set_now(1599);
    assert_eq!(gate.decide(&intent(4, 100)), deny(DenyReason::RateLimited));
    // 恰在窗口结束时刻(t + window = 1600):最早一笔不再计入,放行。
    clock.set_now(1600);
    assert!(
        gate.decide(&intent(4, 100)).is_allow(),
        "恰在 t+window 时刻,更早一笔已滑出窗口(半开口径,与过期语义一致)"
    );
    assert_eq!(gate.velocity_stamps("d1").len(), 3, "成功放行时刻全量保留");
}

#[test]
fn velocity_denied_intent_consumes_no_nonce_and_no_window_slot() {
    let (mut gate, clock) = gate_with(velocity_policy(1, 100), 1500);
    assert!(gate.decide(&intent(1, 100)).is_allow());
    assert_eq!(gate.decide(&intent(2, 100)), deny(DenyReason::RateLimited));
    // 同一 nonce 稍后重试(窗口已滑动)必须放行:拒绝既不耗号也不占窗口槽。
    clock.set_now(1600);
    assert!(
        gate.decide(&intent(2, 100)).is_allow(),
        "被速率拒绝的意图不耗 nonce、不占窗口槽"
    );
}

#[test]
fn velocity_counts_only_successful_spends() {
    let (mut gate, _clock) = gate_with(velocity_policy(2, 100), 1500);
    assert_eq!(
        gate.decide(&intent(1, 5000)),
        deny(DenyReason::OverBudget),
        "超额拒绝不该占速率槽"
    );
    assert_eq!(
        gate.decide(&intent(2, 0)),
        deny(DenyReason::InvalidAmount),
        "非法金额拒绝不该占速率槽"
    );
    assert!(gate.decide(&intent(3, 100)).is_allow());
    assert!(gate.decide(&intent(4, 100)).is_allow());
    assert_eq!(
        gate.decide(&intent(5, 100)),
        deny(DenyReason::RateLimited),
        "只有成功放行才计入速率窗口"
    );
    assert_eq!(gate.velocity_stamps("d1").len(), 2);
}

// ---------------------------------------------------------------------------
// 类目预算(fail-open 口径 + 0 上限 = 禁类目)
// ---------------------------------------------------------------------------

#[test]
fn category_budget_denies_over_category_while_total_has_room() {
    let policy = SpendPolicy {
        category_caps_cents: BTreeMap::from([("grocery".to_string(), 300)]),
        ..SpendPolicy::default()
    };
    let (mut gate, _clock) = gate_with(policy, 1500);
    assert!(gate.decide(&intent(1, 200)).is_allow());
    assert_eq!(
        gate.decide(&intent(2, 200)),
        deny(DenyReason::OverCategoryBudget),
        "类目超限但总预算还有余量,必须报类目原因"
    );
    assert!(
        gate.decide(&intent(3, 100)).is_allow(),
        "恰好补满类目上限应放行(200+100=300)"
    );
    assert_eq!(
        gate.decide(&intent(4, 1)),
        deny(DenyReason::OverCategoryBudget)
    );
    // 未设上限的类目 fail-open:只受总预算管。
    let toys = SpendIntent::new("d1", 5, 500, "jd:shop-1", "toys", "");
    assert!(gate.decide(&toys).is_allow());
    assert_eq!(
        gate.category_spent_cents("d1", "grocery"),
        Some(300),
        "设了上限的类目要记账"
    );
    assert_eq!(
        gate.category_spent_cents("d1", "toys"),
        None,
        "未设上限的类目不记账(无预算可言)"
    );
}

#[test]
fn category_unknown_or_blank_is_fail_open() {
    // 任务书 W-27 指定口径:类目未知 = 无类目预算,放行(总预算仍管)。
    // fail-open 理由落 master-plan 决策记录;此测试锁定该口径不被悄悄改掉。
    let policy = SpendPolicy {
        category_caps_cents: BTreeMap::from([("grocery".to_string(), 1)]),
        ..SpendPolicy::default()
    };
    let (mut gate, _clock) = gate_with(policy, 1500);
    let blank = SpendIntent::new("d1", 1, 500, "jd:shop-1", "", "");
    assert!(
        gate.decide(&blank).is_allow(),
        "空白类目 = 无类目,fail-open"
    );
    let unknown = SpendIntent::new("d1", 2, 500, "jd:shop-1", "toys", "");
    assert!(gate.decide(&unknown).is_allow(), "未知类目 fail-open");
    let capped = SpendIntent::new("d1", 3, 2, "jd:shop-1", "grocery", "");
    assert_eq!(
        gate.decide(&capped),
        deny(DenyReason::OverCategoryBudget),
        "设了上限的类目照常管"
    );
}

#[test]
fn category_cap_zero_forbids_the_category() {
    let policy = SpendPolicy {
        category_caps_cents: BTreeMap::from([("gifts".to_string(), 0)]),
        ..SpendPolicy::default()
    };
    let (mut gate, _clock) = gate_with(policy, 1500);
    assert_eq!(
        gate.decide(&SpendIntent::new("d1", 1, 1, "jd:shop-1", "gifts", "")),
        deny(DenyReason::OverCategoryBudget),
        "类目上限 0 = 禁该类目(与总预算 0 拒收口径不同,理由落决策记录)"
    );
    assert!(
        gate.decide(&SpendIntent::new("d1", 2, 100, "jd:shop-1", "toys", ""))
            .is_allow(),
        "其他类目不受影响"
    );
}

// ---------------------------------------------------------------------------
// 商户名单(deny 优先)
// ---------------------------------------------------------------------------

#[test]
fn merchant_deny_list_wins_over_allow_list() {
    let policy = SpendPolicy {
        merchant_allow: BTreeSet::from(["m1".to_string(), "m2".to_string()]),
        merchant_deny: BTreeSet::from(["m1".to_string()]),
        ..SpendPolicy::default()
    };
    let (mut gate, _clock) = gate_with(policy, 1500);
    let m1 = SpendIntent::new("d1", 1, 100, "m1", "x", "");
    assert_eq!(
        gate.decide(&m1),
        deny(DenyReason::MerchantDenied),
        "同时出现在两份名单:deny 优先"
    );
    let m2 = SpendIntent::new("d1", 2, 100, "m2", "x", "");
    assert!(gate.decide(&m2).is_allow());
}

#[test]
fn merchant_allow_list_gates_unlisted_merchants() {
    let policy = SpendPolicy {
        merchant_allow: BTreeSet::from(["m1".to_string()]),
        ..SpendPolicy::default()
    };
    let (mut gate, _clock) = gate_with(policy, 1500);
    let unlisted = SpendIntent::new("d1", 1, 100, "m2", "x", "");
    assert_eq!(
        gate.decide(&unlisted),
        deny(DenyReason::MerchantNotAllowed),
        "白名单非空时,未列商户必须拒"
    );
    assert!(gate
        .decide(&SpendIntent::new("d1", 2, 100, "m1", "x", ""))
        .is_allow());
}

#[test]
fn merchant_deny_list_alone_only_blocks_listed_merchants() {
    let policy = SpendPolicy {
        merchant_deny: BTreeSet::from(["m1".to_string()]),
        ..SpendPolicy::default()
    };
    let (mut gate, _clock) = gate_with(policy, 1500);
    assert_eq!(
        gate.decide(&SpendIntent::new("d1", 1, 100, "m1", "x", "")),
        deny(DenyReason::MerchantDenied)
    );
    assert!(
        gate.decide(&SpendIntent::new("d1", 2, 100, "m2", "x", ""))
            .is_allow(),
        "黑名单不拦未列商户(allow 空 = 不设白名单)"
    );
}

// ---------------------------------------------------------------------------
// 禁止时段(quiet hours,绝对 Unix 秒,半开)
// ---------------------------------------------------------------------------

#[test]
fn quiet_hours_half_open_absolute_window() {
    let policy = SpendPolicy {
        quiet_windows: vec![QuietWindow {
            from_ts: 1800,
            until_ts: 1900,
        }],
        ..SpendPolicy::default()
    };
    let (mut gate, clock) = gate_with(policy, 1500);
    assert!(gate.decide(&intent(1, 100)).is_allow(), "窗口前放行");
    clock.set_now(1800);
    assert_eq!(
        gate.decide(&intent(2, 100)),
        deny(DenyReason::QuietHours),
        "恰在 from_ts 进入禁止窗口(含端点)"
    );
    clock.set_now(1899);
    assert_eq!(gate.decide(&intent(3, 100)), deny(DenyReason::QuietHours));
    clock.set_now(1900);
    assert!(
        gate.decide(&intent(4, 100)).is_allow(),
        "恰在 until_ts 出窗口(半开,口径与有效期一致)"
    );
}

// ---------------------------------------------------------------------------
// 判定顺序:撤销/重放仍先于策略;策略内部先到先拒
// ---------------------------------------------------------------------------

#[test]
fn revoked_and_replay_precede_policy_checks() {
    // 撤销先于策略:已撤销委托即便商户在黑名单/速率已满,也必须报 revoked。
    let policy = SpendPolicy {
        velocity: Some(VelocityLimit {
            max_spends: 1,
            window_secs: 100,
        }),
        merchant_deny: BTreeSet::from(["jd:shop-1".to_string()]),
        ..SpendPolicy::default()
    };
    let (mut gate, _clock) = gate_with(policy, 1500);
    // 名单只列 jd:shop-1,先用未列商户放行一笔(速度窗口 1/1 随之落下)。
    assert!(
        gate.decide(&SpendIntent::new("d1", 1, 100, "other:shop", "x", ""))
            .is_allow(),
        "黑名单只拦列出的商户,未列商户照常放行"
    );
    gate.revoke("d1").expect("撤销");
    assert_eq!(
        gate.decide(&intent(2, 100)),
        deny(DenyReason::Revoked),
        "kill switch 语义不变:撤销先于一切策略检查(这笔意图同时踩中黑名单+速率)"
    );

    // 重放先于策略:窗口已满时重发已消费 nonce 必须报 replay(不是 rate_limited)。
    let (mut gate, _clock) = gate_with(velocity_policy(1, 100), 1500);
    assert!(gate.decide(&intent(1, 100)).is_allow());
    assert_eq!(
        gate.decide(&intent(1, 100)),
        deny(DenyReason::Replay),
        "重放语义不变:先于速率检查"
    );
}

#[test]
fn policy_checks_are_first_deny_wins_within_stage_five() {
    // 阶段 5 内部顺序:名单 → 时段 → 速率 → 类目 → 总预算(先到先拒)。
    // 手法:每一步构造「后面的门也必拒」的意图,验证报出来的永远是最前面的那道门。
    let policy = SpendPolicy {
        velocity: Some(VelocityLimit {
            max_spends: 1,
            window_secs: 1000, // 1300 的窗口时刻到 2300 才滑出(半开边界一并被踩)
        }),
        category_caps_cents: BTreeMap::from([("grocery".to_string(), 1)]),
        quiet_windows: vec![QuietWindow {
            from_ts: 1400,
            until_ts: 1600,
        }],
        ..SpendPolicy::default()
    };

    // ── 商户名单最前:禁止窗口 + 类目必超限,仍报 merchant_denied ─────────
    let deny_policy = SpendPolicy {
        merchant_deny: BTreeSet::from(["jd:shop-1".to_string()]),
        category_caps_cents: BTreeMap::from([("grocery".to_string(), 1)]),
        quiet_windows: vec![QuietWindow {
            from_ts: 1400,
            until_ts: 1600,
        }],
        ..SpendPolicy::default()
    };
    let (mut gate, _clock) = gate_with(deny_policy, 1500);
    assert_eq!(
        gate.decide(&intent(1, 100)),
        deny(DenyReason::MerchantDenied),
        "名单先于时段/类目:窗口内发黑名单商户,报 merchant_denied"
    );

    // ── 时段先于速率/类目/总预算 ─────────────────────────────────────────
    // 样例委托有效期 [1000, 2000) 不够推到速率窗口滑动,这里另建长有效期闸。
    let clock = MockClock::new(1300);
    let mut gate = Gate::new(Arc::new(clock.clone()));
    gate.register_delegation(
        Delegation::new(
            "d1",
            "boss",
            "claude-code",
            1000,
            1000,
            10_000,
            "agent:claude-code",
        )
        .with_policy(policy),
    )
    .expect("样例委托合法");

    // 窗口外第一笔 1 分:恰满类目上限(0+1=1)、速率记 1/1、总账 1/1000。
    assert!(
        gate.decide(&intent(1, 1)).is_allow(),
        "1300 在禁止窗口外,1 分恰满类目上限,放行"
    );
    clock.set_now(1500); // 进禁止窗口
                         // 同一笔意图同时踩中四道门:时段(窗口内)+ 速率(1/1)+ 类目(1+5000>1)+ 总预算(5001>1000)。
    assert_eq!(
        gate.decide(&intent(2, 5000)),
        deny(DenyReason::QuietHours),
        "先到先拒:时段在速率/类目/总预算之前"
    );
    clock.set_now(1600); // 出窗口:下一道是速率
    assert_eq!(
        gate.decide(&intent(2, 5000)),
        deny(DenyReason::RateLimited),
        "时段过后,速率先于类目/总预算"
    );
    clock.set_now(2300); // 1300 的窗口时刻恰在此刻滑出(2300-1300 = window,半开)
    assert_eq!(
        gate.decide(&intent(2, 2)),
        deny(DenyReason::OverCategoryBudget),
        "速率滑出后,类目先于总预算"
    );
    // 未设上限的类目 fail-open,落到总预算这道门。
    let toys = SpendIntent::new("d1", 3, 5000, "jd:shop-1", "toys", "");
    assert_eq!(
        gate.decide(&toys),
        deny(DenyReason::OverBudget),
        "未知类目落到总预算这道门"
    );
}

#[test]
fn evaluate_is_pure_under_policy() {
    let policy = SpendPolicy {
        velocity: Some(VelocityLimit {
            max_spends: 5,
            window_secs: 100,
        }),
        category_caps_cents: BTreeMap::from([("grocery".to_string(), 1000)]),
        ..SpendPolicy::default()
    };
    let (mut gate, _clock) = gate_with(policy, 1500);
    for _ in 0..3 {
        assert!(gate.evaluate(&intent(1, 100)).is_allow(), "evaluate 纯检查");
    }
    assert!(
        gate.velocity_stamps("d1").is_empty(),
        "evaluate 绝不写速率窗口"
    );
    assert_eq!(gate.category_spent_cents("d1", "grocery"), None);
    assert_eq!(gate.spent_cents("d1"), Some(0));
    // 落地后才产生策略状态。
    assert!(gate.decide(&intent(1, 100)).is_allow());
    assert_eq!(gate.velocity_stamps("d1"), &[1500]);
    assert_eq!(gate.category_spent_cents("d1", "grocery"), Some(100));
}

// ---------------------------------------------------------------------------
// 注册校验(fail-closed:坏策略拒收)
// ---------------------------------------------------------------------------

#[test]
fn policy_validation_rejects_bad_config() {
    let base = Delegation::new("d1", "boss", "agent", 1000, 1000, 2000, "s1");
    let cases: Vec<(SpendPolicy, &str)> = vec![
        (
            SpendPolicy {
                velocity: Some(VelocityLimit {
                    max_spends: 0,
                    window_secs: 100,
                }),
                ..SpendPolicy::default()
            },
            "max_spends=0",
        ),
        (
            SpendPolicy {
                velocity: Some(VelocityLimit {
                    max_spends: 1,
                    window_secs: 0,
                }),
                ..SpendPolicy::default()
            },
            "window_secs=0",
        ),
        (
            SpendPolicy {
                quiet_windows: vec![QuietWindow {
                    from_ts: 2000,
                    until_ts: 2000,
                }],
                ..SpendPolicy::default()
            },
            "零长禁止窗口",
        ),
        (
            SpendPolicy {
                quiet_windows: vec![QuietWindow {
                    from_ts: 2001,
                    until_ts: 2000,
                }],
                ..SpendPolicy::default()
            },
            "倒挂禁止窗口",
        ),
        (
            SpendPolicy {
                merchant_deny: BTreeSet::from([" ".to_string()]),
                ..SpendPolicy::default()
            },
            "空白商户条目",
        ),
        (
            SpendPolicy {
                category_caps_cents: BTreeMap::from([(" ".to_string(), 100)]),
                ..SpendPolicy::default()
            },
            "空白类目键",
        ),
    ];
    for (policy, why) in cases {
        let mut gate = Gate::new(Arc::new(MockClock::new(1500)));
        let err = gate
            .register_delegation(base.clone().with_policy(policy))
            .expect_err(&format!("应拒收:{why}"));
        assert!(
            matches!(err, wanning_core::error::CoreError::InvalidDelegation(_)),
            "{why} 应报 InvalidDelegation,实际 {err:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 序列化:默认策略零格式漂移;旧 WAL 记录可读
// ---------------------------------------------------------------------------

#[test]
fn delegation_serde_backward_compatible_and_policy_roundtrip() {
    // 旧格式(无 policy 字段,既有 WAL 的注册记录长这样)必须照常反序列化。
    let old_json = r#"{
        "id": "d1", "owner": "boss", "agent": "claude-code",
        "budget_cap_cents": 1000, "valid_from": 1000, "valid_until": 2000,
        "nonce_scope": "agent:claude-code"
    }"#;
    let old: Delegation = serde_json::from_str(old_json).expect("旧格式必须可读");
    assert_eq!(old.policy, SpendPolicy::default(), "缺字段 = 无附加策略");

    // 带策略的委托 roundtrip 保留策略。
    let policy = velocity_policy(3, 60);
    let d = Delegation::new("d1", "boss", "agent", 1000, 1000, 2000, "s1").with_policy(policy);
    let json = serde_json::to_string(&d).expect("序列化");
    let back: Delegation = serde_json::from_str(&json).expect("反序列化");
    assert_eq!(back, d);

    // 默认策略不落 policy 字段:既有 WAL 行逐字节不漂移(W-21 链格式稳定)。
    let plain = Delegation::new("d1", "boss", "agent", 1000, 1000, 2000, "s1");
    let plain_json = serde_json::to_string(&plain).expect("序列化");
    assert!(
        !plain_json.contains("policy"),
        "默认策略不得给 WAL 记录引入新字段: {plain_json}"
    );
}

// ---------------------------------------------------------------------------
// 回放与续跑:策略运行时状态必须能从 WAL 重建
// ---------------------------------------------------------------------------

#[test]
fn policy_state_survives_replay_deterministically() {
    let path = tmp_wal("replay-policy");
    let policy = SpendPolicy {
        velocity: Some(VelocityLimit {
            max_spends: 5,
            window_secs: 1000,
        }),
        category_caps_cents: BTreeMap::from([("grocery".to_string(), 500)]),
        ..SpendPolicy::default()
    };
    let clock = MockClock::new(1500);
    let mut state = WanningState::with_wal(Arc::new(clock.clone()), &path).expect("开 WAL");
    state
        .register_delegation(
            Delegation::new(
                "d1",
                "boss",
                "claude-code",
                1000,
                1000,
                2000,
                "agent:claude-code",
            )
            .with_policy(policy),
        )
        .expect("注册");
    assert!(state.decide(&intent(1, 200)).expect("判定").is_allow());
    assert_eq!(
        state.decide(&intent(2, 400)).expect("判定").deny_reason(),
        Some(DenyReason::OverCategoryBudget),
        "200+400 > 类目上限 500"
    );
    assert!(state.decide(&intent(3, 100)).expect("判定").is_allow());

    let live_hash = state.state_hash();
    let replayed = WanningState::replay(&path).expect("回放");
    let replayed_again = WanningState::replay(&path).expect("回放二遍");
    assert_eq!(
        replayed.state_hash(),
        replayed_again.state_hash(),
        "回放确定性"
    );
    assert_eq!(
        replayed.state_hash(),
        live_hash,
        "实时态与回放态必须逐位一致(含策略运行时状态)"
    );
    assert_eq!(
        replayed.gate().velocity_stamps("d1"),
        state.gate().velocity_stamps("d1"),
        "速率窗口时刻必须从 WAL 重建"
    );
    assert_eq!(
        replayed.gate().category_spent_cents("d1", "grocery"),
        Some(300),
        "类目台账必须从 WAL 重建"
    );
}

#[test]
fn live_resuming_keeps_velocity_window_across_restart() {
    let path = tmp_wal("resume-velocity");
    let real_now = SystemClock.now();
    let clock = MockClock::new(real_now - 10);
    let mut state = WanningState::with_wal(Arc::new(clock.clone()), &path).expect("开 WAL");
    state
        .register_delegation(
            Delegation::new(
                "d1",
                "boss",
                "claude-code",
                1000,
                1000,
                real_now.checked_add(86_400).expect("有效期溢出"),
                "agent:claude-code",
            )
            .with_policy(velocity_policy(1, 3600)),
        )
        .expect("注册");
    assert!(
        state.decide(&intent(1, 100)).expect("判定").is_allow(),
        "重启前放行一笔(时刻 real_now-10)"
    );
    drop(state); // 进程「重启」

    let mut resumed = WanningState::live_resuming(&path).expect("续跑");
    assert_eq!(
        resumed.decide(&intent(2, 100)).expect("判定").deny_reason(),
        Some(DenyReason::RateLimited),
        "速率窗口跨重启存活:回放必须重建成功放行时刻,否则重启洗掉限速"
    );
}

#[test]
fn policy_deny_is_recorded_in_wal_with_reason() {
    let path = tmp_wal("wal-reason");
    let clock = MockClock::new(1500);
    let mut state = WanningState::with_wal(Arc::new(clock.clone()), &path).expect("开 WAL");
    state
        .register_delegation(
            Delegation::new(
                "d1",
                "boss",
                "claude-code",
                1000,
                1000,
                2000,
                "agent:claude-code",
            )
            .with_policy(velocity_policy(1, 100)),
        )
        .expect("注册");
    assert!(state.decide(&intent(1, 100)).expect("判定").is_allow());
    assert_eq!(
        state.decide(&intent(2, 100)).expect("判定").deny_reason(),
        Some(DenyReason::RateLimited)
    );

    let records = wanning_core::wal::read_records(&path).expect("读回");
    let last = &records[records.len() - 1].1;
    match last.kind() {
        "decide" => {}
        other => panic!("最后一条应是决策记录,实际 {other}"),
    }
    let wanning_core::wal::WalRecord::Decide {
        decision, reason, ..
    } = last
    else {
        panic!("应是 Decide 记录");
    };
    assert_eq!(*decision, WalDecision::Deny);
    assert_eq!(
        *reason,
        Some(DenyReason::RateLimited),
        "策略拒绝必须带原因落审计"
    );
}
