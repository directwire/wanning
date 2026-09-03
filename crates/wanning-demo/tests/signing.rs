//! W-28/W-50 渠道签名管线测试:参数规范化契约钉死 + RSA sign/verify 往返(dev-only)。
//!
//! W-50 起规范化契约 = **支付宝官方规则**([公开文档直核],来源:
//! 《自行实现签名》https://opendocs.alipay.com/common/057k53 、《自行实现验签》
//! https://opendocs.alipay.com/common/02mse7 ;W-28 时查不到正文、按本地 mock
//! 契约钉死的两条(空值参与/签名不转义)已被官方规则取代并在此改钉)。
//! 仍属「待实签」的只有一条:**服务器认不认**(真网关 + 真密钥,账户开通后)。
//!
//! 纪律:全离线、零网络、零真实请求(只做报文层);测试 RSA 密钥对**现场生成**、
//! 只存在于本测试进程内存、绝不落仓绝不入 git、绝非真商户密钥。

use wanning_demo::signing::{
    canonical_notify_string, canonical_query, CanonicalError, MessageSigner, SignatureVerifier,
    SigningError,
};

// ---------------------------------------------------------------------------
// 请求签名规范化契约(每条规则一个测试钉死;来源见模块头注释)
// ---------------------------------------------------------------------------

#[test]
fn canonical_is_order_independent_and_pins_concatenation() {
    let a = [
        ("out_trade_no", "20260902001"),
        ("subject", "测试商品"),
        ("total_amount", "5.00"),
        ("product_code", "CYCZK"),
    ];
    let b = [
        ("total_amount", "5.00"),
        ("subject", "测试商品"),
        ("out_trade_no", "20260902001"),
        ("product_code", "CYCZK"),
    ];
    let c = [
        ("product_code", "CYCZK"),
        ("out_trade_no", "20260902001"),
        ("total_amount", "5.00"),
        ("subject", "测试商品"),
    ];
    let ca = canonical_query(&a).expect("规范化");
    let cb = canonical_query(&b).expect("规范化");
    let cc = canonical_query(&c).expect("规范化");
    // 同参数不同输入顺序 → 同签名串(W-28 验收门禁核心)
    assert_eq!(ca, cb);
    assert_eq!(cb, cc);
    // 且与逐条拼出的期望串逐字节相等(key 字节升序 + `key=value` + `&` 连接)
    assert_eq!(
        ca,
        "out_trade_no=20260902001&product_code=CYCZK&subject=测试商品&total_amount=5.00"
    );
}

#[test]
fn special_chars_are_verbatim_not_escaped() {
    // [公开文档直核 W-50] 官方步骤2「拼接签名原文」:`key=value` 用 `&` 连接,
    // 值**原样**(URL 编码发生在签名之后的步骤4「拼接完整请求」,不属于签名串)。
    // `&`、`=`、空格、中文逐字节原样出现。
    let s = canonical_query(&[("memo", "a&b=c d 中文"), ("k", "v")]).expect("规范化");
    assert_eq!(s, "k=v&memo=a&b=c d 中文");
}

#[test]
fn sort_is_byte_order_uppercase_before_lowercase() {
    // 'Z'(0x5A) < 'a'(0x61):按字节序排,不是字典序/本地化序。
    let s = canonical_query(&[("apple", "1"), ("Zebra", "2")]).expect("规范化");
    assert_eq!(s, "Zebra=2&apple=1");
}

#[test]
fn sign_key_never_enters_canonical_string() {
    // [公开文档直核 W-50] 官方步骤1:排除 `sign` 字段(自指)。
    let s = canonical_query(&[("sign", "forged-by-agent"), ("biz", "1")]).expect("规范化");
    assert_eq!(s, "biz=1");
}

#[test]
fn sign_type_participates_per_official_example() {
    // [公开文档直核 W-50] 官方步骤1 的示例参数表里 sign_type=RSA2 出现在排序后
    // 的待签名清单中(只有 sign/空值/二进制被排除)→ sign_type 参与签名。
    let s = canonical_query(&[("sign_type", "RSA2"), ("a", "1")]).expect("规范化");
    assert_eq!(s, "a=1&sign_type=RSA2");
}

#[test]
fn empty_values_are_excluded_per_official_rules() {
    // [公开文档直核 W-50] 官方步骤1:排除「值为空(包括 空白字符 和 null)的参数」。
    // W-28 本地契约选了「空值参与」,与官方规则相反 → W-50 改钉官方规则。
    let s = canonical_query(&[("a", "1"), ("empty", "")]).expect("规范化");
    assert_eq!(s, "a=1", "空值参数不进签名串");
    let w = canonical_query(&[("a", "1"), ("blank", "   ")]).expect("规范化");
    assert_eq!(w, "a=1", "纯空白字符值同样按「值为空」排除");
}

#[test]
fn optional_params_are_omitted_by_caller_not_null_filled() {
    // [公开文档直核 W-50] 官方「值为空」含 null——那是 Java Map 语境;本仓参数面
    // 是 (&str, &str),null 不存在,可选参数由调用方**整体省略**(adapter 纪律)。
    // 因此官方 null 排除规则在本仓由「不传」表达,规范化层只处理空串/空白。
    let s = canonical_query(&[("a", "1")]).expect("规范化");
    assert_eq!(s, "a=1");
}

#[test]
fn empty_param_set_canonicalizes_to_empty_string() {
    assert_eq!(canonical_query(&[]), Ok(String::new()));
}

