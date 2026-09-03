//! W-08 验收:决策回路(离线脚本全流程 + GLM 对本地 mock server)。
//!
//! GLM 的 HTTP 层打**本地 mock server**(std TcpListener,127.0.0.1:0),
//! 全程不碰智谱真端点;真调路径另由 W-07 护栏测试挡住。

use std::net::TcpListener;
use std::sync::Arc;

use wanning_core::clock::MockClock;
use wanning_core::delegation::Delegation;
use wanning_core::error::CoreError;
use wanning_core::gate::DenyReason;
use wanning_core::state::WanningState;

use wanning_demo::decision::{
    run_decision_loop, DecisionContext, DecisionError, DecisionSource, GlmSource, LoopConfig,
    LoopError, LoopReport, ScriptedSource, StepEvent, UreqTransport, GLM_MAX_ATTEMPTS,
};
use wanning_demo::guard::EnvSnapshot;

mod common;
use common::spawn_json_mock;
use common::MockJsonServer;

// ---------------------------------------------------------------------------
// 通用夹具
// ---------------------------------------------------------------------------

fn fresh_wal(tag: &str) -> std::path::PathBuf {
    // 名字带进程内原子序号:用例并行起跑,只靠「纳秒+pid」可能同 tick 撞名,
    // 两个用例抢同一把单写者锁,输的一方 WalLocked 起不来(W-21 顺带修)。
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join("wanning-decision-tests");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间早于 Unix 纪元")
        .as_nanos();
    dir.join(format!(
        "{tag}-{nanos}-{}-{}.jsonl",
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::process::id()
    ))
}

fn state_with_delegation(tag: &str) -> WanningState {
    let mut state = WanningState::with_wal(Arc::new(MockClock::new(1_700_000_000)), fresh_wal(tag))
        .expect("开 WAL");
    state
        .register_delegation(Delegation::new(
            "d1",
            "所有者",
            "claude-code",
            1_000,
            1_700_000_000,
            1_700_003_600,
            "agent:claude-code",
        ))
        .expect("注册");
    state
}

fn ctx_for(next_nonce: u64, cap: u64, spent: u64) -> DecisionContext {
    DecisionContext {
        delegation_id: "d1".to_string(),
        budget_cap_cents: cap,
        spent_cents: spent,
        remaining_cents: cap - spent,
        next_nonce,
        step_index: 0,
        last_outcome: None,
    }
}

// ---------------------------------------------------------------------------
// ScriptedSource:离线脚本场景全流程
// ---------------------------------------------------------------------------

#[test]
fn scripted_four_selling_points_flow_through_the_loop() {
    let mut state = state_with_delegation("scripted-flow");
    let mut source = ScriptedSource::selling_points_script("d1");
    let config = LoopConfig {
        delegation_id: "d1".to_string(),
        max_steps: 8,
        revoke_after_n_intents: Some(2),
    };

    let LoopReport {
        source_name,
        events,
        exhausted_naturally,
    } = run_decision_loop(&mut state, &mut source, &config).expect("回路跑通");

    // 输出必须标注「离线脚本场景」——不是模型在决策。
    assert!(
        source_name.contains("离线脚本场景"),
        "来源名必须标注离线脚本: {source_name}"
    );
    assert!(exhausted_naturally, "三笔脚本应自然走完,而不是被上限截断");

    let mut iter = events.iter();
    // ① 预算内放行:¥5.00,nonce=1,WAL 行 2
    match iter.next().expect("第一笔") {
        StepEvent::Spend {
            intent,
            decision,
            wal_line,
        } => {
            assert!(decision.is_allow(), "①必须放行");
            assert_eq!(intent.amount_cents, 500);
            assert_eq!(intent.nonce, 1, "nonce 由闸侧注入");
            assert_eq!(*wal_line, 2);
        }
        other => panic!("①应是 Spend: {other:?}"),
    }
    // ② 超额拒:¥9.00,nonce=2(拒绝不消耗,下一笔仍拿 3)
    match iter.next().expect("第二笔") {
        StepEvent::Spend {
            intent,
            decision,
            wal_line,
        } => {
            assert_eq!(decision.deny_reason(), Some(DenyReason::OverBudget));
            assert_eq!(intent.amount_cents, 900);
            assert_eq!(intent.nonce, 2);
            assert_eq!(*wal_line, 3);
        }
        other => panic!("②应是 Spend: {other:?}"),
    }
    // 所有者收权(kill switch),WAL 行 4
    match iter.next().expect("撤销") {
        StepEvent::BossRevoke {
            delegation_id,
            wal_line,
        } => {
            assert_eq!(delegation_id, "d1");
            assert_eq!(*wal_line, 4);
        }
        other => panic!("应是 BossRevoke: {other:?}"),
    }
    // ③ 撤销后再请求:拒绝,nonce=3,WAL 行 5
    match iter.next().expect("第三笔") {
        StepEvent::Spend {
            intent,
            decision,
            wal_line,
        } => {
            assert_eq!(decision.deny_reason(), Some(DenyReason::Revoked));
            assert_eq!(intent.amount_cents, 100);
            assert_eq!(intent.nonce, 3);
            assert_eq!(*wal_line, 5);
        }
        other => panic!("③应是 Spend: {other:?}"),
    }
    assert!(iter.next().is_none(), "脚本走完不应有多余事件");

    assert_eq!(state.gate().spent_cents("d1"), Some(500), "只有①真的扣了账");
    assert!(state.gate().is_revoked("d1"));
}

