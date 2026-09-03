//! W-11 验收:支付宝 adapter(trait + 本地 mock,不碰真端点)。
//!
//! 覆盖:成功 / 失败 / 重复回调幂等 / 真路径被护栏挡。全部打本地 TcpListener,零外网。

use std::sync::Arc;

use wanning_demo::alipay::{
    apply_pay_notify, AlipayBackend, PayNotify, PayRequest, PayStatus, PaymentChannel,
    PaymentError, TradeState,
};
use wanning_demo::guard::EnvSnapshot;
use wanning_demo::http::{ApiTransport, HttpFailure};

mod common;
use common::{spawn_json_mock, MockJsonServer};

// 本地 mock 契约(wanning-demo 自定义,非支付宝报文;真实字段待 W-13 调研)。
fn mock_pay_body(status: &str) -> String {
    serde_json::json!({
        "trade_no": "ALI-TEST-20260902",
        "status": status,
        "amount_cents": 3990
    })
    .to_string()
}

fn pay_request() -> PayRequest {
    PayRequest {
        order_id: "JD-TEST-1".to_string(),
        amount_cents: 3990,
        delegation_id: "d1".to_string(),
        intent_nonce: 1,
    }
}

fn backend_on(mock: &MockJsonServer) -> AlipayBackend {
    AlipayBackend::new_mock(&mock.url(), Arc::new(wanning_demo::http::UreqApiTransport))
}

fn ledger() -> TradeState {
    TradeState {
        out_request_no: pay_request().out_request_no(),
        trade_no: "ALI-TEST-20260902".to_string(),
        amount_cents: 3990,
        status: PayStatus::Pending,
    }
}

fn success_notify() -> PayNotify {
    PayNotify {
        out_request_no: pay_request().out_request_no(),
        trade_no: "ALI-TEST-20260902".to_string(),
        status: PayStatus::Success,
        amount_cents: 3990,
    }
}

#[test]
fn trigger_pay_happy_path_carries_idempotency_key_and_audit_linkage() {
    let mock = spawn_json_mock(vec![(200, mock_pay_body("pending"))]);
    let mut backend = backend_on(&mock);

    let result = backend.trigger_pay(&pay_request()).expect("发起扣款成功");
    assert_eq!(result.status, PayStatus::Pending);
    assert_eq!(result.trade_no, "ALI-TEST-20260902");
    assert_eq!(result.amount_cents, 3990);
    assert_eq!(result.out_request_no, "w-d1-1-JD-TEST-1");

    let request = &mock.recorded_requests()[0];
    // 审计关联:委托 + nonce 必须随扣款发出;幂等键必须确定性派生。
    assert!(request.contains("out_request_no"), "{request}");
    assert!(request.contains("w-d1-1-JD-TEST-1"), "{request}");
    assert!(request.contains("\"delegation_id\":\"d1\""), "{request}");
    assert!(request.contains("\"intent_nonce\":1"), "{request}");
    assert!(
        request.contains("Bearer mock-access-token"),
        "mock 契约带 Bearer 头: {request}"
    );
}

#[test]
fn success_and_failed_statuses_both_come_back_as_results_not_errors() {
    // 业务终态(成功/失败)是**结果**,不是传输错误;失败由闸/对账层消费。
    let mock = spawn_json_mock(vec![(200, mock_pay_body("success"))]);
    let mut backend = backend_on(&mock);
    let result = backend.trigger_pay(&pay_request()).expect("成功态");
    assert_eq!(result.status, PayStatus::Success);

    let mock = spawn_json_mock(vec![(200, mock_pay_body("failed"))]);
    let mut backend = backend_on(&mock);
    let result = backend.trigger_pay(&pay_request()).expect("失败态也是结果");
    assert_eq!(result.status, PayStatus::Failed);
}

#[test]
fn transport_failures_map_to_http_and_timeout_errors() {
    let mock = spawn_json_mock(vec![(500, "{\"error\":\"mock\"}".to_string())]);
    let mut backend = backend_on(&mock);
    let err = backend.trigger_pay(&pay_request()).unwrap_err();
    assert!(
        matches!(err, PaymentError::Http { status: 500, .. }),
        "{err}"
    );

    #[derive(Debug)]
    struct TimingOutTransport;
    impl ApiTransport for TimingOutTransport {
        fn post_json(
            &self,
            _url: &str,
            _body: &str,
            _headers: &[(String, String)],
        ) -> Result<String, HttpFailure> {
            Err(HttpFailure {
                status: None,
                timeout: true,
                message: "mock 超时".to_string(),
            })
        }
    }
    let mut backend = AlipayBackend::new_mock("http://127.0.0.1:9", Arc::new(TimingOutTransport));
    let err = backend.trigger_pay(&pay_request()).unwrap_err();
    assert!(matches!(err, PaymentError::Timeout(_)), "{err}");
}

