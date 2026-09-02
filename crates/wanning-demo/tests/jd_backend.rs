//! W-10 验收:京东 adapter(trait + 本地 mock,不碰真端点)。

use std::sync::Arc;

use wanning_demo::guard::EnvSnapshot;
use wanning_demo::jd::{
    BackendError, CommerceBackend, CreateOrderRequest, HttpFailure, JdBackend, JdTransport,
    SearchRequest, UreqJdTransport,
};

mod common;
use common::{spawn_json_mock, MockJsonServer};

// 本地 mock 契约(wanning-demo 自定义,非京东报文;真实字段待 W-12 调研)。
fn mock_search_body() -> String {
    serde_json::json!({
        "products": [
            {"sku_id": "1001", "title": "矿泉水 24 瓶", "price_cents": 3990, "merchant_id": "jd:shop-1"},
            {"sku_id": "1002", "title": "抽纸 30 包", "price_cents": 5990, "merchant_id": "jd:shop-2"}
        ]
    })
    .to_string()
}

fn mock_order_body() -> String {
    serde_json::json!({"order_id": "JD-TEST-1", "sku_id": "1001", "amount_cents": 3990}).to_string()
}

fn search_request() -> SearchRequest {
    SearchRequest {
        keyword: "矿泉水".to_string(),
        max_price_cents: Some(10_000),
        limit: 5,
    }
}

fn order_request() -> CreateOrderRequest {
    CreateOrderRequest {
        sku_id: "1001".to_string(),
        quantity: 1,
        amount_cents: 3990,
        delegation_id: "d1".to_string(),
        intent_nonce: 1,
    }
}

fn backend_on(mock: &MockJsonServer) -> JdBackend {
    JdBackend::new_mock(&mock.url(), Arc::new(UreqJdTransport))
}

#[test]
fn search_happy_path_parses_products_from_local_mock() {
    let mock = spawn_json_mock(vec![(200, mock_search_body())]);
    let mut backend = backend_on(&mock);

    let products = backend.search(&search_request()).expect("search 成功");
    assert_eq!(products.len(), 2);
    assert_eq!(products[0].sku_id, "1001");
    assert_eq!(products[0].price_cents, 3990);
    assert_eq!(products[1].merchant_id, "jd:shop-2");

    let request = &mock.recorded_requests()[0];
    assert!(request.contains("矿泉水"), "关键词要进请求体: {request}");
    assert!(
        request.contains("Bearer mock-access-token"),
        "mock 契约带 Bearer 头: {request}"
    );
}

#[test]
fn create_order_happy_path_returns_order_ref_and_carries_audit_linkage() {
    let mock = spawn_json_mock(vec![(200, mock_order_body())]);
    let mut backend = backend_on(&mock);

    let order = backend.create_order(&order_request()).expect("下单成功");
    assert_eq!(order.order_id, "JD-TEST-1");
    assert_eq!(order.amount_cents, 3990);

    let request = &mock.recorded_requests()[0];
    // 订单必须能回溯到授权:委托 id 与意图 nonce 都要进请求。
    assert!(request.contains("delegation_id"), "{request}");
    assert!(request.contains("d1"), "{request}");
    assert!(request.contains("intent_nonce"), "{request}");
    assert!(request.contains("3990"), "{request}");
}

#[test]
fn search_and_order_report_http_errors() {
    for status in [400u16, 500] {
        let mock = spawn_json_mock(vec![(status, "{\"error\":\"mock\"}".to_string())]);
        let mut backend = backend_on(&mock);
        let err = backend.search(&search_request()).unwrap_err();
        assert!(
            matches!(err, BackendError::Http { status: s, .. } if s == status),
            "{err}"
        );
        assert!(err.to_string().contains(&status.to_string()), "{err}");
    }
}