#[test]
fn loop_refuses_to_run_without_audit_log() {
    // 无 WAL 的决策回路不该存在:每笔决策必须落审计。
    let mut state = WanningState::new(Arc::new(MockClock::new(0)));
    let mut source = ScriptedSource::new("离线脚本场景", vec![]);
    let config = LoopConfig {
        delegation_id: "d1".to_string(),
        max_steps: 3,
        revoke_after_n_intents: None,
    };
    let err = run_decision_loop(&mut state, &mut source, &config).unwrap_err();
    assert!(matches!(err, LoopError::Core(CoreError::WalIo(_))), "{err}");
}

#[test]
fn loop_refuses_unknown_delegation() {
    let mut state = state_with_delegation("unknown-delegation");
    let mut source = ScriptedSource::new("离线脚本场景", vec![]);
    let config = LoopConfig {
        delegation_id: "no-such".to_string(),
        max_steps: 3,
        revoke_after_n_intents: None,
    };
    let err = run_decision_loop(&mut state, &mut source, &config).unwrap_err();
    assert!(
        matches!(err, LoopError::Core(CoreError::UnknownDelegation(_))),
        "{err}"
    );
}

// ---------------------------------------------------------------------------
// GLM 本地 mock server(共用 tests/common,零外网)
// ---------------------------------------------------------------------------

/// 智谱响应形状(choices[0].message.content)。
fn chat_response(content: &str) -> String {
    serde_json::json!({
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }]
    })
    .to_string()
}

/// 模型该输出的意图 JSON。
fn intent_json(amount_cents: u64, merchant_id: &str) -> String {
    serde_json::json!({
        "amount_cents": amount_cents,
        "merchant_id": merchant_id,
        "category": "grocery",
        "memo": "本地 mock"
    })
    .to_string()
}

fn glm_against(mock: &MockJsonServer) -> GlmSource {
    GlmSource::with_parts(
        "test-key",
        &mock.url(),
        "glm-test-model",
        Arc::new(UreqTransport),
    )
}

#[test]
fn glm_source_parses_intent_from_local_mock_server() {
    let mock = spawn_json_mock(vec![(
        200,
        chat_response(&intent_json(500, "jd:mock-shop")),
    )]);
    let mut source = glm_against(&mock);

    let intent = source.next_intent(&ctx_for(1, 1_000, 0)).expect("解析成功");
    assert_eq!(intent.amount_cents, 500);
    assert_eq!(intent.merchant_id, "jd:mock-shop");
    // 越权字段以闸侧为准:delegation_id 与 nonce 都来自 ctx。
    assert_eq!(intent.delegation_id, "d1");
    assert_eq!(intent.nonce, 1);

    let requests = mock.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(request.contains("POST /chat/completions"), "{request}");
    assert!(
        request.contains("Authorization: Bearer test-key"),
        "必须带 Bearer 认证: {request}"
    );
    assert!(
        request.contains("\"model\":\"glm-test-model\""),
        "必须带模型名: {request}"
    );
    assert!(
        request.contains("剩余 1000 分"),
        "prompt 必须带闸的预算状态: {request}"
    );
}

