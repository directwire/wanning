//! W-50 验收:支付宝**真实报文模板**(公开文档直核填实,零密钥零真实消费)。
//!
//! 依据([公开文档直核],2026-09-03,全部静态 .md 抓取在档 target/w50/ 临时目录、
//! 引用 URL 落调研文档):
//! - 《自行实现签名》 https://opendocs.alipay.com/common/057k53
//! - 《自行实现验签》 https://opendocs.alipay.com/common/02mse7
//! - alipay.trade.pay(商家扣款版)接口文档(公共参数/业务参数/响应示例/触发通知)
//!   https://opendocs.alipay.com/open/08bntx (示例值 GENERAL_WITHHOLDING)
//! - 《商家扣款产品介绍》 https://opendocs.alipay.com/open/06de8c
//! - ANAI 实战交叉(本家真机过 APP 支付后端):网关固定/RSA2/回调验签剔除
//!   sign+sign_type 后字典序 k=v& ——与官方文档一致(双源印证,落调研文档)。
//!
//! 纪律:**本文件全部打本地测试替身(RecordingTransport/本地 mock server),零网络、
//! 零真实网关、零真实密钥**;测试 RSA 密钥对现场生成(自生成自销毁),只用来扮演
//! 「商户私钥」与「支付宝公钥」两端,验证签名/验签管线语义。「服务器认不认」
//! 如实标 [待实签]——真网关 + 真密钥那一步留账户开通后(见调研文档待实签清单)。

use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;

use wanning_demo::alipay::{
    build_trade_pay_request, cents_to_yuan_amount, sanitize_out_trade_no, verify_pay_notify,
    yuan_to_cents, AlipayBackend, AlipayRealConfig, OutgoingPayRequest, PayRequest, PayStatus,
    PaymentChannel, PaymentError, ALIPAY_GATEWAY,
};
use wanning_demo::guard::EnvSnapshot;
use wanning_demo::http::{ApiTransport, HttpFailure};
use wanning_demo::signing::{MessageSigner, SignatureVerifier, SigningError};

// ---------------------------------------------------------------------------
// 测试替身:录制传输 + 测试 RSA 密钥对(dev-only,现场生成,绝不落仓)
// ---------------------------------------------------------------------------

/// 录制 (url, body, headers) 并回放一段 canned 响应:把真实报文模板钉在断言里。
#[derive(Debug)]
struct RecordingTransport {
    captured: Mutex<CapturedRequests>,
    response: Mutex<Vec<String>>,
}

/// 一次捕获 = (url, body, headers);类型别名只为可读性(clippy type_complexity)。
type CapturedRequests = Vec<(String, String, Vec<(String, String)>)>;

impl RecordingTransport {
    fn new(responses: Vec<String>) -> Self {
        Self {
            captured: Mutex::new(Vec::new()),
            response: Mutex::new(responses),
        }
    }
    fn captured(&self) -> CapturedRequests {
        self.captured.lock().expect("录制锁").clone()
    }
}

impl ApiTransport for RecordingTransport {
    fn post_json(
        &self,
        url: &str,
        body: &str,
        headers: &[(String, String)],
    ) -> Result<String, HttpFailure> {
        self.captured.lock().expect("录制锁").push((
            url.to_string(),
            body.to_string(),
            headers.to_vec(),
        ));
        self.response
            .lock()
            .expect("响应锁")
            .pop()
            .ok_or_else(|| HttpFailure {
                status: None,
                timeout: false,
                message: "测试替身无更多 canned 响应".to_string(),
            })
    }
}

struct TestRsaSigner {
    inner: rsa::pkcs1v15::SigningKey<sha2::Sha256>,
}
impl MessageSigner for TestRsaSigner {
    fn alg(&self) -> &'static str {
        "RSA2"
    }
    fn sign(&self, canonical: &str) -> Result<Vec<u8>, SigningError> {
        use rsa::signature::{SignatureEncoding as _, Signer as _};
        Ok(self.inner.sign(canonical.as_bytes()).to_vec())
    }
}

struct TestRsaVerifier {
    inner: rsa::pkcs1v15::VerifyingKey<sha2::Sha256>,
}
impl SignatureVerifier for TestRsaVerifier {
    fn verify(&self, canonical: &str, signature: &[u8]) -> bool {
        let sig = match rsa::pkcs1v15::Signature::try_from(signature) {
            Ok(sig) => sig,
            Err(_) => return false,
        };
        rsa::signature::Verifier::verify(&self.inner, canonical.as_bytes(), &sig).is_ok()
    }
}