#[test]
fn bad_response_is_reported_not_swallowed() {
    // 2xx 但响应不符合契约:fail-closed 报 BadResponse,不静默返回空列表。
    let mock = spawn_json_mock(vec![(200, "这不是 JSON".to_string())]);
    let mut backend = backend_on(&mock);
    let err = backend.search(&search_request()).unwrap_err();
    assert!(matches!(err, BackendError::BadResponse(_)), "{err}");

    // 200 + 合法 JSON 但缺字段:同样报错。
    let mock = spawn_json_mock(vec![(
        200,
        "{\"products\":[{\"sku_id\":\"1\"}]}".to_string(),
    )]);
    let mut backend = backend_on(&mock);
    let err = backend.search(&search_request()).unwrap_err();
    assert!(matches!(err, BackendError::BadResponse(_)), "{err}");
}

#[test]
fn timeout_is_reported_as_timeout() {
    #[derive(Debug)]
    struct TimingOutTransport;
    impl JdTransport for TimingOutTransport {
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

    let mut backend = JdBackend::new_mock("http://127.0.0.1:9", Arc::new(TimingOutTransport));
    let err = backend.search(&search_request()).unwrap_err();
    assert!(matches!(err, BackendError::Timeout(_)), "{err}");
    assert!(err.to_string().contains("超时"), "{err}");
}

#[test]
fn invalid_requests_fail_closed_before_touching_network() {
    let mock = spawn_json_mock(vec![]); // 不应有任何请求
    let mut backend = backend_on(&mock);

    let mut bad = search_request();
    bad.keyword = "  ".to_string();
    assert!(matches!(
        backend.search(&bad),
        Err(BackendError::InvalidRequest(_))
    ));
    let mut bad = search_request();
    bad.limit = 0;
    assert!(matches!(
        backend.search(&bad),
        Err(BackendError::InvalidRequest(_))
    ));

    for mutate in [
        |r: &mut CreateOrderRequest| r.sku_id = String::new(),
        |r: &mut CreateOrderRequest| r.quantity = 0,
        |r: &mut CreateOrderRequest| r.amount_cents = 0,
        |r: &mut CreateOrderRequest| r.delegation_id = " ".to_string(),
        |r: &mut CreateOrderRequest| r.intent_nonce = 0,
    ] {
        let mut request = order_request();
        mutate(&mut request);
        assert!(matches!(
            backend.create_order(&request),
            Err(BackendError::InvalidRequest(_))
        ));
    }
    assert!(mock.recorded_requests().is_empty(), "非法请求绝不能出网");
}

#[test]
fn real_path_requires_guard_and_endpoint() {
    // ① 全空 env:被护栏拦下。
    let err = JdBackend::from_snapshot_real(&EnvSnapshot::default()).unwrap_err();
    assert!(matches!(err, BackendError::GuardBlocked(_)), "{err}");
    assert!(
        err.to_string().contains("WANNING_ALLOW_REAL_SPEND"),
        "{err}"
    );

    // ② 护栏 env 全齐但端点未知(今晚真相):拒配,不臆造京东 URL。
    let mut env = EnvSnapshot::default();
    env.insert("WANNING_ALLOW_REAL_SPEND", "1");
    env.insert("WANNING_GLM_KEY", "k");
    env.insert("WANNING_JD_APP_KEY", "k");
    env.insert("WANNING_JD_APP_SECRET", "k");
    env.insert("WANNING_JD_ACCESS_TOKEN", "k");
    let err = JdBackend::from_snapshot_real(&env).unwrap_err();
    assert!(matches!(err, BackendError::Config(_)), "{err}");
    assert!(err.to_string().contains("WANNING_JD_ENDPOINT"), "{err}");

    // ③ 全齐(端点指向本地 mock):完整真实形路径可用,且全程零外网。
    let mock = spawn_json_mock(vec![(200, mock_search_body())]);
    env.insert("WANNING_JD_ENDPOINT", &mock.url());
    let mut backend = JdBackend::from_snapshot_real(&env).expect("全齐应构建成功");
    let products = backend.search(&search_request()).expect("search 成功");
    assert_eq!(products.len(), 2);
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
