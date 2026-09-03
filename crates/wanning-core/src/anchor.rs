//! 审计锚点(W-23):**所有者侧密钥**锚住 WAL 前缀,堵住完整性链的已知边界。
//!
//! W-21 的完整性链有一个诚实声明过的盲区(见 [`crate::wal`] 模块注释):最后一行
//! 没有后继行引用它——**只改尾行内容**或**整体截尾**,链自身验不住。本模块给的
//! 抓法:所有者用**自己的密钥**(不在任何 Wanning 进程手里)对「前 N 行内容 + 行数 +
//! 链尾」算一个 HMAC-SHA256,锚点文件另行保管;此后任何时候再验——
//!
//! - 截尾 → 当前 WAL 行数 < 锚点行数,当场现形;
//! - 改尾行(或前缀任何一行)→ 前 N 行内容哈希对不上锚点,当场现形;
//! - 锚定后又合法追加了新行 → 前缀哈希不变,照常通过(锚的是前缀,不是整个文件)。
//!
//! **为什么这里用 HMAC 而 W-21 明说不用的那一个**:W-21 反对的是「密钥在写进程
//! 手里」——写进程(agent)既能写日志又能算 MAC,防线形同虚设。锚点的密钥在
//! **所有者**手里,写进程从头到尾接触不到;签名动作(`--anchor-sign`)是所有者拿着
//! 密钥文件在闸外做的,闸的 MCP 工具面永不提供锚点能力(agent 能签就能伪造锚点)。
//! 密钥保管是人的程序,和真实消费护栏(W-07)同一个信任边界。
//!
//! **内容哈希为什么是 SHA-256 而不是复用 FNV 链**:FNV-1a64 是确定性对账用的
//! 非密码学哈希(64 位空间谈不上抗碰撞),对手方是「能写盘的 agent」时必须用
//! 密码学哈希。锚点载荷里同时带上 FNV 链尾(人能和 W-22 回放页肉眼对账)与
//! SHA-256 内容哈希(密码学强度),两者各司其职。
//!
//! **诚实边界(已由 v2 升级收口)**:HMAC 的验证方需要密钥——v1 锚点只有所有者
//! (持密钥者)能验。W-31 起新增 **锚点 v2 = ed25519 非对称签名,公钥随锚点走,
//! 第三方零密钥即可验**(独立 bin `wanning-anchor-verify`,见 demo crate 的
//! `anchor_v2` 模块);本模块的 HMAC v1 模式保留(向后兼容,锚点文件带 version
//! 字段)。v2 复用本模块的材料/载荷纪律与 `assert_wal_matches_anchor` 比对。
//!
//! 验证顺序即 fail-closed 顺序:先验锚点本身可信([`verify_anchor_file`]:MAC 对
//! 不上 = 锚点不是所有者签的或锚点被改),再读 WAL 验完整性链,最后逐字段比对
//! ([`assert_material_matches`])。任何一步不过都报错,绝不给出「部分通过」。

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::sha256::{hex, sha256};
use crate::wal::{chain_value, WalRecord};

/// 锚点文件 schema 名;版本变了就不认(fail-closed,不猜)。
pub const ANCHOR_SCHEMA: &str = "wanning-anchor-v1";

/// HMAC-SHA256 的块长(RFC 2104)。
const HMAC_BLOCK: usize = 64;