/// 现场生成 2048 位测试密钥对(扮演商户私钥 + 支付宝公钥两端;零真密钥)。
fn test_keypair() -> (TestRsaSigner, TestRsaVerifier) {
    use rand::rngs::OsRng;
    let private = rsa::RsaPrivateKey::new(&mut OsRng, 2048).expect("现场生成测试 RSA 密钥对");
    let public = private.to_public_key();
    (
        TestRsaSigner {
            inner: rsa::pkcs1v15::SigningKey::<sha2::Sha256>::new(private),
        },
        TestRsaVerifier {
            inner: rsa::pkcs1v15::VerifyingKey::<sha2::Sha256>::new(public),
        },
    )
}

// ---------------------------------------------------------------------------
// 测试夹具
// ---------------------------------------------------------------------------

fn pay_request() -> PayRequest {
    PayRequest {
        order_id: "JD-TEST-1".to_string(),
        amount_cents: 3990,
        delegation_id: "d1".to_string(),
        intent_nonce: 1,
    }
}

fn real_config() -> AlipayRealConfig {
    AlipayRealConfig {
        gateway: ALIPAY_GATEWAY.to_string(),
        app_id: "2021000100000000".to_string(),
        agreement_no: "20170322450983769228".to_string(),
        notify_url: None,
    }
}