#[test]
fn repeat_trigger_reuses_same_idempotency_key_so_no_double_charge() {
    let mock = spawn_json_mock(vec![
        (200, mock_pay_body("pending")),
        (200, mock_pay_body("pending")),
    ]);
    let mut backend = backend_on(&mock);

    let first = backend.trigger_pay(&pay_request()).expect("第一次");
    let second = backend.trigger_pay(&pay_request()).expect("重试");

    // 同一 (委托, 意图, 订单) 重试:请求体逐字节相同 → 上游幂等,不会变成第二笔。
    let requests = mock.recorded_requests();
    assert_eq!(requests.len(), 2);
    let key = "out_request_no";
    assert!(requests[0].contains(key) && requests[1].contains(key));
    assert_eq!(first.out_request_no, second.out_request_no);
    assert_eq!(first.trade_no, second.trade_no);
}

#[test]
fn duplicate_callbacks_are_idempotent_no_ops() {
    let mut state = ledger();
    let notify = success_notify();

    // 第一次:推进 Pending → Success。
    assert!(matches!(apply_pay_notify(&mut state, &notify), Ok(true)));
    assert_eq!(state.status, PayStatus::Success);

    // 重复投递同一条通知(网络重发是常态):幂等 no-op,不重复入账。
    for _ in 0..3 {
        assert!(matches!(apply_pay_notify(&mut state, &notify), Ok(false)));
    }
    assert_eq!(state.status, PayStatus::Success);
    assert_eq!(state.amount_cents, 3990);
    assert_eq!(state.trade_no, "ALI-TEST-20260902");

    // Pending 通知重复投递同理。
    let mut state = ledger();
    let pending = PayNotify {
        status: PayStatus::Pending,
        ..success_notify()
    };
    assert!(matches!(apply_pay_notify(&mut state, &pending), Ok(false)));
}

#[test]
fn mismatched_or_regressing_callbacks_are_refused_without_touching_state() {
    let base = ledger();

    // ① 回调挂到别的交易上:拒。
    let mut state = base.clone();
    let stranger = PayNotify {
        out_request_no: "w-d9-9-OTHER".to_string(),
        ..success_notify()
    };
    assert!(matches!(
        apply_pay_notify(&mut state, &stranger),
        Err(PaymentError::BadResponse(_))
    ));

    // ② 金额不符:拒(钱!分毫不差)。
    let mut state = base.clone();
    let wrong_amount = PayNotify {
        amount_cents: 399,
        ..success_notify()
    };
    assert!(matches!(
        apply_pay_notify(&mut state, &wrong_amount),
        Err(PaymentError::BadResponse(_))
    ));

    // ③ 已成功又来「失败」通知:状态回退,拒。
    let mut state = base.clone();
    assert!(matches!(
        apply_pay_notify(&mut state, &success_notify()),
        Ok(true)
    ));
    let downgrade = PayNotify {
        status: PayStatus::Failed,
        ..success_notify()
    };
    assert!(matches!(
        apply_pay_notify(&mut state, &downgrade),
        Err(PaymentError::BadResponse(_))
    ));

    // ④ 已失败又来「成功」通知:拒,需人工核对。
    let mut state = base.clone();
    state.status = PayStatus::Failed;
    assert!(matches!(
        apply_pay_notify(&mut state, &success_notify()),
        Err(PaymentError::BadResponse(_))
    ));

    // ⑤ 成功态收到不同交易号:拒。
    let mut state = base.clone();
    assert!(matches!(
        apply_pay_notify(&mut state, &success_notify()),
        Ok(true)
    ));
    let other_trade = PayNotify {
        trade_no: "ALI-OTHER".to_string(),
        ..success_notify()
    };
    assert!(matches!(
        apply_pay_notify(&mut state, &other_trade),
        Err(PaymentError::BadResponse(_))
    ));

    // 每一种拒法之后,台账都必须还是初始 Pending 态(除 ③④⑤ 已按用例前置)。
    let state = base;
    assert_eq!(state.status, PayStatus::Pending);
}

#[test]
fn trigger_pay_rejects_bad_responses_instead_of_swallowing() {
    // 非 JSON。
    let mock = spawn_json_mock(vec![(200, "这不是 JSON".to_string())]);
    let mut backend = backend_on(&mock);
    assert!(matches!(
        backend.trigger_pay(&pay_request()),
        Err(PaymentError::BadResponse(_))
    ));

    // 空 trade_no。
    let mock = spawn_json_mock(vec![(
        200,
        "{\"trade_no\":\" \",\"status\":\"pending\",\"amount_cents\":3990}".to_string(),
    )]);
    let mut backend = backend_on(&mock);
    assert!(matches!(
        backend.trigger_pay(&pay_request()),
        Err(PaymentError::BadResponse(_))
    ));

    // 渠道回传金额与请求不符(钱!)。
    let mock = spawn_json_mock(vec![(
        200,
        "{\"trade_no\":\"T1\",\"status\":\"pending\",\"amount_cents\":3989}".to_string(),
    )]);
    let mut backend = backend_on(&mock);
    let err = backend.trigger_pay(&pay_request()).unwrap_err();
    assert!(matches!(err, PaymentError::BadResponse(_)), "{err}");
    assert!(err.to_string().contains("3989"), "{err}");
}