#[test]
fn glm_source_ignores_model_supplied_delegation_and_nonce() {
    // 模型试图指定别的委托/别的 nonce:闸侧字段一律覆盖,越权无效。
    let hostile = serde_json::json!({
        "amount_cents": 300,
        "merchant_id": "jd:x",
        "delegation_id": "d-other",
        "nonce": 99
    })
    .to_string();
    let mock = spawn_json_mock(vec![(200, chat_response(&hostile))]);
    let mut source = glm_against(&mock);

    let intent = source.next_intent(&ctx_for(4, 1_000, 0)).expect("解析成功");
    assert_eq!(intent.delegation_id, "d1", "委托必须以闸侧为准");
    assert_eq!(intent.nonce, 4, "nonce 必须以闸侧为准");
    assert_eq!(intent.amount_cents, 300);
}

#[test]
fn glm_source_retries_once_then_succeeds() {
    // 第一次给非 JSON,第二次给合法意图:恰好 2 次尝试。
    let mock = spawn_json_mock(vec![
        (200, chat_response("抱歉,我不能输出 JSON")),
        (200, chat_response(&intent_json(200, "jd:mock-shop"))),
    ]);
    let mut source = glm_against(&mock);

    let intent = source
        .next_intent(&ctx_for(1, 1_000, 0))
        .expect("第二次应成功");
    assert_eq!(intent.amount_cents, 200);
    assert_eq!(mock.recorded_requests().len(), 2);
}

#[test]
fn glm_source_fails_after_retry_without_fabricating() {
    // 两次都不可用:报错并拒绝编造,恰好尝试 GLM_MAX_ATTEMPTS 次。
    let mock = spawn_json_mock(vec![
        (200, chat_response("不是 JSON 的回复")),
        (200, chat_response(&intent_json(0, "jd:mock-shop"))),
    ]);
    let mut source = glm_against(&mock);

    let err = source
        .next_intent(&ctx_for(1, 1_000, 0))
        .expect_err("两次失败必须报错");
    assert!(
        matches!(err, DecisionError::Source(_)),
        "来源故障,不是 Exhausted: {err}"
    );
    assert!(err.to_string().contains("拒绝编造"), "{err}");
    assert!(
        err.to_string()
            .contains(&format!("{GLM_MAX_ATTEMPTS} 次尝试")),
        "{err}"
    );
    assert_eq!(
        mock.recorded_requests().len(),
        GLM_MAX_ATTEMPTS as usize,
        "恰好尝试 2 次,不多不少"
    );
}

#[test]
fn glm_source_reports_http_500_and_retries() {
    let mock = spawn_json_mock(vec![
        (500, chat_response(&intent_json(100, "jd:mock-shop"))),
        (500, chat_response(&intent_json(100, "jd:mock-shop"))),
    ]);
    let mut source = glm_against(&mock);

    let err = source
        .next_intent(&ctx_for(1, 1_000, 0))
        .expect_err("500 必须报错");
    assert!(err.to_string().contains("HTTP 500"), "{err}");
    assert_eq!(mock.recorded_requests().len(), 2);
}

#[test]
fn glm_source_fails_closed_when_no_server_listens() {
    // 端口上没有任何服务:连接层故障 → 重试后报错(不编造)。
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("地址");
    drop(listener); // 立刻关掉,连接必被拒

    let mut source = GlmSource::with_parts(
        "test-key",
        &format!("http://{addr}"),
        "glm-test-model",
        Arc::new(UreqTransport),
    );
    let err = source
        .next_intent(&ctx_for(1, 1_000, 0))
        .expect_err("连接失败必须报错");
    assert!(err.to_string().contains("拒绝编造"), "{err}");
}

#[test]
fn glm_source_refuses_to_build_without_key() {
    let env = EnvSnapshot::default();
    let err = GlmSource::from_snapshot(&env).expect_err("无密钥必须拒绝构建");
    assert!(err.to_string().contains("WANNING_GLM_KEY"), "{err}");
    assert!(err.to_string().contains("fail-closed"), "{err}");
}

#[test]
fn glm_source_builds_from_snapshot_with_overrides() {
    let mut env = EnvSnapshot::default();
    env.insert("WANNING_GLM_KEY", " k ");
    env.insert("WANNING_GLM_BASE_URL", "http://127.0.0.1:9/");
    env.insert("WANNING_GLM_MODEL", "glm-x");
    let source = GlmSource::from_snapshot(&env).expect("有密钥即可构建");
    assert_eq!(source.endpoint(), "http://127.0.0.1:9/chat/completions");
}