/// 测试用 URL 编码(与实现同一 RFC 3986 规则:保留 [A-Za-z0-9-._~],其余全编码)。
/// 独立小实现:实现与测试各写一份,两边不一致时断言当场红。
fn form_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        let c = *byte as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// 按官方异步验签规则(剔除 sign/sign_type 后 url_decode、字典序、k=v&)拼出
/// 一条**已签名**的通知 form 报文(用测试密钥对扮演支付宝侧签名)。
fn signed_notify(pairs: &[(&str, String)], signer: &(dyn MessageSigner + Send + Sync)) -> String {
    let mut filtered: Vec<(&str, String)> = pairs
        .iter()
        .filter(|(k, _)| *k != "sign" && *k != "sign_type")
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    filtered.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let canonical = filtered
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    let sign = B64.encode(signer.sign(&canonical).expect("测试签名"));
    let mut all: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    all.push(("sign".to_string(), sign));
    all.iter()
        .map(|(k, v)| format!("{k}={}", form_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn notify_pairs(total_amount: &str, trade_status: &str) -> Vec<(&'static str, String)> {
    vec![
        ("app_id", "2021000100000000".to_string()),
        ("trade_no", "2013112011001004330000121536".to_string()),
        ("out_trade_no", "w_d1_1_JD_TEST_1".to_string()),
        ("total_amount", total_amount.to_string()),
        ("trade_status", trade_status.to_string()),
        (
            "notify_id",
            "91722adff935e8cfa58b3aabf4dead6ibe".to_string(),
        ),
        ("notify_time", "2026-09-03 12:00:00".to_string()),
        ("gmt_payment", "2026-09-03 12:00:00".to_string()),
        ("sign_type", "RSA2".to_string()),
    ]
}

/// 组装一段**已签名**的同步响应包(官方响应示例形态;测试密钥扮演支付宝侧)。
fn signed_pay_envelope(inner: &str, signer: &(dyn MessageSigner + Send + Sync)) -> String {
    let sign = B64.encode(signer.sign(inner).expect("测试签名"));
    format!(r#"{{"alipay_trade_pay_response":{inner},"sign":"{sign}"}}"#)
}

/// 官方响应示例向量(alipay.trade.pay 文档「响应示例·正常示例」,按本仓解析面
/// 截取字段;字段与取值逐字保留)。`total_amount` 用请求金额,便于对账断言。
fn official_success_inner(total_amount: &str, extra: &str) -> String {
    format!(
        r#"{{"code":"10000","msg":"Success","trade_no":"2013112011001004330000121536","out_trade_no":"w_d1_1_JD_TEST_1","buyer_logon_id":"159****5620","total_amount":"{total_amount}","receipt_amount":"{total_amount}","gmt_payment":"2014-11-27 15:45:57","buyer_open_id":"074a1CcTG1LelxKe4xQC0zgNdId0nxi95b5lsNpazWYoCo5","buyer_user_id":"2088101117955611"{extra}}}"#
    )
}

fn real_backend(
    transport: Arc<RecordingTransport>,
    signer: Arc<dyn MessageSigner + Send + Sync>,
    verifier: Arc<dyn SignatureVerifier + Send + Sync>,
) -> AlipayBackend {
    let mut env = EnvSnapshot::default();
    env.insert("WANNING_ALLOW_REAL_SPEND", "1");
    env.insert("WANNING_GLM_KEY", "k");
    env.insert("WANNING_JD_APP_KEY", "k");
    env.insert("WANNING_JD_APP_SECRET", "k");
    env.insert("WANNING_JD_ACCESS_TOKEN", "k");
    env.insert("WANNING_ALIPAY_APP_ID", "2021000100000000");
    env.insert("WANNING_ALIPAY_AGREEMENT_NO", "20170322450983769228");
    AlipayBackend::from_snapshot_real(&env)
        .expect("真实路径构建成功(零外网:传输为测试替身)")
        .with_signer(signer)
        .with_verifier(verifier)
        .with_transport(transport)
}

/// 测试密钥对装进 Arc(后端槽位消费一份,签名夹具持一份引用,同一对钥)。签名/
/// 验签**必须同钥**:响应由「支付宝」侧(测试扮演)签,后端用「支付宝公钥」验。
fn arc_keypair() -> (
    Arc<dyn MessageSigner + Send + Sync>,
    Arc<dyn SignatureVerifier + Send + Sync>,
) {
    let (signer, verifier) = test_keypair();
    (Arc::new(signer), Arc::new(verifier))
}

// ---------------------------------------------------------------------------
// ① 请求模板:官方网关/公共参数/biz_content/拆包(query=平台参数, body=biz_content)
// ---------------------------------------------------------------------------

#[test]
fn build_trade_pay_request_pins_official_wire_shape_and_signature() {
    let (signer, verifier) = test_keypair();
    let cfg = real_config();
    let timestamp = "2026-09-03 12:00:00";

    let outgoing: OutgoingPayRequest =
        build_trade_pay_request(&cfg, &pay_request(), timestamp, &signer).expect("模板构建");

    // ① 幂等键过官方 out_trade_no 字符集(仅字母数字下划线):'-' → '_'
    //    [公开文档直核: out_trade_no「仅支持字母、数字、下划线」]。
    assert!(outgoing.body.contains("w_d1_1_JD_TEST_1"), "{outgoing:?}");

    // ② 平台参数在 query(ASCII 升序 + sign 最后),biz_content 在 body
    //    [公开文档直核: 官方步骤5请求示例 + 「业务参数放 body、平台参数放 query」]。
    //    sign 的**值**从实际 URL 抽出(它是签名产物,正确性由下方 ⑤ 验签背书);
    //    位置/编码/前序参数形状由本断言逐段钉死。
    let sign_encoded = extract_query_param(&outgoing.url, "sign").expect("query 带 sign");
    assert_eq!(
        outgoing.url,
        format!(
            "{ALIPAY_GATEWAY}?app_id={app}&charset=utf-8&method=alipay.trade.pay&sign_type=RSA2\
             &timestamp={ts}&version=1.0&sign={sign}",
            app = cfg.app_id,
            ts = form_encode(timestamp),
            sign = sign_encoded,
        ),
        "query 形状必须与官方请求示例逐段一致"
    );

    // ③ Content-Type = application/x-www-form-urlencoded(网关表单语义,非 JSON)。
    assert_eq!(outgoing.content_type, "application/x-www-form-urlencoded");

    // ④ body = biz_content=<URL 编码的 biz JSON>;解码后字段逐项钉死。
    let encoded_biz = outgoing
        .body
        .strip_prefix("biz_content=")
        .expect("body 必须是 biz_content 表单项");
    let biz = percent_decode(encoded_biz);
    assert!(
        biz.contains(r#""out_trade_no":"w_d1_1_JD_TEST_1""#),
        "{biz}"
    );
    assert!(biz.contains(r#""total_amount":"39.90""#), "{biz}");
    assert!(biz.contains(r#""subject""#), "{biz}");
    assert!(
        biz.contains(r#""product_code":"GENERAL_WITHHOLDING""#),
        "{biz} [公开文档直核: product_code 固定 GENERAL_WITHHOLDING]"
    );
    assert!(
        biz.contains(r#""agreement_params":{"agreement_no":"20170322450983769228"}"#),
        "{biz} [公开文档直核: 代扣必传 agreement_params.agreement_no]"
    );

    // ⑤ 签名确实覆盖「官方规范化串」:测试端独立拼官方规则串 → 验签必须通过。
    let canonical = expected_canonical(&cfg, timestamp, &biz, None);
    let sign_b64 = percent_decode(&sign_encoded);
    assert!(
        verifier.verify(&canonical, &B64.decode(sign_b64).expect("base64")),
        "签名必须覆盖官方规范化串"
    );
}

#[test]
fn notify_url_when_configured_joins_query_and_signature() {
    let (signer, verifier) = test_keypair();
    let cfg = AlipayRealConfig {
        notify_url: Some("https://example.invalid/notify".to_string()),
        ..real_config()
    };
    let outgoing = build_trade_pay_request(&cfg, &pay_request(), "2026-09-03 12:00:00", &signer)
        .expect("模板构建");
    let biz = percent_decode(
        outgoing
            .body
            .strip_prefix("biz_content=")
            .expect("body 前缀"),
    );
    let canonical = expected_canonical(&cfg, "2026-09-03 12:00:00", &biz, None);
    let sign_b64 =
        percent_decode(&extract_query_param(&outgoing.url, "sign").expect("query 带 sign"));
    assert!(outgoing
        .url
        .contains("notify_url=https%3A%2F%2Fexample.invalid%2Fnotify"));
    let expected = {
        let mut pairs = [
            ("app_id", cfg.app_id.as_str()),
            ("charset", "utf-8"),
            ("method", "alipay.trade.pay"),
            ("sign_type", "RSA2"),
            ("timestamp", "2026-09-03 12:00:00"),
            ("version", "1.0"),
            ("notify_url", "https://example.invalid/notify"),
            ("biz_content", biz.as_str()),
        ];
        pairs.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&")
    };
    assert_eq!(canonical, expected, "notify_url 参与签名");
    assert!(verifier.verify(&canonical, &B64.decode(sign_b64).expect("base64")));
}

#[test]
fn out_trade_no_sanitization_pins_official_charset_and_length() {
    // [公开文档直核: out_trade_no「仅支持字母、数字、下划线」,64 字符以内]。
    assert_eq!(
        sanitize_out_trade_no("w-d1-1-JD-TEST-1").expect("常规键"),
        "w_d1_1_JD_TEST_1"
    );
    let long = "a".repeat(65);
    let err = sanitize_out_trade_no(&long).unwrap_err();
    assert!(matches!(err, PaymentError::InvalidRequest(_)), "{err}");
    assert!(err.to_string().contains("64"), "{err}");
    let edge = "a".repeat(64);
    assert_eq!(sanitize_out_trade_no(&edge).expect("恰 64 位允许"), edge);
}

// ---------------------------------------------------------------------------
// ② 金额语义:分 → 元字符串严格互转(禁浮点,这是钱)
// ---------------------------------------------------------------------------

#[test]
fn cents_to_yuan_amount_is_exact_two_decimal_string() {
    assert_eq!(cents_to_yuan_amount(1), "0.01");
    assert_eq!(cents_to_yuan_amount(5), "0.05");
    assert_eq!(cents_to_yuan_amount(100), "1.00");
    assert_eq!(cents_to_yuan_amount(3990), "39.90");
    assert_eq!(cents_to_yuan_amount(10_000_000_000), "100000000.00");
}

#[test]
fn yuan_to_cents_parses_strictly_and_rejects_ambiguity() {
    assert_eq!(yuan_to_cents("0.01").expect("两 位小数"), 1);
    assert_eq!(yuan_to_cents("88.88").expect("两位小数"), 8888);
    assert_eq!(yuan_to_cents("88.8").expect("一位小数"), 8880);
    assert_eq!(yuan_to_cents("10").expect("整数形"), 1000);
    for bad in ["88.881", "abc", "-1", "", "1.2.3", " 1.00"] {
        assert!(
            yuan_to_cents(bad).is_err(),
            "{bad:?} 必须被拒(金额歧义零容忍)"
        );
    }
}

#[test]
fn amount_above_official_cap_is_refused_before_network() {
    // [公开文档直核: total_amount 取值范围 [0.01,100000000]]。
    let (signer, _) = test_keypair();
    let mut request = pay_request();
    request.amount_cents = 10_000_000_001; // = 100000000.01 元,超上限
    let err = build_trade_pay_request(&real_config(), &request, "t", &signer).unwrap_err();
    assert!(matches!(err, PaymentError::InvalidRequest(_)), "{err}");
}

// ---------------------------------------------------------------------------
// ③ 时间戳:北京时间 yyyy-MM-dd HH:mm:ss(格式[公开文档直核];东八区[ANAI 实战])
// ---------------------------------------------------------------------------

#[test]
fn beijing_timestamp_pins_utc_plus_eight() {
    assert_eq!(
        wanning_demo::alipay::beijing_timestamp(0),
        "1970-01-01 08:00:00"
    );
    assert_eq!(
        wanning_demo::alipay::beijing_timestamp(1_700_000_000),
        "2023-11-15 06:13:20"
    );
}

// ---------------------------------------------------------------------------
// ④ 同步响应:验签先行 → 官方示例向量 → 状态映射与对账
// ---------------------------------------------------------------------------

#[test]
fn official_response_example_vector_maps_to_success() {
    let (signer, verifier) = test_keypair();
    let inner = official_success_inner("39.90", r#","async_payment_mode":"SYNC_DIRECT_PAY""#);
    let transport = Arc::new(RecordingTransport::new(vec![signed_pay_envelope(
        &inner, &signer,
    )]));
    let mut backend = real_backend(transport.clone(), Arc::new(signer), Arc::new(verifier));
    let result = backend
        .trigger_pay(&pay_request())
        .expect("同步直接扣款成功");
    assert_eq!(result.status, PayStatus::Success);
    assert_eq!(result.trade_no, "2013112011001004330000121536");
    assert_eq!(result.amount_cents, 3990);
    assert_eq!(result.out_request_no, "w-d1-1-JD-TEST-1");
    assert!(transport.captured()[0].0.starts_with(ALIPAY_GATEWAY));

    // Content-Type 必须是表单语义(网关契约),且恰好一个 content-type 头
    // (传输层默认 application/json 必须被覆盖,不能两个并存)。
    let (_, _, headers) = &transport.captured()[0];
    let content_types: Vec<&(String, String)> = headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .collect();
    assert_eq!(content_types.len(), 1, "恰好一个 content-type: {headers:?}");
    assert_eq!(
        content_types[0].1, "application/x-www-form-urlencoded",
        "{headers:?}"
    );
}

#[test]
fn async_payment_modes_other_than_sync_direct_map_to_pending() {
    // [公开文档直核: async_payment_mode 五枚举;非 SYNC_DIRECT_PAY 即异步,
    // 终态由**已验签回调**确认——同步返回不判终态(模板决策,见模块文档)。]
    let (signer, verifier) = arc_keypair();
    for mode in [
        "ASYNC_DELAY_PAY",
        "ASYNC_REALTIME_PAY",
        "NORMAL_ASYNC_PAY",
        "QUOTA_OCCUPYIED_ASYNC_PAY",
    ] {
        let inner = official_success_inner("39.90", &format!(r#","async_payment_mode":"{mode}""#));
        let transport = Arc::new(RecordingTransport::new(vec![signed_pay_envelope(
            &inner,
            signer.as_ref(),
        )]));
        let mut backend = real_backend(transport, signer.clone(), verifier.clone());
        let result = backend.trigger_pay(&pay_request()).expect("异步扣款受理");
        assert_eq!(result.status, PayStatus::Pending, "{mode} 必须映射 Pending");
    }
}

#[test]
fn sync_response_sign_tampering_is_fail_closed() {
    let (signer, verifier) = test_keypair();
    let inner = official_success_inner("39.90", "");
    let envelope = signed_pay_envelope(&inner, &signer);
    // 改响应正文一个字符(金额)→ 验签必须失败,绝不产出 PayResult。
    let tampered = envelope.replace("39.90", "39.91");
    let transport = Arc::new(RecordingTransport::new(vec![tampered]));
    let mut backend = real_backend(transport, Arc::new(signer), Arc::new(verifier));
    let err = backend.trigger_pay(&pay_request()).unwrap_err();
    assert!(matches!(err, PaymentError::BadResponse(_)), "{err}");
    assert!(err.to_string().contains("验签"), "{err}");
}

#[test]
fn sync_response_without_sign_or_envelope_is_refused() {
    let (signer, verifier) = arc_keypair();
    let inner = official_success_inner("39.90", "");
    // 缺 sign。
    let transport = Arc::new(RecordingTransport::new(vec![format!(
        r#"{{"alipay_trade_pay_response":{inner}}}"#
    )]));
    let mut backend = real_backend(transport, signer.clone(), verifier.clone());
    let err = backend.trigger_pay(&pay_request()).unwrap_err();
    assert!(matches!(err, PaymentError::BadResponse(_)), "{err}");

    // 缺 alipay_trade_pay_response(网关异常包)。
    let transport = Arc::new(RecordingTransport::new(vec![
        r#"{"error":"boom"}"#.to_string()
    ]));
    let mut backend = real_backend(transport, signer.clone(), verifier.clone());
    assert!(matches!(
        backend.trigger_pay(&pay_request()),
        Err(PaymentError::BadResponse(_))
    ));

    // sign 不是合法 base64。
    let transport = Arc::new(RecordingTransport::new(vec![format!(
        r#"{{"alipay_trade_pay_response":{inner},"sign":"@@@not-base64@@@"}}"#
    )]));
    let mut backend = real_backend(transport, signer.clone(), verifier.clone());
    assert!(matches!(
        backend.trigger_pay(&pay_request()),
        Err(PaymentError::BadResponse(_))
    ));
}

#[test]
fn amount_mismatch_between_request_and_response_is_refused() {
    let (signer, verifier) = test_keypair();
    let inner = official_success_inner("39.89", ""); // 请求 39.90 / 响应 39.89
    let transport = Arc::new(RecordingTransport::new(vec![signed_pay_envelope(
        &inner, &signer,
    )]));
    let mut backend = real_backend(transport, Arc::new(signer), Arc::new(verifier));
    let err = backend.trigger_pay(&pay_request()).unwrap_err();
    assert!(matches!(err, PaymentError::BadResponse(_)), "{err}");
    assert!(err.to_string().contains("39.89"), "{err}");
}

#[test]
fn gateway_error_codes_carry_code_and_sub_code() {
    // [公开文档直核: 网关返回码 40004=业务处理失败,sub_code=ACQ.*;20000=服务不可用。]
    let (signer, verifier) = arc_keypair();
    let inner = r#"{"code":"40004","msg":"Business Failed","sub_code":"ACQ.AGREEMENT_INVALID","sub_msg":"协议无效"}"#;
    let transport = Arc::new(RecordingTransport::new(vec![signed_pay_envelope(
        inner,
        signer.as_ref(),
    )]));
    let mut backend = real_backend(transport, signer.clone(), verifier.clone());
    let err = backend.trigger_pay(&pay_request()).unwrap_err();
    match err {
        PaymentError::GatewayRejected {
            code,
            sub_code,
            sub_msg,
        } => {
            assert_eq!(code, "40004");
            assert_eq!(sub_code.as_deref(), Some("ACQ.AGREEMENT_INVALID"));
            assert_eq!(sub_msg.as_deref(), Some("协议无效"));
        }
        other => panic!("必须映射 GatewayRejected,得到 {other}"),
    }

    let inner = r#"{"code":"20000","msg":"Service Currently Unavailable","sub_code":"isp.unknow-error","sub_msg":"服务暂不可用"}"#;
    let transport = Arc::new(RecordingTransport::new(vec![signed_pay_envelope(
        inner,
        signer.as_ref(),
    )]));
    let mut backend = real_backend(transport, signer.clone(), verifier.clone());
    assert!(matches!(
        backend.trigger_pay(&pay_request()),
        Err(PaymentError::GatewayRejected { .. })
    ));
}

// ---------------------------------------------------------------------------
// ⑤ 异步通知:原始报文先验签再解析 → apply_pay_notify 幂等入账
// ---------------------------------------------------------------------------

#[test]
fn verified_notify_maps_official_params_and_feeds_apply_pay_notify() {
    let (signer, verifier) = test_keypair();
    let raw = signed_notify(&notify_pairs("39.90", "TRADE_SUCCESS"), &signer);

    let notify = verify_pay_notify(&raw, &verifier).expect("验签通过的回调必须可解析");
    assert_eq!(notify.out_request_no, "w_d1_1_JD_TEST_1");
    assert_eq!(notify.trade_no, "2013112011001004330000121536");
    assert_eq!(notify.status, PayStatus::Success);
    assert_eq!(notify.amount_cents, 3990);

    // 验签通过的通知直接进既有幂等管线(W-11 语义零改动)。
    let mut state = wanning_demo::alipay::TradeState {
        out_request_no: notify.out_request_no.clone(),
        trade_no: notify.trade_no.clone(),
        amount_cents: 3990,
        status: PayStatus::Pending,
    };
    assert!(matches!(
        wanning_demo::alipay::apply_pay_notify(&mut state, &notify),
        Ok(true)
    ));
}

#[test]
fn notify_trade_status_enum_maps_all_official_values() {
    // [公开文档直核: 触发通知类型表 WAIT_BUYER_PAY/TRADE_CLOSED/TRADE_SUCCESS/
    // TRADE_FINISHED(alipay.trade.pay 文档)。]
    let (signer, verifier) = test_keypair();
    let cases = [
        ("TRADE_SUCCESS", PayStatus::Success),
        ("TRADE_FINISHED", PayStatus::Success),
        ("WAIT_BUYER_PAY", PayStatus::Pending),
        ("TRADE_CLOSED", PayStatus::Failed),
    ];
    for (status, expected) in cases {
        let raw = signed_notify(&notify_pairs("39.90", status), &signer);
        let notify = verify_pay_notify(&raw, &verifier).expect(status);
        assert_eq!(notify.status, expected, "{status}");
    }
    // 枚举外取值 fail-closed(不猜测语义)。
    let raw = signed_notify(&notify_pairs("39.90", "SOMETHING_ELSE"), &signer);
    assert!(matches!(
        verify_pay_notify(&raw, &verifier),
        Err(PaymentError::BadResponse(_))
    ));
}

#[test]
fn notify_with_tampered_or_missing_sign_is_refused() {
    let (signer, verifier) = test_keypair();

    // 改一个值 → 验签不过,拒绝解析(fail-closed:验签先行)。
    let raw = signed_notify(&notify_pairs("39.90", "TRADE_SUCCESS"), &signer);
    let tampered = raw.replace("total_amount=39.90", "total_amount=0.01");
    let err = verify_pay_notify(&tampered, &verifier).unwrap_err();
    assert!(matches!(err, PaymentError::BadResponse(_)), "{err}");
    assert!(err.to_string().contains("验签"), "{err}");

    // 缺 sign → 拒。
    let unsigned: Vec<(&str, String)> = notify_pairs("39.90", "TRADE_SUCCESS");
    let no_sign = unsigned
        .iter()
        .map(|(k, v)| format!("{k}={}", form_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    assert!(matches!(
        verify_pay_notify(&no_sign, &verifier),
        Err(PaymentError::BadResponse(_))
    ));

    // sign 非 base64 → 拒。
    let mut bad = unsigned.clone();
    bad.push(("sign", "@@@@".to_string()));
    let bad_raw = bad
        .iter()
        .map(|(k, v)| format!("{k}={}", form_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    assert!(matches!(
        verify_pay_notify(&bad_raw, &verifier),
        Err(PaymentError::BadResponse(_))
    ));
}

#[test]
fn notify_amount_must_parse_to_exact_cents() {
    let (signer, verifier) = test_keypair();
    let raw = signed_notify(&notify_pairs("39.901", "TRADE_SUCCESS"), &signer);
    assert!(matches!(
        verify_pay_notify(&raw, &verifier),
        Err(PaymentError::BadResponse(_))
    ));
}

// ---------------------------------------------------------------------------
// ⑥ 真实路径 fail-closed:护栏/配置/签名槽位缺一即拒
// ---------------------------------------------------------------------------

#[test]
fn real_path_guard_and_config_requirements_pin_fail_closed_chain() {
    // ① 全空 env:护栏拦下(不变,W-07 语义)。
    let err = AlipayBackend::from_snapshot_real(&EnvSnapshot::default()).unwrap_err();
    assert!(matches!(err, PaymentError::GuardBlocked(_)), "{err}");

    let mut env = EnvSnapshot::default();
    env.insert("WANNING_ALLOW_REAL_SPEND", "1");
    env.insert("WANNING_GLM_KEY", "k");
    env.insert("WANNING_JD_APP_KEY", "k");
    env.insert("WANNING_JD_APP_SECRET", "k");
    env.insert("WANNING_JD_ACCESS_TOKEN", "k");

    // ② 缺 app_id:点名 WANNING_ALIPAY_APP_ID。
    let err = AlipayBackend::from_snapshot_real(&env).unwrap_err();
    assert!(matches!(err, PaymentError::Config(_)), "{err}");
    assert!(err.to_string().contains("WANNING_ALIPAY_APP_ID"), "{err}");

    // ③ 缺 agreement_no:点名 WANNING_ALIPAY_AGREEMENT_NO(没有协议号绝不发扣款,
    //    协议内扣款语义,见模块文档)。
    env.insert("WANNING_ALIPAY_APP_ID", "2021000100000000");
    let err = AlipayBackend::from_snapshot_real(&env).unwrap_err();
    assert!(matches!(err, PaymentError::Config(_)), "{err}");
    assert!(
        err.to_string().contains("WANNING_ALIPAY_AGREEMENT_NO"),
        "{err}"
    );

    // ④ 全齐:网关默认 = 官方固定网关 [公开文档直核];env 可覆盖(测试打本地替身)。
    env.insert("WANNING_ALIPAY_AGREEMENT_NO", "20170322450983769228");
    let backend = AlipayBackend::from_snapshot_real(&env).expect("全齐构建");
    assert_eq!(backend.endpoint(), ALIPAY_GATEWAY);
    env.insert("WANNING_ALIPAY_ENDPOINT", "http://127.0.0.1:1");
    let backend = AlipayBackend::from_snapshot_real(&env).expect("覆盖构建");
    assert_eq!(backend.endpoint(), "http://127.0.0.1:1");
}

#[test]
fn real_mode_without_signer_or_verifier_fails_closed_before_network() {
    let mut env = EnvSnapshot::default();
    env.insert("WANNING_ALLOW_REAL_SPEND", "1");
    env.insert("WANNING_GLM_KEY", "k");
    env.insert("WANNING_JD_APP_KEY", "k");
    env.insert("WANNING_JD_APP_SECRET", "k");
    env.insert("WANNING_JD_ACCESS_TOKEN", "k");
    env.insert("WANNING_ALIPAY_APP_ID", "app");
    env.insert("WANNING_ALIPAY_AGREEMENT_NO", "agr");

    let transport = Arc::new(RecordingTransport::new(vec![]));
    let mut backend = AlipayBackend::from_snapshot_real(&env)
        .expect("配置齐")
        .with_transport(transport.clone());
    // 无签名槽位:拒,零出网。
    let err = backend.trigger_pay(&pay_request()).unwrap_err();
    assert!(matches!(err, PaymentError::Config(_)), "{err}");
    assert!(err.to_string().contains("签名"), "{err}");
    assert!(transport.captured().is_empty(), "绝不能出网");

    // 只给签名不给验签:同样拒(验不过的响应绝不采信)。
    let (signer, _) = test_keypair();
    let mut backend = backend.with_signer(Arc::new(signer));
    let err = backend.trigger_pay(&pay_request()).unwrap_err();
    assert!(matches!(err, PaymentError::Config(_)), "{err}");
    assert!(transport.captured().is_empty(), "绝不能出网");
}

// ---------------------------------------------------------------------------
// 小工具(测试端独立实现,与实现不一致时断言当场红)
// ---------------------------------------------------------------------------

fn extract_query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split('?').nth(1)?;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

fn percent_decode(value: &str) -> String {
    let src = value.as_bytes();
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'%' && i + 3 <= src.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&src[i + 1..i + 3]).unwrap_or("zz"), 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(src[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn expected_canonical(
    cfg: &AlipayRealConfig,
    timestamp: &str,
    biz_content: &str,
    _extra: Option<()>,
) -> String {
    let mut pairs: Vec<(String, String)> = vec![
        ("app_id".to_string(), cfg.app_id.clone()),
        ("charset".to_string(), "utf-8".to_string()),
        ("method".to_string(), "alipay.trade.pay".to_string()),
        ("sign_type".to_string(), "RSA2".to_string()),
        ("timestamp".to_string(), timestamp.to_string()),
        ("version".to_string(), "1.0".to_string()),
        ("biz_content".to_string(), biz_content.to_string()),
    ];
    if let Some(url) = &cfg.notify_url {
        pairs.push(("notify_url".to_string(), url.clone()));
    }
    pairs.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}