#[test]
fn duplicate_key_is_fail_closed() {
    // 同名参数两次 → 签名串有歧义,拒收(adapter 层不会产生,fail-closed 保底)。
    assert_eq!(
        canonical_query(&[("amount", "100"), ("amount", "200")]),
        Err(CanonicalError::DuplicateKey("amount".to_string()))
    );
}

#[test]
fn empty_key_is_fail_closed() {
    assert_eq!(canonical_query(&[("", "v")]), Err(CanonicalError::EmptyKey));
}

// ---------------------------------------------------------------------------
// 异步通知验签规范化契约(W-50 [公开文档直核];来源见模块头注释 02mse7)
// ---------------------------------------------------------------------------

#[test]
fn notify_canonical_excludes_sign_and_sign_type_per_official_rules() {
    // [公开文档直核 W-50] 官方异步验签第2步:「除去 sign、sign_type 两个参数外,
    // 凡是通知返回的参数皆是待验签的参数」。(生活号场景保留 sign_type 的例外
    // 与 Wanning 无关——Wanning 不是生活号,注释在档即可。)
    let s = canonical_notify_string(&[
        ("sign", "IGNORED"),
        ("sign_type", "RSA2"),
        ("out_trade_no", "w-d1-1-JD-TEST-1"),
        ("trade_status", "TRADE_SUCCESS"),
    ])
    .expect("规范化");
    assert_eq!(
        s,
        "out_trade_no=w-d1-1-JD-TEST-1&trade_status=TRADE_SUCCESS"
    );
}

#[test]
fn notify_canonical_keeps_empty_values_unlike_request_rules() {
    // [公开文档直核 W-50] 请求签名排除空值;**通知验签**的官方规则没有空值排除
    // ——「凡是通知返回的参数皆是待验签的参数」。两条契约刻意不同,各自钉死。
    let s = canonical_notify_string(&[("a", "1"), ("empty", "")]).expect("规范化");
    assert_eq!(s, "a=1&empty=");
}

#[test]
fn notify_canonical_sorts_and_joins_byte_order() {
    let s = canonical_notify_string(&[("b", "2"), ("A", "1"), ("c", "3")]).expect("规范化");
    assert_eq!(s, "A=1&b=2&c=3");
}

#[test]
fn notify_canonical_rejects_duplicate_and_empty_keys() {
    assert_eq!(
        canonical_notify_string(&[("k", "1"), ("k", "2")]),
        Err(CanonicalError::DuplicateKey("k".to_string()))
    );
    assert_eq!(
        canonical_notify_string(&[("", "v")]),
        Err(CanonicalError::EmptyKey)
    );
}

// ---------------------------------------------------------------------------
// 签名/验签槽位:dev-only RSA 实现现场生成测试密钥对,往返自测
// ---------------------------------------------------------------------------

struct TestRsaSigner {
    inner: rsa::pkcs1v15::SigningKey<sha2::Sha256>,
}

impl MessageSigner for TestRsaSigner {
    fn alg(&self) -> &'static str {
        // [公开文档直核 W-50] 官方算法标识 = sign_type `RSA2`(SHA256WithRSA,
        // RSA 密钥 ≥2048 位)。W-28 本地契约命名 `sha256withrsa` 已被官方名取代。
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

/// 现场生成 2048 位测试密钥对:只在调用进程内存里,自生成自销毁,绝不落仓。
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

#[test]
fn rsa_sign_verify_roundtrip_and_fail_closed_paths() {
    let (signer, verifier) = test_keypair();
    let canonical = canonical_query(&[("out_trade_no", "20260902001"), ("total_amount", "5.00")])
        .expect("规范化");

    let sig = signer.sign(&canonical).expect("签名成功");
    assert!(verifier.verify(&canonical, &sig), "自己签的必须自己验得过");

    // 报文被改一个字符 → 拒(fail-closed)
    let tampered = canonical.replace("5.00", "5000");
    assert!(!verifier.verify(&tampered, &sig), "报文被改过必须验不过");

    // 别的密钥签的名 → 拒
    let (other, _) = test_keypair();
    let wrong = other.sign(&canonical).expect("签名成功");
    assert!(!verifier.verify(&canonical, &wrong), "别人的签名必须验不过");

    // 垃圾字节 → 返回 false,不 panic
    assert!(!verifier.verify(&canonical, b"not-a-signature"));
    // 空签名 → 返回 false,不 panic
    assert!(!verifier.verify(&canonical, &[]));
}

#[test]
fn rsa_signature_is_deterministic_for_same_message() {
    let (signer, _) = test_keypair();
    let canonical = canonical_query(&[("a", "1")]).expect("规范化");
    assert_eq!(
        signer.sign(&canonical).expect("签名成功"),
        signer.sign(&canonical).expect("签名成功"),
        "同一报文两次签名必须逐字节相同(PKCS#1 v1.5 确定性)"
    );
}

#[test]
fn alg_name_is_pinned() {
    let (signer, _) = test_keypair();
    assert_eq!(signer.alg(), "RSA2");
}

#[test]
fn signing_error_display_is_actionable() {
    // 报错必须说清「该做什么」,不给一句空泛的 failed。
    assert!(SigningError::NoKeyMaterial
        .to_string()
        .contains("账户开通后"));
    assert!(SigningError::KeyRejected("bad pem".to_string())
        .to_string()
        .contains("bad pem"));
}