/// HMAC-SHA256(RFC 2104)。密钥任意长度:> 块长先哈希,不足块长补零。
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; HMAC_BLOCK];
    if key.len() > HMAC_BLOCK {
        block[..32].copy_from_slice(&sha256(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Vec::with_capacity(HMAC_BLOCK + message.len());
    for byte in &block {
        inner.push(byte ^ 0x36);
    }
    inner.extend_from_slice(message);
    let inner_hash = sha256(&inner);

    let mut outer = Vec::with_capacity(HMAC_BLOCK + 32);
    for byte in &block {
        outer.push(byte ^ 0x5c);
    }
    outer.extend_from_slice(&inner_hash);
    sha256(&outer)
}

/// 锚定的材料:被锚住的 WAL 前缀的指纹。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorMaterial {
    /// 被锚住的行数(锚点声明「这个 WAL 至少有过这么多行」)。
    pub lines: u64,
    /// 第 `lines` 行的 FNV 完整性链值(与 W-22 回放页展示的链尾肉眼可对)。
    pub chain_tail: u64,
    /// 前 `lines` 行 `rec` 规范 JSON(逐行各接一个换行)的 SHA-256——密码学强度
    /// 的内容指纹,不依赖 FNV 的抗碰撞性。
    pub records_sha256: [u8; 32],
}

/// 从逐行记录([`crate::wal::read_verified`] 的产出,或其前缀)独立重算锚定材料。
///
/// 链尾用与写侧完全相同的口径重算([`chain_value`]),不照抄调用方给的值——
/// 锚点材料的每一项都是自己算的,这是「读侧独立重算」纪律的延续。
pub fn material_from_records(records: &[(u64, WalRecord)]) -> Result<AnchorMaterial, CoreError> {
    let mut chain = 0u64;
    let mut content = Vec::new();
    for (line_no, record) in records {
        let rec_json = serde_json::to_string(record)
            .map_err(|e| CoreError::AnchorInvalid(format!("记录序列化失败: {e}")))?;
        chain = chain_value(chain, *line_no, &rec_json);
        content.extend_from_slice(rec_json.as_bytes());
        content.push(b'\n');
    }
    Ok(AnchorMaterial {
        lines: records.len() as u64,
        chain_tail: chain,
        records_sha256: sha256(&content),
    })
}

/// 锚点载荷的规范序列化。**手写不用 serde**:载荷是被签名的对象,它的字节必须
/// 由我们逐字定义(serde 的键序/转义策略不属于本约定),签名与验证两端才有
/// 同一份字节。换格式必须换 [`ANCHOR_SCHEMA`] 版本号。
pub fn canonical_payload(material: &AnchorMaterial, anchored_at_unix: u64) -> String {
    format!(
        "WANNING-ANCHOR-v1\n\
         lines={}\n\
         chain_tail=0x{:016x}\n\
         records_sha256={}\n\
         anchored_at_unix={}",
        material.lines,
        material.chain_tail,
        hex(&material.records_sha256),
        anchored_at_unix
    )
}

/// 锚点文件(落盘形态)。MAC 由所有者密钥对 [`canonical_payload`] 计算。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnchorFile {
    pub schema: String,
    /// 显式版本号(W-31 起加字段;缺省 = 1,序列化时省略——既有 v1 文件字节
    /// 不漂移,旧文件(无此字段)也照常解析)。v2(ed25519)文件显式落 `"version": 2`。
    #[serde(default = "default_anchor_version")]
    #[serde(skip_serializing_if = "AnchorFile::version_is_implicit")]
    pub version: u32,
    pub lines: u64,
    /// `0x` + 16 位十六进制(FNV 链尾)。
    pub chain_tail_hex: String,
    /// 64 位十六进制(前缀内容 SHA-256)。
    pub records_sha256_hex: String,
    pub anchored_at_unix: u64,
    /// 64 位十六进制(HMAC-SHA256)。
    pub mac_hex: String,
}

fn default_anchor_version() -> u32 {
    1
}

impl AnchorFile {
    /// version == 1 是既有 v1 格式的隐含值,序列化时省略(字节不漂移)。
    fn version_is_implicit(version: &u32) -> bool {
        *version == 1
    }
}

/// 签出锚点文件。纯函数:同一材料 + 同一密钥 + 同一时刻 → 字节级同一锚点。
pub fn sign_anchor(material: &AnchorMaterial, key: &[u8; 32], anchored_at_unix: u64) -> AnchorFile {
    let payload = canonical_payload(material, anchored_at_unix);
    let mac = hmac_sha256(key, payload.as_bytes());
    AnchorFile {
        schema: ANCHOR_SCHEMA.to_string(),
        version: 1,
        lines: material.lines,
        chain_tail_hex: format!("0x{:016x}", material.chain_tail),
        records_sha256_hex: hex(&material.records_sha256),
        anchored_at_unix,
        mac_hex: hex(&mac),
    }
}

