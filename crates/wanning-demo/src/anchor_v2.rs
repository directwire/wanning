//! 审计锚点 v2(W-31):**ed25519 第三方可验**——W-23 诚实边界的升级收口。
//!
//! v1(HMAC-SHA256,见 [`wanning_core::anchor`])的验证方 = 持密钥的所有者,
//! 第三方无法独立验证。v2 换成 ed25519 非对称签名:**公钥随锚点走**,第三方
//! 拿锚点 + WAL(零密钥文件)就能验——独立 bin `wanning-anchor-verify`。
//!
//! **依赖决策(A 案,落决策记录)**:ed25519 手写不可接受——
//! SHA-256 当年能手写(W-23)是因为 spec 短、测试向量密;曲线实现的小错误
//! (点校验/延展性/边角 case)是致命的,必须用经过大量实战检验的实现。
//! 引 `ed25519-dalek` 2.x——本仓第一个**运行时**外部加密依赖,只进 demo
//! 工具面(所有者侧 CLI);core/闸/MCP/SDK 依赖树零增长(锚点签名/验签不进
//! 闸的任何面,与 W-23「MCP 面永不提供锚点能力」同一信任边界)。
//!
//! **v2 格式**(`wanning-anchor-v2`,显式 `"version": 2`):
//!
//! ```json
//! {"schema":"wanning-anchor-v2","version":2,"lines":3,"chain_tail_hex":"0x…",
//!  "records_sha256_hex":"…","anchored_at_unix":1700000000,
//!  "public_key_hex":"…(32 字节)","signature_hex":"…(64 字节)"}
//! ```
//!
//! 被签消息 = [`canonical_payload_v2`](`WANNING-ANCHOR-v2` 头 + 材料 + 公钥)。
//! **公钥属于被签内容**:只换公钥不改签名,验签当场现形。
//!
//! **诚实边界(非对称签名解决不了的那一半)**:签名只证明「持对应私钥者签的」,
//! 不证明「持钥者是所有者」。攻击者可以换上自己的公钥重签(文件内部自洽)——
//! 这一步靠**带外身份绑定**堵:第三方从所有者公开渠道核对锚点里的公钥
//! (`--expect-key` 钉定;钉定后换钥当场 fail-closed)。不钉定时,内部自洽的
//! 换钥锚点验得过——这个边界写进了测试,不藏着。
//!
//! v1 HMAC 模式保留(向后兼容):v1 文件不带 version 字段 = 缺省 1(core 侧
//! `AnchorFile.version` 序列化省略,既有 v1 文件字节不漂移)。

use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use wanning_core::anchor::{self, material_from_records, AnchorMaterial};
use wanning_core::error::CoreError;

use crate::anchor_cmd::write_atomic;
use crate::audit_html::build_report;

/// v2 schema 名;版本变了就不认(fail-closed,不猜)。
pub const ANCHOR_SCHEMA_V2: &str = "wanning-anchor-v2";

/// v2 版本号(文件里显式落 `"version": 2`)。
pub const ANCHOR_VERSION_V2: u32 = 2;

/// ed25519 签名种子(32 字节 = ed25519 私钥的种子形态)。签名动作与 W-23
/// 同一信任边界:**所有者在闸外亲手做**,种子文件不在任何 Wanning 进程手里;
/// [`Debug`](std::fmt::Debug) 一律打码,密钥不进日志。
#[derive(Clone, PartialEq, Eq)]
pub struct Ed25519Seed([u8; 32]);

impl std::fmt::Debug for Ed25519Seed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ed25519Seed(**打码**)")
    }
}

impl Ed25519Seed {
    /// 从种子文件读:内容 trim 后必须恰好 64 位十六进制(32 字节)。
    pub fn from_hex_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("读种子文件 {} 失败(fail-closed): {e}", path.display()))?;
        Self::from_hex(&text).map_err(|e| {
            format!(
                "种子文件 {} 内容非法(要恰好 64 位十六进制 = 32 字节): {e}",
                path.display()
            )
        })
    }

    fn from_hex(text: &str) -> Result<Self, String> {
        let bytes = anchor::parse_hex_32(text.trim())?;
        Ok(Self(bytes))
    }
}

/// 锚点文件 v2(落盘形态)。签名由所有者种子对 [`canonical_payload_v2`] 计算。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnchorFileV2 {
    pub schema: String,
    /// 显式版本号:v2 必须是 2(读到别的值 = 版本不符不猜)。
    pub version: u32,
    pub lines: u64,
    /// `0x` + 16 位十六进制(FNV 链尾)。
    pub chain_tail_hex: String,
    /// 64 位十六进制(前缀内容 SHA-256)。
    pub records_sha256_hex: String,
    pub anchored_at_unix: u64,
    /// ed25519 公钥(32 字节 hex)。公钥随锚点走——第三方无需密钥文件。
    pub public_key_hex: String,
    /// ed25519 签名(64 字节 hex),对 [`canonical_payload_v2`]。
    pub signature_hex: String,
}

