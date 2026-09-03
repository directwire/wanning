//! W-31 验收:ed25519 第三方可验锚点(v2)。
//!
//! W-23 的诚实边界「HMAC 对称,第三方不可独立验证」由本任务升级:锚点 v2 用
//! ed25519 非对称签名,**公钥随锚点走**,第三方无需任何密钥文件即可验
//! (`wanning-anchor-verify` bin)。HMAC v1 模式保留(向后兼容,锚点文件带
//! version 字段)。依赖决策(A 案,落决策记录):ed25519 手写
//! 不可接受——哈希能手写因为 spec 短、向量密;曲线实现边缘 case 致命,
//! 引 `ed25519-dalek`(本仓第一个运行时外部加密依赖,只进 demo 工具面,
//! core/闸/MCP/SDK 依赖树零增长)。
//!
//! RFC 8032 §7.1 测试向量来源:<https://www.rfc-editor.org/rfc/rfc8032#section-7.1>
//! (直核,零编造)。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use wanning_core::anchor::{AnchorFile, ANCHOR_SCHEMA};
use wanning_core::clock::MockClock;
use wanning_core::delegation::Delegation;
use wanning_core::error::CoreError;
use wanning_core::intent::SpendIntent;
use wanning_core::state::WanningState;
use wanning_demo::anchor_cmd::AnchorKey;
use wanning_demo::anchor_v2::{
    public_key_from_seed_hex, sign_v2, verify_ed25519_hex, verify_v2, AnchorFileV2, Ed25519Seed,
    ANCHOR_SCHEMA_V2,
};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn tmp_path(tag: &str, ext: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("wanning-demo-anchor-v2-tests");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间早于 Unix 纪元")
        .as_nanos();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("{tag}-{nanos}-{seq}-{}.{ext}", std::process::id()))
}

/// 三行样本账:注册 → 放行 ¥5 → 超额拒(与 W-23 测试同一样本,结论可比)。
fn build_sample_wal(path: &Path) {
    let clock = MockClock::new(1_700_000_000);
    let mut state = WanningState::with_wal(Arc::new(clock.clone()), path).expect("开 WAL");
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
        .decide(&SpendIntent::new(
            "d1",
            1,
            500,
            "jd:shop-1",
            "grocery",
            "预算内放行",
        ))
        .expect("放行");
    clock.set_now(1_700_000_060);
    state
        .decide(&SpendIntent::new(
            "d1",
            2,
            900,
            "jd:shop-1",
            "grocery",
            "超出预算",
        ))
        .expect("超额拒");
}

fn seed_file(hex: &str) -> (PathBuf, Ed25519Seed) {
    let path = tmp_path("seed", "hex");
    std::fs::write(&path, format!("{hex}\n")).expect("写种子文件");
    let seed = Ed25519Seed::from_hex_file(&path).expect("读种子");
    (path, seed)
}

const SEED_HEX_BOSS: &str = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";
const SEED_HEX_ATTACKER: &str = "bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22";

// ---------------------------------------------------------------------------
// RFC 8032 §7.1 公开向量(直核零编造)
// ---------------------------------------------------------------------------

/// RFC 8032 §7.1 TEST 1/2/3:公开钥 + 消息 + 签名逐条验过;
/// 私钥种子 → 公钥推导同样逐条对上。
#[test]
fn rfc8032_test_vectors_pass_with_strict_verification() {
    // (种子, 公钥, 消息字节, 签名)——消息是**字节串**:TEST 2 的消息是单字节
    // 0x72,TEST 3 是 0xaf82,不是 ASCII 文本。
    let cases: [(&str, &str, Vec<u8>, &str); 3] = [
        (
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
            vec![],
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e0652249015\
             55fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        ),
        (
            "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
            "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
            vec![0x72],
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da\
             085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
        ),
        (
            "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
            "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
            vec![0xaf, 0x82],
            "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac\
             18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
        ),
    ];
    for (idx, (seed, public, message, signature)) in cases.iter().enumerate() {
        assert_eq!(
            public_key_from_seed_hex(seed).expect("种子推公钥"),
            *public,
            "RFC 8032 用例 {} 种子→公钥",
            idx + 1
        );
        verify_ed25519_hex(public, message, signature)
            .unwrap_or_else(|e| panic!("RFC 8032 用例 {} 验签必须过: {e}", idx + 1));
    }
    // 篡改消息或签名的任何一位,严格验签必须拒。
    let (_, public, message, signature) = &cases[2];
    let mut bad_message = message.clone();
    bad_message[0] ^= 0x01;
    assert!(verify_ed25519_hex(public, &bad_message, signature).is_err());
    let tampered_sig = signature[..signature.len() - 2].to_string() + "00";
    assert!(
        verify_ed25519_hex(public, message, &tampered_sig).is_err(),
        "改签名末位必须拒"
    );
}

