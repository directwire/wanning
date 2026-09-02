//! W-39 验收:美团 adapter(**契约占位** + 本地 mock,不碰真端点)。
//!
//! 依赖 W-38 调研结论:美团查不到用户侧免密/代扣开放 API,官方报文字段零 [直核]
//! → 本任务按任务书「若调研直核字段不足以建模,缩为 trait 占位 + TODO 清单,同样
//! 算完成(诚实优于臆造)」分支执行:管线(trait/传输/错误映射/解析/护栏)照建照测,
//! 契约体是本地 mock,绝不臆造美团字段。全部打本地 TcpListener,零外网。

use std::sync::Arc;

use wanning_demo::guard::EnvSnapshot;
use wanning_demo::http::HttpFailure;
use wanning_demo::jd::{BackendError, CommerceBackend, CreateOrderRequest, SearchRequest};
use wanning_demo::meituan::{MeituanBackend, MeituanTransport, UreqMeituanTransport};

mod common;
use common::{spawn_json_mock, MockJsonServer};

// 本地 mock 契约(wanning-demo 自定义,非美团报文;真实字段 W-38 零直核,绝不臆造)。
fn mock_search_body() -> String {
    serde_json::json!({
        "products": [
            {"sku_id": "MT-1", "title": "外卖套餐 A", "price_cents": 2500, "merchant_id": "meituan:poi-1"}
        ]
    })
    .to_string()
}

fn mock_order_body() -> String {
    serde_json::json!({"order_id": "MT-TEST-1", "sku_id": "MT-1", "amount_cents": 2500}).to_string()
}

fn search_request() -> SearchRequest {
    SearchRequest {
        keyword: "外卖".to_string(),
        max_price_cents: Some(5_000),
        limit: 3,
    }
}

fn order_request() -> CreateOrderRequest {
    CreateOrderRequest {
        sku_id: "MT-1".to_string(),
        quantity: 1,
        amount_cents: 2500,
        delegation_id: "d1".to_string(),
        intent_nonce: 1,
    }
}

fn backend_on(mock: &MockJsonServer) -> MeituanBackend {
    MeituanBackend::new_mock(&mock.url(), Arc::new(UreqMeituanTransport))
}

#[test]
fn module_doc_declares_contract_placeholder_honestly() {
    // 诚实纪律的机器锁:模块文档必须写明「契约占位」与 W-38 结论依据,
    // 防止将来有人把这层 mock 契约当成美团真实报文。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/meituan.rs"));
    assert!(source.contains("契约占位"), "模块文档必须声明占位性质");
    assert!(source.contains("查不到"), "模块文档必须引用 W-38 调研结论");
    assert!(
        source.contains("WANNING_MEITUAN_ENDPOINT"),
        "真实端点 env 名必须在档"
    );
    assert!(source.contains("TODO(再评估触发后)"), "TODO 清单必须在档");
}

#[test]
fn search_happy_path_parses_products_from_local_mock() {
    let mock = spawn_json_mock(vec![(200, mock_search_body())]);
    let mut backend = backend_on(&mock);

    let products = backend.search(&search_request()).expect("search 成功");
    assert_eq!(products.len(), 1);
    assert_eq!(products[0].sku_id, "MT-1");
    assert_eq!(products[0].merchant_id, "meituan:poi-1");

    let request = &mock.recorded_requests()[0];
    assert!(request.contains("外卖"), "关键词要进请求体: {request}");
    assert!(request.contains("Bearer"), "mock 契约带演示头: {request}");
}

#[test]
fn create_order_happy_path_returns_order_ref_and_carries_audit_linkage() {
    let mock = spawn_json_mock(vec![(200, mock_order_body())]);
    let mut backend = backend_on(&mock);

    let order = backend.create_order(&order_request()).expect("下单成功");
    assert_eq!(order.order_id, "MT-TEST-1");
    assert_eq!(order.amount_cents, 2500);

    let request = &mock.recorded_requests()[0];
    // 审计关联必须进请求:订单能回溯到授权(委托 id + 意图 nonce)。
    assert!(request.contains("delegation_id"), "{request}");
    assert!(request.contains("d1"), "{request}");
    assert!(request.contains("intent_nonce"), "{request}");
}

#[test]
fn http_errors_are_reported_not_swallowed() {
    for status in [400u16, 500] {
        let mock = spawn_json_mock(vec![(status, "{\"error\":\"mock\"}".to_string())]);
        let mut backend = backend_on(&mock);
        let err = backend.search(&search_request()).unwrap_err();
        assert!(
            matches!(err, BackendError::Http { status: s, .. } if s == status),
            "{err}"
        );
    }
}

#[test]
fn bad_response_is_reported_not_swallowed() {
    let mock = spawn_json_mock(vec![(200, "这不是 JSON".to_string())]);
    let mut backend = backend_on(&mock);
    let err = backend.search(&search_request()).unwrap_err();
    assert!(matches!(err, BackendError::BadResponse(_)), "{err}");

    // 200 + 合法 JSON 但缺字段:同样报错(不静默返回空列表)。
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
    impl MeituanTransport for TimingOutTransport {
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

    let mut backend = MeituanBackend::new_mock("http://127.0.0.1:9", Arc::new(TimingOutTransport));
    let err = backend.search(&search_request()).unwrap_err();
    assert!(matches!(err, BackendError::Timeout(_)), "{err}");
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

    let mut bad_order = order_request();
    bad_order.amount_cents = 0;
    assert!(matches!(
        backend.create_order(&bad_order),
        Err(BackendError::InvalidRequest(_))
    ));
    assert!(mock.recorded_requests().is_empty(), "非法请求绝不能出网");
}

#[test]
fn real_path_fails_closed_and_config_error_names_w38_conclusion() {
    // ① 全空 env:被护栏拦下(与京东同一道 W-07 门)。
    let err = MeituanBackend::from_snapshot_real(&EnvSnapshot::default()).unwrap_err();
    assert!(matches!(err, BackendError::GuardBlocked(_)), "{err}");
    assert!(
        err.to_string().contains("WANNING_ALLOW_REAL_SPEND"),
        "{err}"
    );

    // ② 护栏 env 全齐但端点未知(当前真相):拒配 + 点名 W-38 结论,绝不臆造 URL。
    let mut env = EnvSnapshot::default();
    env.insert("WANNING_ALLOW_REAL_SPEND", "1");
    env.insert("WANNING_GLM_KEY", "k");
    env.insert("WANNING_JD_APP_KEY", "k");
    env.insert("WANNING_JD_APP_SECRET", "k");
    env.insert("WANNING_JD_ACCESS_TOKEN", "k");
    let err = MeituanBackend::from_snapshot_real(&env).unwrap_err();
    assert!(matches!(err, BackendError::Config(_)), "{err}");
    let message = err.to_string();
    assert!(message.contains("WANNING_MEITUAN_ENDPOINT"), "{message}");
    assert!(message.contains("W-38"), "报错要点名调研依据: {message}");

    // ③ 端点显式指向本地 mock(全齐):管线走通,且全程零外网。
    let mock = spawn_json_mock(vec![(200, mock_search_body())]);
    env.insert("WANNING_MEITUAN_ENDPOINT", &mock.url());
    let mut backend = MeituanBackend::from_snapshot_real(&env).expect("全齐应构建成功");
    let products = backend.search(&search_request()).expect("search 成功");
    assert_eq!(products.len(), 1);
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
