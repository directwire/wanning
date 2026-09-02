//! W-28 渠道签名管线测试:参数规范化本地契约钉死 + RSA sign/verify 往返(dev-only)。
//!
//! 纪律:全离线、零网络、零真实请求(只做报文层);测试 RSA 密钥对**现场生成**、
//! 只存在于本测试进程内存、绝不落仓绝不入 git、绝非真商户密钥。

use wanning_demo::signing::{
    canonical_query, CanonicalError, MessageSigner, SignatureVerifier, SigningError,
};

// ---------------------------------------------------------------------------
// 规范化本地契约(每条规则一个测试钉死;官方规范正文查不到,见模块文档)
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
    // 本地契约:值原样字节进签名串,不做 URL 转义(官方是否转义查不到 → TODO 联调核对)。
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
    // `sign` 不参与签名(自指);调用方就算把它塞进来也不进签名串。
    let s = canonical_query(&[("sign", "forged-by-agent"), ("biz", "1")]).expect("规范化");
    assert_eq!(s, "biz=1");
}

#[test]
fn sign_type_participates_pending_official_confirmation() {
    // 本地契约:除 `sign` 外一律参与签名。sign_type 是否被官方排除查不到正文
    // → 联调时逐条核对(TODO 账户开通后)。
    let s = canonical_query(&[("sign_type", "RSA2"), ("a", "1")]).expect("规范化");
    assert_eq!(s, "a=1&sign_type=RSA2");
}

#[test]
fn empty_value_participates_pending_official_confirmation() {
    // 本地契约:空值也参与签名(官方是否跳过空值查不到正文 → TODO 联调核对)。
    let s = canonical_query(&[("a", "1"), ("empty", "")]).expect("规范化");
    assert_eq!(s, "a=1&empty=");
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
// 签名/验签槽位:dev-only RSA 实现现场生成测试密钥对,往返自测
// ---------------------------------------------------------------------------

struct TestRsaSigner {
    inner: rsa::pkcs1v15::SigningKey<sha2::Sha256>,
}

impl MessageSigner for TestRsaSigner {
    fn alg(&self) -> &'static str {
        "sha256withrsa"
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
    assert_eq!(signer.alg(), "sha256withrsa");
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