// ---------------------------------------------------------------------------
// v2 签出/验回(公钥随锚点走,验签不需要任何密钥文件)
// ---------------------------------------------------------------------------

#[test]
fn sign_v2_then_verify_v2_roundtrip_no_key_file_needed() {
    let wal = tmp_path("wal", "jsonl");
    build_sample_wal(&wal);
    let (_seed_path, seed) = seed_file(SEED_HEX_BOSS);
    let anchor_path = tmp_path("anchor-v2", "json");

    let file = sign_v2(&wal, &seed, 1_700_010_000, &anchor_path).expect("签出");
    assert_eq!(file.schema, ANCHOR_SCHEMA_V2);
    assert_eq!(file.version, 2, "v2 文件显式带 version 字段");
    assert_eq!(file.lines, 3);
    assert_eq!(file.public_key_hex.len(), 64, "公钥 32 字节 hex");
    assert_eq!(file.signature_hex.len(), 128, "签名 64 字节 hex");
    assert_eq!(
        file.public_key_hex,
        public_key_from_seed_hex(SEED_HEX_BOSS).expect("推公钥"),
        "文件里的公钥 = 种子推导的公钥"
    );

    // 落盘形态可读回、带换行结尾。
    let text = std::fs::read_to_string(&anchor_path).expect("锚点文件存在");
    assert!(text.ends_with('\n'));
    assert!(text.contains("\"version\": 2"));

    // 验签**零密钥**:公钥随锚点走。
    let outcome = verify_v2(&wal, &anchor_path, None).expect("无密钥验得过");
    assert_eq!(outcome.anchored_lines, 3);
    assert_eq!(outcome.current_lines, 3);
    assert_eq!(outcome.public_key_hex, file.public_key_hex);
}

#[test]
fn sign_v2_is_deterministic_and_seed_bound() {
    let wal = tmp_path("wal", "jsonl");
    build_sample_wal(&wal);
    let (_a, seed) = seed_file(SEED_HEX_BOSS);
    let out_a = tmp_path("anchor-a", "json");
    let out_b = tmp_path("anchor-b", "json");
    let a = sign_v2(&wal, &seed, 42, &out_a).expect("签 A");
    let b = sign_v2(&wal, &seed, 42, &out_b).expect("签 B");
    assert_eq!(a, b, "同种子同材料同时刻 → 字节级同锚点");

    let (_c, attacker) = seed_file(SEED_HEX_ATTACKER);
    let c = sign_v2(&wal, &attacker, 42, &tmp_path("anchor-c", "json")).expect("签 C");
    assert_ne!(a.public_key_hex, c.public_key_hex, "换种子必须换公钥");
    assert_ne!(a.signature_hex, c.signature_hex, "换种子必须换签名");
}

#[test]
fn v2_payload_commits_to_public_key_material_and_time() {
    use wanning_core::anchor::material_from_records;
    use wanning_demo::anchor_v2::canonical_payload_v2;
    let wal = tmp_path("wal", "jsonl");
    build_sample_wal(&wal);
    let verified = wanning_core::wal::read_verified(&wal).expect("读账");
    let material = material_from_records(&verified.records).expect("材料");

    let p = canonical_payload_v2(&material, 42, "ab".repeat(32).as_str());
    assert_eq!(
        p,
        format!(
            "WANNING-ANCHOR-v2\nlines=3\nchain_tail=0x{:016x}\nrecords_sha256={}\n\
             anchored_at_unix=42\npublic_key={}",
            material.chain_tail,
            wanning_core::sha256::hex(&material.records_sha256),
            "ab".repeat(32)
        ),
        "载荷规范格式逐字节钉死(签名两端共享)"
    );
    // 公钥属于被签内容:换公钥不改签名,验签当场现形。
    assert_ne!(
        p,
        canonical_payload_v2(&material, 42, "cd".repeat(32).as_str()),
        "载荷必须提交到公钥"
    );
    assert_ne!(
        p,
        canonical_payload_v2(&material, 43, "ab".repeat(32).as_str()),
        "anchored_at 变了载荷必须变"
    );
}