/// v2 被签消息的规范序列化。**手写不用 serde**(与 v1 [`wanning_core::anchor::
/// canonical_payload`] 同一纪律):载荷是被签名的对象,字节由我们逐字定义。
/// 与 v1 的差别:头是 `WANNING-ANCHOR-v2`,且多一行 `public_key=`——
/// 公钥属于被签内容。
pub fn canonical_payload_v2(
    material: &AnchorMaterial,
    anchored_at_unix: u64,
    public_key_hex: &str,
) -> String {
    format!(
        "WANNING-ANCHOR-v2\n\
         lines={}\n\
         chain_tail=0x{:016x}\n\
         records_sha256={}\n\
         anchored_at_unix={}\n\
         public_key={}",
        material.lines,
        material.chain_tail,
        wanning_core::sha256::hex(&material.records_sha256),
        anchored_at_unix,
        public_key_hex.to_ascii_lowercase(),
    )
}

/// 种子 → 公钥 hex(小写)。签出时打印/落文件用,也是 RFC 8032 向量的推导口径。
pub fn public_key_from_seed_hex(seed_hex: &str) -> Result<String, String> {
    let text = seed_hex.trim();
    let bytes = anchor::parse_hex_32(text)?;
    let key = SigningKey::from_bytes(&bytes);
    Ok(hex_64(&key.verifying_key().to_bytes()))
}

/// ed25519 验签(hex 层):第三方与 RFC 8032 向量的复现口径。
/// 严格验签(`verify_strict`):拒非规范 s(延展性)与小阶点——签名不唯一
/// 意味着「同锚点可再造一份签名」,审计锚点不要这种自由度。
pub fn verify_ed25519_hex(
    public_key_hex: &str,
    message: &[u8],
    signature_hex: &str,
) -> Result<(), String> {
    let key_bytes = parse_hex_bytes(public_key_hex).map_err(|e| format!("公钥 hex 读不懂: {e}"))?;
    let key_bytes: [u8; 32] = key_bytes
        .try_into()
        .map_err(|v: Vec<u8>| format!("公钥要 32 字节,实际 {} 字节", v.len()))?;
    let key =
        VerifyingKey::from_bytes(&key_bytes).map_err(|e| format!("公钥不是曲线上的合法点: {e}"))?;
    let sig_bytes = parse_hex_bytes(signature_hex).map_err(|e| format!("签名 hex 读不懂: {e}"))?;
    let sig_bytes: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|v: Vec<u8>| format!("签名要 64 字节,实际 {} 字节", v.len()))?;
    let signature = Signature::from_slice(&sig_bytes)
        .map_err(|e| format!("签名不是合法的 ed25519 签名字节: {e}"))?;
    key.verify_strict(message, &signature)
        .map_err(|e| format!("ed25519 验签不过: {e}"))
}

/// 签出锚点 v2:对账 → 材料 → ed25519 签名 → 原子落盘。
///
/// 对账先行([`build_report`]):坏账绝不签(与 v1 [`crate::anchor_cmd::sign`]
/// 同一纪律);空账同样拒。签名密钥 = 所有者的 32 字节种子文件,不在任何
/// Wanning 进程手里。
pub fn sign_v2(
    wal_path: &Path,
    seed: &Ed25519Seed,
    anchored_at_unix: u64,
    out_path: &Path,
) -> Result<AnchorFileV2, CoreError> {
    let report = build_report(wal_path)?;
    if report.rows.is_empty() {
        return Err(CoreError::AnchorInvalid(format!(
            "{} 是空账(0 行),无从锚起——确认 --wal 指向的是要锚的审计日志",
            wal_path.display()
        )));
    }
    let records: Vec<_> = report
        .rows
        .iter()
        .map(|row| (row.line_no, row.record.clone()))
        .collect();
    let material = material_from_records(&records)?;

    let mut seed_bytes = [0u8; 32];
    seed_bytes.copy_from_slice(&seed.0);
    let signing = SigningKey::from_bytes(&seed_bytes);
    let public_key_hex = hex_64(&signing.verifying_key().to_bytes());

    let payload = canonical_payload_v2(&material, anchored_at_unix, &public_key_hex);
    let signature = signing.sign(payload.as_bytes());

    let file = AnchorFileV2 {
        schema: ANCHOR_SCHEMA_V2.to_string(),
        version: ANCHOR_VERSION_V2,
        lines: material.lines,
        chain_tail_hex: format!("0x{:016x}", material.chain_tail),
        records_sha256_hex: wanning_core::sha256::hex(&material.records_sha256),
        anchored_at_unix,
        public_key_hex: public_key_hex.clone(),
        signature_hex: hex_64(&signature.to_bytes()),
    };
    let json = serde_json::to_string_pretty(&file)
        .map_err(|e| CoreError::AnchorInvalid(format!("锚点序列化失败: {e}")))?;
    write_atomic(out_path, (json + "\n").as_bytes())?;
    Ok(file)
}

