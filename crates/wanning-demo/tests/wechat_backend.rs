//! W-27 验收:微信支付 adapter(委托代扣 papay,trait + 本地 mock,不碰真端点)。
//!
//! 覆盖:直核必填字段落报文(W-24 调研)/ 重试同幂等键 / PAP 回调校验与幂等平移 /
//! 真路径护栏+配置链 fail-closed。全部打本地 TcpListener,零外网。
//! 报文口径见 `src/wechat.rs` 模块文档:请求必填字段与「受理后异步扣款」语义是
//! W-24 `[直核]`,响应体/回调体结构是**本地 mock 契约**(真实字段未直核,绝不臆造)。

use std::sync::Arc;

use wanning_demo::channel::{
    apply_pay_notify, PayRequest, PayStatus, PaymentChannel, PaymentError, TradeState,
};
use wanning_demo::guard::EnvSnapshot;
use wanning_demo::http::{ApiTransport, HttpFailure};
use wanning_demo::wechat::{parse_papay_notify, WechatBackend};

mod common;
use common::{spawn_json_mock, MockJsonServer};

// 本地 mock 契约(wanning-demo 自定义,非微信报文;真实响应体字段未直核,W-24)。
fn mock_apply_body(trade_state: &str, total: u64) -> String {
    serde_json::json!({
        "out_trade_no": "w-d1-1-JD-TEST-1",
        "transaction_id": "WX-TEST-20260902",
        "trade_type": "PAP",
        "trade_state": trade_state,
        "amount": { "total": total, "currency": "CNY" }
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

fn backend_on(mock: &MockJsonServer) -> WechatBackend {
    WechatBackend::new_mock(&mock.url(), Arc::new(wanning_demo::http::UreqApiTransport))
}

fn ledger() -> TradeState {
    TradeState {
        out_request_no: pay_request().out_request_no(),
        trade_no: "WX-TEST-20260902".to_string(),
        amount_cents: 3990,
        status: PayStatus::Pending,
    }
}

fn papay_notify_raw(trade_state: &str, trade_type: &str, total: u64) -> String {
    serde_json::json!({
        "trade_type": trade_type,
        "out_trade_no": "w-d1-1-JD-TEST-1",
        "transaction_id": "WX-TEST-20260902",
        "trade_state": trade_state,
        "amount": { "total": total, "currency": "CNY" }
    })
    .to_string()
}

#[test]
fn trigger_pay_happy_path_carries_papay_required_fields_and_audit_linkage() {
    let mock = spawn_json_mock(vec![(200, mock_apply_body("pending", 3990))]);
    let mut backend = backend_on(&mock);

    let result = backend.trigger_pay(&pay_request()).expect("受理成功");
    assert_eq!(
        result.status,
        PayStatus::Pending,
        "受理 = 异步,常态是 pending"
    );
    assert_eq!(result.trade_no, "WX-TEST-20260902");
    assert_eq!(result.amount_cents, 3990);
    assert_eq!(result.out_request_no, "w-d1-1-JD-TEST-1");

    let request = &mock.recorded_requests()[0];
    // W-24 [直核] 的受理扣款必填字段一个不能少:
    // appid / out_trade_no / description / transaction_notify_url / contract_id /
    // amount{total(分,整数), currency(仅 CNY)}。
    for field in [
        "\"appid\"",
        "\"out_trade_no\"",
        "\"description\"",
        "\"transaction_notify_url\"",
        "\"contract_id\"",
        // amount 子对象直核口径:total 是整数分、currency 仅 CNY(serde_json 键序无关)。
        "\"amount\"",
        "\"total\":3990",
        "\"currency\":\"CNY\"",
    ] {
        assert!(request.contains(field), "缺必填字段 {field}: {request}");
    }
    // 审计关联(mock 契约附加):委托 + nonce 必须随扣款发出。
    assert!(request.contains("\"delegation_id\":\"d1\""), "{request}");
    assert!(request.contains("\"intent_nonce\":1"), "{request}");
    assert!(request.contains("Bearer mock-access-token"), "{request}");
}

#[test]
fn success_and_failed_states_come_back_as_results_not_errors() {
    // 业务终态(成功/失败)是**结果**,不是传输错误;失败由闸/对账层消费。
    let mock = spawn_json_mock(vec![(200, mock_apply_body("success", 3990))]);
    let mut backend = backend_on(&mock);
    let result = backend.trigger_pay(&pay_request()).expect("成功态");
    assert_eq!(result.status, PayStatus::Success);

    let mock = spawn_json_mock(vec![(200, mock_apply_body("failed", 3990))]);
    let mut backend = backend_on(&mock);
    let result = backend.trigger_pay(&pay_request()).expect("失败态也是结果");
    assert_eq!(result.status, PayStatus::Failed);
}

#[test]
fn transport_failures_map_to_http_and_timeout_errors() {
    let mock = spawn_json_mock(vec![(500, "{\"code\":\"SYSTEM_ERROR\"}".to_string())]);
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
    let mut backend = WechatBackend::new_mock("http://127.0.0.1:9", Arc::new(TimingOutTransport));
    let err = backend.trigger_pay(&pay_request()).unwrap_err();
    assert!(matches!(err, PaymentError::Timeout(_)), "{err}");
}

#[test]
fn retry_reuses_same_out_trade_no_until_success_or_period_ends() {
    // W-24 [直核]:「扣费失败可再次调用本接口发起重试扣费……直到扣费成功或者
    // 可扣费期结束」——重试用同一幂等键,上游幂等,不会变成第二笔扣款。
    let mock = spawn_json_mock(vec![
        (200, mock_apply_body("failed", 3990)),
        (200, mock_apply_body("pending", 3990)),
    ]);
    let mut backend = backend_on(&mock);

    let first = backend.trigger_pay(&pay_request()).expect("第一次");
    assert_eq!(first.status, PayStatus::Failed);
    let second = backend.trigger_pay(&pay_request()).expect("重试");
    assert_eq!(second.status, PayStatus::Pending);

    let requests = mock.recorded_requests();
    assert_eq!(requests.len(), 2);
    // 请求逐字节相同(确定性派生)→ 同一笔交易的重试,不是新交易。
    assert_eq!(requests[0], requests[1], "重试报文必须逐字节相同");
    assert!(requests[0].contains("\"out_trade_no\":\"w-d1-1-JD-TEST-1\""));
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

    // 空 transaction_id。
    let mock = spawn_json_mock(vec![(
        200,
        serde_json::json!({
            "out_trade_no": "w-d1-1-JD-TEST-1", "transaction_id": " ",
            "trade_type": "PAP", "trade_state": "pending",
            "amount": { "total": 3990, "currency": "CNY" }
        })
        .to_string(),
    )]);
    let mut backend = backend_on(&mock);
    assert!(matches!(
        backend.trigger_pay(&pay_request()),
        Err(PaymentError::BadResponse(_))
    ));

    // 渠道回传金额与请求不符(钱!分毫不差)。
    let mock = spawn_json_mock(vec![(200, mock_apply_body("pending", 3989))]);
    let mut backend = backend_on(&mock);
    let err = backend.trigger_pay(&pay_request()).unwrap_err();
    assert!(matches!(err, PaymentError::BadResponse(_)), "{err}");
    assert!(err.to_string().contains("3989"), "{err}");

    // 币种不是 CNY(W-24 [直核]:目前仅支持 CNY)。
    let mock = spawn_json_mock(vec![(
        200,
        serde_json::json!({
            "out_trade_no": "w-d1-1-JD-TEST-1", "transaction_id": "WX-1",
            "trade_type": "PAP", "trade_state": "pending",
            "amount": { "total": 3990, "currency": "USD" }
        })
        .to_string(),
    )]);
    let mut backend = backend_on(&mock);
    let err = backend.trigger_pay(&pay_request()).unwrap_err();
    assert!(matches!(err, PaymentError::BadResponse(_)), "{err}");
    assert!(err.to_string().contains("CNY"), "{err}");

    // 受理响应交易类型不是 PAP(挂错渠道):拒,同一纪律。
    let mock = spawn_json_mock(vec![(
        200,
        serde_json::json!({
            "out_trade_no": "w-d1-1-JD-TEST-1", "transaction_id": "WX-1",
            "trade_type": "JSAPI", "trade_state": "pending",
            "amount": { "total": 3990, "currency": "CNY" }
        })
        .to_string(),
    )]);
    let mut backend = backend_on(&mock);
    let err = backend.trigger_pay(&pay_request()).unwrap_err();
    assert!(matches!(err, PaymentError::BadResponse(_)), "{err}");
    assert!(err.to_string().contains("PAP"), "{err}");

    // 受理响应的 out_trade_no 与请求幂等键不符(对不上账):拒。
    let mock = spawn_json_mock(vec![(
        200,
        serde_json::json!({
            "out_trade_no": "w-d9-9-OTHER", "transaction_id": "WX-1",
            "trade_type": "PAP", "trade_state": "pending",
            "amount": { "total": 3990, "currency": "CNY" }
        })
        .to_string(),
    )]);
    let mut backend = backend_on(&mock);
    let err = backend.trigger_pay(&pay_request()).unwrap_err();
    assert!(matches!(err, PaymentError::BadResponse(_)), "{err}");
    assert!(err.to_string().contains("w-d9-9-OTHER"), "{err}");
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
fn callback_with_non_pap_trade_type_is_refused() {
    // W-24 [直核]:委托代扣的交易类型枚举 = PAP。不是 PAP 的回调 = 挂错渠道或伪造,
    // 一律拒(fail-closed),绝不进台账。
    for trade_type in ["JSAPI", "NATIVE", ""] {
        let err = parse_papay_notify(&papay_notify_raw("success", trade_type, 3990)).unwrap_err();
        assert!(matches!(err, PaymentError::BadResponse(_)), "{err}");
        assert!(err.to_string().contains("PAP"), "{err}");
    }

    // 缺 trade_type 字段同理。
    let missing = serde_json::json!({
        "out_trade_no": "w-d1-1-JD-TEST-1",
        "transaction_id": "WX-TEST-20260902",
        "trade_state": "success",
        "amount": { "total": 3990, "currency": "CNY" }
    });
    let err = parse_papay_notify(&missing.to_string()).unwrap_err();
    assert!(matches!(err, PaymentError::BadResponse(_)), "{err}");

    // 正确的 PAP 可解析,且映射到渠道无关的 PayNotify。
    let parsed = parse_papay_notify(&papay_notify_raw("success", "PAP", 3990)).expect("PAP 可解析");
    assert_eq!(parsed.status, PayStatus::Success);
    assert_eq!(parsed.out_request_no, "w-d1-1-JD-TEST-1");
    assert_eq!(parsed.trade_no, "WX-TEST-20260902");
    assert_eq!(parsed.amount_cents, 3990);
}

#[test]
fn callback_parse_requires_reconcilable_ids_and_cny_amount() {
    assert!(parse_papay_notify("这不是 JSON").is_err());

    for broken in [
        // 缺 out_trade_no → 无法对账。
        serde_json::json!({
            "transaction_id": "WX-1", "trade_type": "PAP", "trade_state": "success",
            "amount": { "total": 3990, "currency": "CNY" }
        }),
        // 缺 transaction_id → 无法对账。
        serde_json::json!({
            "out_trade_no": "w-d1-1-JD-TEST-1", "trade_type": "PAP",
            "trade_state": "success", "amount": { "total": 3990, "currency": "CNY" }
        }),
        // 缺金额 → 无法对账(钱!)。
        serde_json::json!({
            "out_trade_no": "w-d1-1-JD-TEST-1", "transaction_id": "WX-1",
            "trade_type": "PAP", "trade_state": "success"
        }),
        // 币种不是 CNY。
        serde_json::json!({
            "out_trade_no": "w-d1-1-JD-TEST-1", "transaction_id": "WX-1",
            "trade_type": "PAP", "trade_state": "success",
            "amount": { "total": 3990, "currency": "EUR" }
        }),
        // 未知的 trade_state(不臆造枚举)。
        serde_json::json!({
            "out_trade_no": "w-d1-1-JD-TEST-1", "transaction_id": "WX-1",
            "trade_type": "PAP", "trade_state": "SUPER_PAID",
            "amount": { "total": 3990, "currency": "CNY" }
        }),
    ] {
        let err = parse_papay_notify(&broken.to_string()).unwrap_err();
        assert!(matches!(err, PaymentError::BadResponse(_)), "{err}");
    }
}

#[test]
fn papay_callback_applies_idempotently_through_shared_ledger_semantics() {
    // 全链路:PAP 回调报文 → parse(渠道侧校验)→ apply_pay_notify(渠道无关幂等)。
    // W-24 结论:回调幂等语义从支付宝通道原样平移,渠道差异只在报文与端点。
    let mut state = ledger();
    let raw = papay_notify_raw("success", "PAP", 3990);

    // 第一次:推进 Pending → Success。
    let notify = parse_papay_notify(&raw).expect("PAP 回调可解析");
    assert!(matches!(apply_pay_notify(&mut state, &notify), Ok(true)));
    assert_eq!(state.status, PayStatus::Success);

    // 重复投递同一条通知(微信会重发):幂等 no-op,不重复入账。
    for _ in 0..3 {
        let notify = parse_papay_notify(&raw).expect("PAP 回调可解析");
        assert!(matches!(apply_pay_notify(&mut state, &notify), Ok(false)));
    }
    assert_eq!(state.status, PayStatus::Success);
    assert_eq!(state.amount_cents, 3990);
    assert_eq!(state.trade_no, "WX-TEST-20260902");

    // 金额不符的 PAP 回调:拒,不改台账(误扣对账线)。
    let mut state = ledger();
    let wrong_amount =
        parse_papay_notify(&papay_notify_raw("success", "PAP", 399)).expect("可解析");
    assert!(matches!(
        apply_pay_notify(&mut state, &wrong_amount),
        Err(PaymentError::BadResponse(_))
    ));
    assert_eq!(state.status, PayStatus::Pending, "拒绝应用,台账不动");

    // 挂错交易(别人的 out_trade_no):拒。
    let mut state = ledger();
    let stranger_raw = serde_json::json!({
        "trade_type": "PAP",
        "out_trade_no": "w-d9-9-OTHER",
        "transaction_id": "WX-TEST-20260902",
        "trade_state": "success",
        "amount": { "total": 3990, "currency": "CNY" }
    })
    .to_string();
    let stranger = parse_papay_notify(&stranger_raw).expect("可解析");
    assert!(matches!(
        apply_pay_notify(&mut state, &stranger),
        Err(PaymentError::BadResponse(_))
    ));
    assert_eq!(state.status, PayStatus::Pending, "拒绝应用,台账不动");

    // 状态回退(已成功又来「失败」):拒。
    let mut state = ledger();
    let notify = parse_papay_notify(&raw).expect("可解析");
    assert!(matches!(apply_pay_notify(&mut state, &notify), Ok(true)));
    let downgrade = parse_papay_notify(&papay_notify_raw("failed", "PAP", 3990)).expect("可解析");
    assert!(matches!(
        apply_pay_notify(&mut state, &downgrade),
        Err(PaymentError::BadResponse(_))
    ));
    assert_eq!(state.status, PayStatus::Success, "拒绝回退,台账不动");
}

#[test]
fn real_path_requires_guard_then_full_wechat_config_chain() {
    // ① 全空 env:被护栏拦下(真实消费总闸)。
    let err = WechatBackend::from_snapshot_real(&EnvSnapshot::default()).unwrap_err();
    assert!(matches!(err, PaymentError::GuardBlocked(_)), "{err}");
    assert!(
        err.to_string().contains("WANNING_ALLOW_REAL_SPEND"),
        "{err}"
    );

    // ② 护栏 env 全齐但端点未知(今晚真相):拒配,不臆造微信 URL。
    let mut env = EnvSnapshot::default();
    env.insert("WANNING_ALLOW_REAL_SPEND", "1");
    env.insert("WANNING_GLM_KEY", "k");
    env.insert("WANNING_JD_APP_KEY", "k");
    env.insert("WANNING_JD_APP_SECRET", "k");
    env.insert("WANNING_JD_ACCESS_TOKEN", "k");
    let err = WechatBackend::from_snapshot_real(&env).unwrap_err();
    assert!(matches!(err, PaymentError::Config(_)), "{err}");
    assert!(err.to_string().contains("WANNING_WECHAT_ENDPOINT"), "{err}");

    // ③ 有端点没 appid。
    env.insert("WANNING_WECHAT_ENDPOINT", "http://127.0.0.1:9");
    let err = WechatBackend::from_snapshot_real(&env).unwrap_err();
    assert!(matches!(err, PaymentError::Config(_)), "{err}");
    assert!(err.to_string().contains("WANNING_WECHAT_APPID"), "{err}");

    // ④ 有 appid 没签约协议:无 contract_id 绝不发起扣款(协议内扣款,不是裸转账)。
    env.insert("WANNING_WECHAT_APPID", "wx-mock-appid");
    let err = WechatBackend::from_snapshot_real(&env).unwrap_err();
    assert!(matches!(err, PaymentError::Config(_)), "{err}");
    assert!(
        err.to_string().contains("WANNING_WECHAT_CONTRACT_ID"),
        "{err}"
    );
    assert!(err.to_string().contains("协议"), "{err}");

    // ⑤ 有协议没回调地址。
    env.insert("WANNING_WECHAT_CONTRACT_ID", "mock-contract-id");
    let err = WechatBackend::from_snapshot_real(&env).unwrap_err();
    assert!(matches!(err, PaymentError::Config(_)), "{err}");
    assert!(
        err.to_string().contains("WANNING_WECHAT_NOTIFY_URL"),
        "{err}"
    );

    // ⑥ 全齐(端点指向本地 mock):完整真实形路径可用,且全程零外网。
    let mock = spawn_json_mock(vec![(200, mock_apply_body("success", 3990))]);
    env.insert("WANNING_WECHAT_ENDPOINT", &mock.url());
    env.insert("WANNING_WECHAT_NOTIFY_URL", "http://127.0.0.1:9/notify");
    let mut backend = WechatBackend::from_snapshot_real(&env).expect("全齐应构建成功");
    let result = backend.trigger_pay(&pay_request()).expect("扣款成功");
    assert_eq!(result.status, PayStatus::Success);
    assert_eq!(result.trade_no, "WX-TEST-20260902");
}

#[test]
fn debug_output_never_leaks_credentials_or_contract_id() {
    let mock = spawn_json_mock(vec![]);
    let backend = backend_on(&mock);
    let debug = format!("{backend:?}");
    assert!(
        !debug.contains("mock-app-secret"),
        "secret 绝不能进 Debug: {debug}"
    );
    assert!(!debug.contains("mock-access-token"), "{debug}");
    // contract_id 是用户授权凭证(拿住就能对用户扣款),同样绝不进 Debug。
    assert!(!debug.contains("mock-contract-id"), "{debug}");
}