// ---------------------------------------------------------------------------
// fail-closed:伪造锚点 / 换公钥 / 截尾 / 改尾行
// ---------------------------------------------------------------------------

#[test]
fn verify_v2_fails_on_forged_anchor_fields() {
    let wal = tmp_path("wal", "jsonl");
    build_sample_wal(&wal);
    let (_p, seed) = seed_file(SEED_HEX_BOSS);
    let anchor_path = tmp_path("anchor-v2", "json");
    sign_v2(&wal, &seed, 1_700_010_000, &anchor_path).expect("签出");

    // 伪造:改声明字段但签名还是旧的 → 验签不过。
    let forged_path = tmp_path("anchor-forged", "json");
    let mut file: AnchorFileV2 =
        serde_json::from_str(&std::fs::read_to_string(&anchor_path).expect("读锚点"))
            .expect("锚点是 JSON");
    file.lines = 99;
    std::fs::write(
        &forged_path,
        serde_json::to_string_pretty(&file).expect("序列化"),
    )
    .expect("写伪造锚点");
    let err = verify_v2(&wal, &forged_path, None).unwrap_err();
    assert!(
        matches!(err, CoreError::AnchorInvalid(ref m) if m.contains("签名")),
        "伪造锚点要点名签名不符: {err}"
    );

    // 锚点缺文件/坏 JSON/坏 schema,同样 fail-closed。
    assert!(verify_v2(&wal, &tmp_path("nope", "json"), None).is_err());
    let bad = tmp_path("anchor-bad", "json");
    std::fs::write(&bad, "{ not json").expect("写坏 JSON");
    assert!(verify_v2(&wal, &bad, None).is_err());
}

#[test]
fn verify_v2_fails_when_public_key_swapped_and_pin_catches_it() {
    let wal = tmp_path("wal", "jsonl");
    build_sample_wal(&wal);
    let (_p, boss) = seed_file(SEED_HEX_BOSS);
    let anchor_path = tmp_path("anchor-v2", "json");
    let legit = sign_v2(&wal, &boss, 1_700_010_000, &anchor_path).expect("签出");

    // 攻击者:换上自己的公钥并重签(载荷含攻击者公钥 → 内部自洽)。
    let (_ap, attacker) = seed_file(SEED_HEX_ATTACKER);
    let attacker_file = sign_v2(
        &wal,
        &attacker,
        legit.anchored_at_unix,
        &tmp_path("x", "json"),
    )
    .expect("攻击者重签");
    let swapped_path = tmp_path("anchor-swapped", "json");
    std::fs::write(
        &swapped_path,
        serde_json::to_string_pretty(&attacker_file).expect("序列化"),
    )
    .expect("写换钥锚点");

    // **诚实边界**:不钉期望公钥时,内部自洽的换钥锚点验得过——非对称签名
    // 只证明「持钥者签的」,不证明「持钥者是所有者」;身份绑定在带外
    // (第三方从所有者公开渠道核对公钥)。
    assert!(
        verify_v2(&wal, &swapped_path, None).is_ok(),
        "无钉定时换钥锚点内部自洽(边界如实落测试)"
    );
    // 钉了带外核对过的公钥 → 换公钥当场现形(fail-closed)。
    let err = verify_v2(&wal, &swapped_path, Some(&legit.public_key_hex)).unwrap_err();
    assert!(
        matches!(err, CoreError::AnchorInvalid(ref m) if m.contains("公钥")),
        "换公钥要点名公钥不符: {err}"
    );
    // 钉对公钥 → 正常锚点照常过。
    assert!(verify_v2(&wal, &anchor_path, Some(&legit.public_key_hex)).is_ok());

    // 只换公钥不换签名(签名还是所有者的)→ 验签不过。
    let mut key_only: AnchorFileV2 =
        serde_json::from_str(&std::fs::read_to_string(&anchor_path).expect("读锚点"))
            .expect("锚点是 JSON");
    key_only.public_key_hex = attacker_file.public_key_hex.clone();
    let key_only_path = tmp_path("anchor-keyonly", "json");
    std::fs::write(
        &key_only_path,
        serde_json::to_string_pretty(&key_only).expect("序列化"),
    )
    .expect("写");
    assert!(
        verify_v2(&wal, &key_only_path, None).is_err(),
        "只换公钥(签名还是原签)→ 载荷对不上,验签必须拒"
    );
}