/// v2 验签产出(打印给第三方/所有者看的对账结果)。
#[derive(Debug, PartialEq, Eq)]
pub struct VerifyOutcomeV2 {
    pub anchored_lines: u64,
    /// 当前 WAL 实际行数(≥ 锚点行数:锚定后合法追加不算事)。
    pub current_lines: u64,
    /// 当前 WAL 前 `anchored_lines` 行的链尾(应与锚点声明一致)。
    pub chain_tail: u64,
    pub records_sha256_hex: String,
    pub anchored_at_unix: u64,
    /// 锚点携带的公钥(打印出来,供第三方与带外渠道核对)。
    pub public_key_hex: String,
}

/// 验锚点 v2(**零密钥**):`expect_key_hex` = 第三方带外核对过的公钥
/// (可选;钉定后换公钥当场 fail-closed)。
///
/// 顺序即 fail-closed 顺序:版本/schema → 公钥钉定 → 签名 → WAL 完整性链 →
/// 前缀逐字段比对。任何一步不过都报错,绝不给出「部分通过」。
pub fn verify_v2(
    wal_path: &Path,
    anchor_path: &Path,
    expect_key_hex: Option<&str>,
) -> Result<VerifyOutcomeV2, CoreError> {
    let text = std::fs::read_to_string(anchor_path).map_err(|e| {
        CoreError::AnchorInvalid(format!("读锚点文件 {} 失败: {e}", anchor_path.display()))
    })?;
    let file: AnchorFileV2 = serde_json::from_str(&text).map_err(|e| {
        CoreError::AnchorInvalid(format!(
            "锚点文件 {} 不是 v2 锚点(解析失败: {e};v1 对称锚点需要密钥,\
             用 wanning-demo --anchor-verify)",
            anchor_path.display()
        ))
    })?;
    if file.schema != ANCHOR_SCHEMA_V2 || file.version != ANCHOR_VERSION_V2 {
        return Err(CoreError::AnchorInvalid(format!(
            "schema {:?}/version {} 不是 {:?}/{}(版本不符不猜;v1 是对称锚点,\
             需要密钥,用 wanning-demo --anchor-verify)",
            file.schema, file.version, ANCHOR_SCHEMA_V2, ANCHOR_VERSION_V2
        )));
    }

    // 带外身份钉定先行:期望公钥对不上,连签名都不必验——这把「换钥重签」
    // 挡在密码学之前。
    if let Some(expect) = expect_key_hex {
        let expect = expect.trim().to_ascii_lowercase();
        if expect != file.public_key_hex {
            return Err(CoreError::AnchorInvalid(format!(
                "锚点公钥 {} 与期望公钥(带外核对值)不匹配——公钥被换过,\
                 这个锚点不可信",
                file.public_key_hex
            )));
        }
    }

    let records_sha256 = anchor::parse_hex_32(&file.records_sha256_hex)
        .map_err(|e| CoreError::AnchorInvalid(format!("records_sha256_hex 读不懂: {e}")))?;
    let chain_tail = parse_chain_tail(&file.chain_tail_hex)
        .map_err(|e| CoreError::AnchorInvalid(format!("chain_tail_hex 读不懂: {e}")))?;
    let material = AnchorMaterial {
        lines: file.lines,
        chain_tail,
        records_sha256,
    };
    let payload = canonical_payload_v2(&material, file.anchored_at_unix, &file.public_key_hex);
    verify_ed25519_hex(
        &file.public_key_hex,
        payload.as_bytes(),
        &file.signature_hex,
    )
    .map_err(|e| {
        CoreError::AnchorInvalid(format!(
            "锚点签名与公钥对不上——锚点不是持钥者签的,或锚点文件被改过: {e}"
        ))
    })?;

    // 先验完整性链(中间行被改在这里就现形),再做前缀锚比对(尾行被改/截尾)。
    let verified = wanning_core::wal::read_verified(wal_path)?;
    anchor::assert_wal_matches_anchor(&verified.records, &material)?;
    let prefix_chain_tail = if material.lines == 0 {
        0
    } else {
        verified.links[material.lines as usize - 1].value
    };
    Ok(VerifyOutcomeV2 {
        anchored_lines: material.lines,
        current_lines: verified.records.len() as u64,
        chain_tail: prefix_chain_tail,
        records_sha256_hex: file.records_sha256_hex,
        anchored_at_unix: file.anchored_at_unix,
        public_key_hex: file.public_key_hex,
    })
}

fn hex_64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
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
    let s = s.trim();
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