#[test]
fn invalid_requests_fail_closed_before_touching_network() {
    let mock = spawn_json_mock(vec![]); // 不应有任何请求
    let mut backend = backend_on(&mock);

    for mutate in [
        |r: &mut PayRequest| r.order_id = String::new(),
        |r: &mut PayRequest| r.order_id = "  ".to_string(),
        |r: &mut PayRequest| r.amount_cents = 0,
        |r: &mut PayRequest| r.delegation_id = String::new(),
        |r: &mut PayRequest| r.intent_nonce = 0,
    ] {
        let mut request = pay_request();
        mutate(&mut request);
        assert!(matches!(
            backend.trigger_pay(&request),
            Err(PaymentError::InvalidRequest(_))
        ));
    }
    assert!(mock.recorded_requests().is_empty(), "非法请求绝不能出网");
}

#[test]
fn notify_parse_refuses_reports_without_reconcilable_ids() {
    assert!(PayNotify::parse("这不是 JSON").is_err());
    let missing = serde_json::json!({"trade_no": "T1", "status": "success", "amount_cents": 1});
    assert!(PayNotify::parse(&missing.to_string()).is_err());
    let no_trade = serde_json::json!({
        "out_request_no": "w-d1-1-JD-TEST-1",
        "trade_no": "",
        "status": "success",
        "amount_cents": 1
    });
    assert!(PayNotify::parse(&no_trade.to_string()).is_err());

    let parsed = PayNotify::parse(
        &serde_json::json!({
            "out_request_no": "w-d1-1-JD-TEST-1",
            "trade_no": "T1",
            "status": "success",
            "amount_cents": 3990
        })
        .to_string(),
    )
    .expect("合法回调可解析");
    assert_eq!(parsed.status, PayStatus::Success);
}

#[test]
fn real_path_requires_guard_and_alipay_config() {
    // ① 全空 env:被护栏拦下。
    let err = AlipayBackend::from_snapshot_real(&EnvSnapshot::default()).unwrap_err();
    assert!(matches!(err, PaymentError::GuardBlocked(_)), "{err}");
    assert!(
        err.to_string().contains("WANNING_ALLOW_REAL_SPEND"),
        "{err}"
    );

    // ② 护栏 env 全齐但缺 app_id(W-50 起 fail-closed 链第二级)。
    let mut env = EnvSnapshot::default();
    env.insert("WANNING_ALLOW_REAL_SPEND", "1");
    env.insert("WANNING_GLM_KEY", "k");
    env.insert("WANNING_JD_APP_KEY", "k");
    env.insert("WANNING_JD_APP_SECRET", "k");
    env.insert("WANNING_JD_ACCESS_TOKEN", "k");
    let err = AlipayBackend::from_snapshot_real(&env).unwrap_err();
    assert!(matches!(err, PaymentError::Config(_)), "{err}");
    assert!(err.to_string().contains("WANNING_ALIPAY_APP_ID"), "{err}");

    // ③ 缺签约协议号:协议内扣款语义 = 没有协议号的扣款绝不发。
    env.insert("WANNING_ALIPAY_APP_ID", "2021000100000000");
    let err = AlipayBackend::from_snapshot_real(&env).unwrap_err();
    assert!(matches!(err, PaymentError::Config(_)), "{err}");
    assert!(
        err.to_string().contains("WANNING_ALIPAY_AGREEMENT_NO"),
        "{err}"
    );

    // ④ 全齐:网关默认官方地址(W-50 [公开文档直核];端点 env 可覆盖,见
    //    tests/alipay_real.rs 的覆盖断言)。真实扣款报文的端到端测试在
    //    tests/alipay_real.rs(官方响应向量 + 测试密钥对扮演两端)。
    env.insert("WANNING_ALIPAY_AGREEMENT_NO", "20170322450983769228");
    let backend = AlipayBackend::from_snapshot_real(&env).expect("全齐应构建成功");
    assert_eq!(backend.endpoint(), "https://openapi.alipay.com/gateway.do");
}

#[test]
fn debug_output_never_leaks_credentials() {
    let mock = spawn_json_mock(vec![]);
    let backend = backend_on(&mock);
    let debug = format!("{backend:?}");
    assert!(
        !debug.contains("mock-app-secret"),
        "secret 绝不能进 Debug: {debug}"
    );
    assert!(!debug.contains("mock-access-token"), "{debug}");
}