#[test]
fn verify_v2_fails_on_truncation_and_tail_tamper() {
    let wal = tmp_path("wal", "jsonl");
    build_sample_wal(&wal);
    let (_p, seed) = seed_file(SEED_HEX_BOSS);
    let anchor_path = tmp_path("anchor-v2", "json");
    sign_v2(&wal, &seed, 1_700_010_000, &anchor_path).expect("签出");

    // 截尾:删掉最后一行。
    let truncated = tmp_path("wal-truncated", "jsonl");
    let lines = wanning_core::wal::raw_lines(&wal).expect("读");
    std::fs::write(&truncated, lines[..2].join("\n") + "\n").expect("写截尾副本");
    let err = verify_v2(&truncated, &anchor_path, None).unwrap_err();
    assert!(
        matches!(err, CoreError::AnchorMismatch(ref m) if m.contains("截尾")),
        "截尾要现形: {err}"
    );

    // 改尾行 memo:W-21 完整性链的盲区,锚点抓住。
    let tail_tampered = tmp_path("wal-tail", "jsonl");
    let mut lines = lines.clone();
    let mut value: serde_json::Value = serde_json::from_str(&lines[2]).expect("行是 JSON");
    value["rec"]["intent"]["memo"] = serde_json::json!("尾行被改写");
    lines[2] = value.to_string();
    std::fs::write(&tail_tampered, lines.join("\n") + "\n").expect("写改尾副本");
    let err = verify_v2(&tail_tampered, &anchor_path, None).unwrap_err();
    assert!(
        matches!(err, CoreError::AnchorMismatch(ref m) if m.contains("被改")),
        "改尾行要现形(W-21 盲区): {err}"
    );
}

#[test]
fn verify_v2_reports_chain_break_before_anchor_comparison() {
    let wal = tmp_path("wal", "jsonl");
    build_sample_wal(&wal);
    let (_p, seed) = seed_file(SEED_HEX_BOSS);
    let anchor_path = tmp_path("anchor-v2", "json");
    sign_v2(&wal, &seed, 1_700_010_000, &anchor_path).expect("签出");

    let tampered = tmp_path("wal-mid", "jsonl");
    let mut lines = wanning_core::wal::raw_lines(&wal).expect("读");
    let mut value: serde_json::Value = serde_json::from_str(&lines[1]).expect("行是 JSON");
    value["rec"]["intent"]["memo"] = serde_json::json!("中间行被改写");
    lines[1] = value.to_string();
    std::fs::write(&tampered, lines.join("\n") + "\n").expect("写");
    let err = verify_v2(&tampered, &anchor_path, None).unwrap_err();
    assert!(
        matches!(err, CoreError::WalChainBroken { line: 3, .. }),
        "中间行被动过,完整性链先现形(轮不到锚点比对): {err}"
    );
}

// ---------------------------------------------------------------------------
// 向后兼容:v1(HMAC)保留,v1/v2 文件都带 version
// ---------------------------------------------------------------------------

#[test]
fn v1_hmac_anchor_survives_with_version_field_no_byte_drift() {
    let wal = tmp_path("wal", "jsonl");
    build_sample_wal(&wal);
    let key_path = tmp_path("key", "hex");
    std::fs::write(&key_path, format!("{SEED_HEX_BOSS}\n")).expect("写密钥文件");
    let key = AnchorKey::from_hex_file(&key_path).expect("读密钥");
    let anchor_path = tmp_path("anchor-v1", "json");
    let file =
        wanning_demo::anchor_cmd::sign(&wal, &key, 1_700_010_000, &anchor_path).expect("v1 签出");
    assert_eq!(file.schema, ANCHOR_SCHEMA);
    assert_eq!(file.version, 1, "v1 文件的 version = 1(内存表示)");

    // 字节不漂移:新签 v1 的 JSON **不落** version 字段(既有 v1 文件格式不变)。
    let text = std::fs::read_to_string(&anchor_path).expect("读");
    assert!(
        !text.contains("version"),
        "v1 落盘 JSON 不得出现 version 字段(既有文件字节不漂移): {text}"
    );

    // 旧 v1 文件(无 version 字段)照常解析验签;显式 "version": 1 也收。
    let outcome = wanning_demo::anchor_cmd::verify(&wal, &anchor_path, &key).expect("v1 验得过");
    assert_eq!(outcome.anchored_lines, 3);
    let with_version: AnchorFile = serde_json::from_str(&text.replace(
        "\"schema\": \"wanning-anchor-v1\"",
        "\"version\": 1,\n  \"schema\": \"wanning-anchor-v1\"",
    ))
    .expect("显式 version: 1 的 v1 文件可解析");
    assert_eq!(with_version.version, 1);
}