/// 验锚点文件本身可信:MAC 与所有者密钥对得上,字段读得懂。
/// 通过则返回它声明的材料(交给 [`assert_material_matches`] 对 WAL)。
pub fn verify_anchor_file(file: &AnchorFile, key: &[u8; 32]) -> Result<AnchorMaterial, CoreError> {
    if file.schema != ANCHOR_SCHEMA {
        return Err(CoreError::AnchorInvalid(format!(
            "schema {:?} 不是 {:?}(版本不符不猜,换版要换验法)",
            file.schema, ANCHOR_SCHEMA
        )));
    }
    let records_sha256 = parse_hex_32(&file.records_sha256_hex)
        .map_err(|e| CoreError::AnchorInvalid(format!("records_sha256_hex 读不懂: {e}")))?;
    let chain_tail = parse_chain_tail(&file.chain_tail_hex)
        .map_err(|e| CoreError::AnchorInvalid(format!("chain_tail_hex 读不懂: {e}")))?;
    let material = AnchorMaterial {
        lines: file.lines,
        chain_tail,
        records_sha256,
    };
    let payload = canonical_payload(&material, file.anchored_at_unix);
    let claimed = parse_hex_32(&file.mac_hex)
        .map_err(|e| CoreError::AnchorInvalid(format!("mac_hex 读不懂: {e}")))?;
    let expected = hmac_sha256(key, payload.as_bytes());
    if !constant_time_eq(&expected, &claimed) {
        // 不泄露哪个字段「差一点」:MAC 不符就是整个锚点不可信。
        return Err(CoreError::AnchorInvalid(
            "锚点 MAC 与所有者密钥对不上——锚点不是所有者签的,或锚点文件被改过".to_string(),
        ));
    }
    Ok(material)
}

/// 对比「当前 WAL」与「锚点」。**前缀锚语义**:锚住的是前 `anchored.lines` 行——
/// 锚定后合法追加的新行不影响通过;截尾与改前缀内容都会在这里现形。
///
/// 前缀切片在本函数内部完成,不交给调用方:内容哈希是整段串联的 SHA-256,
/// 「先算全量哈希再比」对追加后的 WAL 恒不成立(测试实证过这个坑)——
/// 必须取**前 N 行**重算。调用方只需给 [`crate::wal::read_verified`] 的产出。
pub fn assert_wal_matches_anchor(
    records: &[(u64, WalRecord)],
    anchored: &AnchorMaterial,
) -> Result<(), CoreError> {
    if (records.len() as u64) < anchored.lines {
        return Err(CoreError::AnchorMismatch(format!(
            "整体截尾:当前 WAL 只有 {} 行,锚点声明 {} 行——锚定之后的行不见了",
            records.len(),
            anchored.lines
        )));
    }
    let actual = material_from_records(&records[..anchored.lines as usize])?;
    if actual.records_sha256 != anchored.records_sha256 {
        return Err(CoreError::AnchorMismatch(format!(
            "前 {} 行内容与锚点不符——被锚定的部分在锚定后被改过\
             (完整性链抓不住的尾行篡改/历史改写,锚点抓住了)",
            anchored.lines
        )));
    }
    if actual.chain_tail != anchored.chain_tail {
        // 前缀内容哈希一致而链尾不一致,只可能是 FNV 碰撞或材料构造出错;
        // 无论哪种都不是能放过去的事,如实报。
        return Err(CoreError::AnchorMismatch(format!(
            "链尾 0x{:016x} 与锚点声明的 0x{:016x} 不符(内容哈希一致而链尾不一致,\
             属状态异常,fail-closed)",
            actual.chain_tail, anchored.chain_tail
        )));
    }
    Ok(())
}

/// 定长比较(MAC 比对用):不因首个不同字节提前返回。长度不等直接 false
/// (长度不是秘密,MAC 恒为 32 字节)。
fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 解析 64 位十六进制为 32 字节(锚点密钥/哈希字段共用;`pub` 供 demo 的
/// 密钥文件加载复用同一套严格校验,不另抄一份)。
pub fn parse_hex_32(s: &str) -> Result<[u8; 32], String> {
    let s = s.trim();
    let bytes = parse_hex_bytes(s)?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|v: Vec<u8>| format!("需要 64 个十六进制字符(32 字节),实际 {} 字节", v.len()))?;
    Ok(arr)
}

fn parse_chain_tail(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let digits = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .ok_or_else(|| format!("缺少 0x 前缀: {s:?}"))?;
    if digits.len() != 16 {
        return Err(format!("需要 16 位十六进制,实际 {} 位", digits.len()));
    }
    u64::from_str_radix(digits, 16).map_err(|e| format!("十六进制解析失败: {e}"))
}