#[test]
fn verify_v2_rejects_v1_schema_and_unknown_schema() {
    let wal = tmp_path("wal", "jsonl");
    build_sample_wal(&wal);
    let v1_like = tmp_path("anchor-v1-like", "json");
    std::fs::write(
        &v1_like,
        r#"{"schema":"wanning-anchor-v1","lines":3,"chain_tail_hex":"0x0000000000000000",
            "records_sha256_hex":"00","anchored_at_unix":1,"mac_hex":"00"}"#,
    )
    .expect("写 v1 形态锚点");
    let err = verify_v2(&wal, &v1_like, None).unwrap_err();
    assert!(
        matches!(err, CoreError::AnchorInvalid(ref m) if m.contains("v1")),
        "v1 是对称锚点要密钥,报错要指路: {err}"
    );

    let unknown = tmp_path("anchor-unknown", "json");
    std::fs::write(&unknown, r#"{"schema":"wanning-anchor-v9"}"#).expect("写");
    assert!(verify_v2(&wal, &unknown, None).is_err(), "未知 schema 不猜");
}

// ---------------------------------------------------------------------------
// 种子文件纪律
// ---------------------------------------------------------------------------

#[test]
fn seed_file_parsing_is_strict_and_masked() {
    let err = Ed25519Seed::from_hex_file(Path::new("D:/definitely/not/here.key")).unwrap_err();
    assert!(err.contains("读种子文件"), "{err}");
    let short = tmp_path("seed-short", "hex");
    std::fs::write(&short, "ab".repeat(31)).expect("写");
    assert!(Ed25519Seed::from_hex_file(&short)
        .unwrap_err()
        .contains("64 位十六进制"));
    let (_p, seed) = seed_file(SEED_HEX_BOSS);
    assert!(!format!("{seed:?}").contains("aa11"), "Debug 必须打码");
    assert!(public_key_from_seed_hex("zz").is_err(), "非十六进制拒");
}

// ---------------------------------------------------------------------------
// CLI --anchor-sign-v2(所有者侧签出)端到端
// ---------------------------------------------------------------------------

#[test]
fn cli_anchor_sign_v2_produces_verifiable_anchor() {
    let wal = tmp_path("wal", "jsonl");
    build_sample_wal(&wal);
    let (seed_path, seed) = seed_file(SEED_HEX_BOSS);
    let anchor_path = tmp_path("anchor-cli", "json");

    let output = Command::new(env!("CARGO_BIN_EXE_wanning-demo"))
        .arg("--anchor-sign-v2")
        .arg(&wal)
        .arg("--seed")
        .arg(&seed_path)
        .arg("--out")
        .arg(&anchor_path)
        .output()
        .expect("spawn wanning-demo");
    assert!(output.status.success(), "签出必须成功: {:?}", output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(stdout.contains("公钥(hex):"), "回执带公钥: {stdout}");
    assert!(
        stdout.contains("wanning-anchor-verify"),
        "回执指路第三方验法: {stdout}"
    );

    // CLI 产出的锚点能被库面/验签 bin 验过(同一格式)。
    let outcome = verify_v2(&wal, &anchor_path, None).expect("CLI 产出的锚点验得过");
    assert_eq!(outcome.anchored_lines, 3);
    assert_eq!(
        outcome.public_key_hex,
        public_key_from_seed_hex(SEED_HEX_BOSS).expect("推公钥")
    );

    // 用法纪律:v2 签名误带 --key(v1 的密钥)→ 拒;缺 --seed → 拒。
    let output = Command::new(env!("CARGO_BIN_EXE_wanning-demo"))
        .arg("--anchor-sign-v2")
        .arg(&wal)
        .arg("--key")
        .arg(&seed_path)
        .arg("--out")
        .arg(&anchor_path)
        .output()
        .expect("spawn");
    assert!(!output.status.success(), "--key 不随 v2 使用");
    let output = Command::new(env!("CARGO_BIN_EXE_wanning-demo"))
        .arg("--anchor-sign-v2")
        .arg(&wal)
        .arg("--out")
        .arg(&anchor_path)
        .output()
        .expect("spawn");
    assert!(!output.status.success(), "缺 --seed 必拒");
    let _ = seed;
}

// ---------------------------------------------------------------------------
// 第三方验签 bin(无密钥文件)端到端
// ---------------------------------------------------------------------------

#[test]
fn wanning_anchor_verify_bin_end_to_end_without_any_key() {
    let wal = tmp_path("wal", "jsonl");
    build_sample_wal(&wal);
    let (_p, seed) = seed_file(SEED_HEX_BOSS);
    let anchor_path = tmp_path("anchor-v2", "json");
    let legit = sign_v2(&wal, &seed, 1_700_010_000, &anchor_path).expect("签出");
    let bin = env!("CARGO_BIN_EXE_wanning-anchor-verify");

    // ① 第三方只拿锚点 + WAL(零密钥文件)→ 验过,exit 0,回执带公钥与行数。
    let output = Command::new(bin)
        .arg("--anchor")
        .arg(&anchor_path)
        .arg("--wal")
        .arg(&wal)
        .output()
        .expect("spawn 验签 bin");
    assert!(
        output.status.success(),
        "验过必须 exit 0: {:?}",
        output.stdout
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.contains(&legit.public_key_hex),
        "回执带公钥: {stdout}"
    );
    assert!(stdout.contains("3"), "回执带锚定行数: {stdout}");

    // ② 钉了带外核对过的公钥 → 正常锚点照常过。
    let output = Command::new(bin)
        .arg("--anchor")
        .arg(&anchor_path)
        .arg("--wal")
        .arg(&wal)
        .arg("--expect-key")
        .arg(&legit.public_key_hex)
        .output()
        .expect("spawn");
    assert!(
        output.status.success(),
        "钉对公钥必须过: {:?}",
        output.stderr
    );

    // ③ 截尾副本 → exit 非零,报错说清原因。
    let truncated = tmp_path("wal-truncated", "jsonl");
    let lines = wanning_core::wal::raw_lines(&wal).expect("读");
    std::fs::write(&truncated, lines[..2].join("\n") + "\n").expect("写截尾副本");
    let output = Command::new(bin)
        .arg("--anchor")
        .arg(&anchor_path)
        .arg("--wal")
        .arg(&truncated)
        .output()
        .expect("spawn");
    assert!(!output.status.success(), "截尾必须非零退出");
    assert!(
        !output.stdout.is_empty() || !output.stderr.is_empty(),
        "报错要可见"
    );

    // ④ 换钥锚点 + 钉真公钥 → 非零退出(fail-closed)。
    let (_ap, attacker) = seed_file(SEED_HEX_ATTACKER);
    let swapped = sign_v2(
        &wal,
        &attacker,
        legit.anchored_at_unix,
        &tmp_path("sw", "json"),
    )
    .expect("攻击者重签");
    let swapped_path = tmp_path("anchor-swapped", "json");
    std::fs::write(
        &swapped_path,
        serde_json::to_string_pretty(&swapped).expect("序列化"),
    )
    .expect("写");
    let output = Command::new(bin)
        .arg("--anchor")
        .arg(&swapped_path)
        .arg("--wal")
        .arg(&wal)
        .arg("--expect-key")
        .arg(&legit.public_key_hex)
        .output()
        .expect("spawn");
    assert!(!output.status.success(), "换钥+钉定必须非零退出");

    // ⑤ 缺 --wal / 缺 --anchor → 用法报错非零退出(零密钥参数,无 --key 选项)。
    let output = Command::new(bin)
        .arg("--anchor")
        .arg(&anchor_path)
        .output()
        .expect("spawn");
    assert!(!output.status.success());
    let help = Command::new(bin).arg("--help").output().expect("spawn");
    let help_text =
        String::from_utf8_lossy(&help.stdout).to_string() + &String::from_utf8_lossy(&help.stderr);
    assert!(
        help_text.contains("--wal"),
        "用法说明要提 --wal: {help_text}"
    );
    assert!(
        !help_text.contains("--key "),
        "验签 bin 不得有 --key 选项(第三方无密钥)"
    );
    // 行为级:传密钥参数必须被拒(未知参数,非零退出)——第三方工具面零密钥。
    let with_key = Command::new(bin)
        .arg("--anchor")
        .arg(&anchor_path)
        .arg("--wal")
        .arg(&wal)
        .arg("--key")
        .arg("whatever")
        .output()
        .expect("spawn");
    assert!(!with_key.status.success(), "--key 必须是未知参数并失败");
}