fn parse_hex_bytes(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("十六进制长度必须是偶数".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("位置 {i} 不是十六进制: {e}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4231 测试用例(HMAC-SHA256);向量另经本机 .NET `HMACSHA256`
    /// 逐条交叉核验(W-23 取证,真实运行输出)。
    /// 来源:<https://datatracker.ietf.org/doc/html/rfc4231>。
    #[test]
    fn rfc4231_test_cases() {
        let cases: Vec<(&[u8], &[u8], &str)> = vec![
            // TC1:key = 0x0b × 20,data = "Hi There"。
            (
                &[0x0b; 20],
                b"Hi There",
                "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
            ),
            // TC2:key = "Jefe"。
            (
                b"Jefe",
                b"what do ya want for nothing?",
                "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            ),
            // TC3:key = 0xaa × 20,data = 0xdd × 50。
            (
                &[0xaa; 20],
                &[0xdd; 50],
                "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe",
            ),
            // TC4:key = 0x01..0x19(25 字节),data = 0xcd × 50。
            (
                &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19],
                &[0xcd; 50],
                "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b",
            ),
            // TC6:key(131 字节)超过块长 → 先哈希再 pads。
            (
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First",
                "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
            ),
            // TC7:超块长 key + 超块长 data。
            (
                &[0xaa; 131],
                b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm.",
                "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2",
            ),
        ];
        for (idx, (key, message, expected)) in cases.iter().enumerate() {
            let actual = hex(&hmac_sha256(key, message));
            assert_eq!(&actual, expected, "RFC 4231 用例 {}", idx + 1);
        }
    }

    /// 边界:空密钥/空消息、恰好块长的密钥(向量为本机 .NET oracle 实算,W-23)。
    #[test]
    fn hmac_key_length_boundaries() {
        assert_eq!(
            hex(&hmac_sha256(b"", b"")),
            "b613679a0814d9ec772f95d778c35fc5ff1697c493715653c6c712144292c5ad",
            "空密钥+空消息(.NET oracle 实算)"
        );
        assert_eq!(
            hex(&hmac_sha256(&[0x3a; 64], b"exact block size key")),
            "a59ee14066ab0f880f654a760fbc54ebe0abcd27b31743e1a4e5378797470bb3",
            "恰好 64 字节密钥(.NET oracle 实算)"
        );
    }

    fn sample_records() -> Vec<(u64, WalRecord)> {
        use crate::delegation::Delegation;
        use crate::intent::SpendIntent;
        use crate::wal::WalDecision;
        let delegation = Delegation::new(
            "d1",
            "所有者",
            "agent-1",
            10_00,
            1_000,
            2_000,
            "wanning-test",
        );
        vec![
            (
                1,
                WalRecord::RegisterDelegation {
                    ts: 1_500,
                    delegation: delegation.clone(),
                },
            ),
            (
                2,
                WalRecord::Decide {
                    ts: 1_600,
                    decision: WalDecision::Allow,
                    delegation_id: "d1".into(),
                    intent: SpendIntent::new("d1", 1, 500, "jd:shop-1", "grocery", "测试意图"),
                    reason: None,
                    budget_after_cents: 500,
                },
            ),
        ]
    }

    #[test]
    fn material_is_independent_recompute() {
        // 材料重算与 WAL 写侧链尾同口径:两条记录后链尾非 0,行数/内容哈希就位。
        let material = material_from_records(&sample_records()).expect("材料");
        assert_eq!(material.lines, 2);
        assert_ne!(material.chain_tail, 0);
        // 空前缀:0 行、创世链 0、空内容哈希。
        let empty = material_from_records(&[]).expect("空材料");
        assert_eq!(empty.lines, 0);
        assert_eq!(empty.chain_tail, 0);
        assert_eq!(
            hex(&empty.records_sha256),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn content_hash_changes_on_any_record_change() {
        // 内容哈希是密码学强度的:改任何一个字节(比如 memo)必须变。
        let mut tampered = sample_records();
        if let WalRecord::Decide { intent, .. } = &mut tampered[1].1 {
            intent.memo = "测试意图".to_string() + "改";
        }
        let a = material_from_records(&sample_records()).expect("材料");
        let b = material_from_records(&tampered).expect("被改材料");
        assert_ne!(a.records_sha256, b.records_sha256, "改内容必须改哈希");
        assert_ne!(a.chain_tail, b.chain_tail, "链值同样变");
    }

    #[test]
    fn payload_is_stable_and_field_complete() {
        let material = material_from_records(&sample_records()).expect("材料");
        let payload = canonical_payload(&material, 1_700_000_000);
        // 逐行规范格式(签名两端共享的字节定义,写死在测试里)。
        let expected = format!(
            "WANNING-ANCHOR-v1\nlines=2\nchain_tail=0x{:016x}\nrecords_sha256={}\nanchored_at_unix=1700000000",
            material.chain_tail,
            hex(&material.records_sha256)
        );
        assert_eq!(payload, expected);
        // 同输入同输出;anchored_at 变了载荷必须变(锚点的时刻属于被签内容)。
        assert_eq!(payload, canonical_payload(&material, 1_700_000_000));
        assert_ne!(payload, canonical_payload(&material, 1_700_000_001));
    }

    #[test]
    fn sign_then_verify_roundtrip() {
        let material = material_from_records(&sample_records()).expect("材料");
        let key = [7u8; 32];
        let file = sign_anchor(&material, &key, 1_700_000_000);
        assert_eq!(file.schema, ANCHOR_SCHEMA);
        let verified = verify_anchor_file(&file, &key).expect("同密钥验得过");
        assert_eq!(verified, material);
    }

    #[test]
    fn sign_is_deterministic() {
        let material = material_from_records(&sample_records()).expect("材料");
        let key = [9u8; 32];
        let a = sign_anchor(&material, &key, 42);
        let b = sign_anchor(&material, &key, 42);
        assert_eq!(a, b, "同材料同密钥同时刻 → 同锚点");
        let c = sign_anchor(&material, &[10u8; 32], 42);
        assert_ne!(a, c, "换密钥锚点必须变");
    }

    #[test]
    fn verify_rejects_wrong_key() {
        let material = material_from_records(&sample_records()).expect("材料");
        let file = sign_anchor(&material, &[1u8; 32], 42);
        let err = verify_anchor_file(&file, &[2u8; 32]).unwrap_err();
        assert!(
            matches!(err, CoreError::AnchorInvalid(_)),
            "错密钥 = 锚点不可信: {err}"
        );
    }

    #[test]
    fn verify_rejects_tampered_fields() {
        let material = material_from_records(&sample_records()).expect("材料");
        let key = [3u8; 32];
        let file = sign_anchor(&material, &key, 42);

        let mut lines = file.clone();
        lines.lines = 3; // 行数被改 → 载荷变 → MAC 不符
        assert!(matches!(
            verify_anchor_file(&lines, &key),
            Err(CoreError::AnchorInvalid(_))
        ));

        let mut anchored_at = file.clone();
        anchored_at.anchored_at_unix = 43;
        assert!(matches!(
            verify_anchor_file(&anchored_at, &key),
            Err(CoreError::AnchorInvalid(_))
        ));

        let mut schema = file.clone();
        schema.schema = "wanning-anchor-v0".into();
        assert!(matches!(
            verify_anchor_file(&schema, &key),
            Err(CoreError::AnchorInvalid(_))
        ));

        let mut mac = file.clone();
        mac.mac_hex = "00".repeat(32);
        assert!(matches!(
            verify_anchor_file(&mac, &key),
            Err(CoreError::AnchorInvalid(_))
        ));
    }

    #[test]
    fn match_semantics_prefix_truncation_and_tamper() {
        let records = sample_records();
        let anchored = material_from_records(&records).expect("锚定材料");

        // 原样(或合法追加)→ 过。
        assert!(assert_wal_matches_anchor(&records, &anchored).is_ok());
        let mut grown = records.clone();
        grown.push((3, records[1].1.clone()));
        assert!(
            assert_wal_matches_anchor(&grown, &anchored).is_ok(),
            "锚定后追加新行,前缀锚照常通过"
        );

        // 截尾:当前只有 1 行 < 锚点 2 行 → 现形。
        let err = assert_wal_matches_anchor(&records[..1], &anchored).unwrap_err();
        assert!(
            matches!(err, CoreError::AnchorMismatch(ref m) if m.contains("截尾")),
            "截尾要点名截尾: {err}"
        );

        // 改被锚前缀的内容(哪怕行数够)→ 现形。
        let mut tampered = grown.clone();
        if let WalRecord::Decide { intent, .. } = &mut tampered[1].1 {
            intent.amount_cents = 999;
        }
        let err = assert_wal_matches_anchor(&tampered, &anchored).unwrap_err();
        assert!(
            matches!(err, CoreError::AnchorMismatch(ref m) if m.contains("被改")),
            "改前缀内容要现形: {err}"
        );
    }

    #[test]
    fn hex_parsing_is_strict() {
        assert!(parse_hex_32(&"ab".repeat(32)).is_ok());
        assert!(parse_hex_32(&"AB".repeat(32)).is_ok(), "大写也收");
        assert!(parse_hex_32(&"ab".repeat(31)).is_err(), "长度不足拒");
        assert!(parse_hex_32("zz").is_err(), "非十六进制拒");
        assert_eq!(parse_chain_tail("0x0000000000000000").unwrap(), 0);
        assert!(
            parse_chain_tail("0000000000000000").is_err(),
            "缺 0x 前缀拒"
        );
        assert!(parse_chain_tail("0x00").is_err(), "长度不对拒");
    }
}
