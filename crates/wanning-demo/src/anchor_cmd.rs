//! 所有者侧审计锚点命令(W-23):`--anchor-sign` / `--anchor-verify` 的管线。
//!
//! 分工:核心([`wanning_core::anchor`])只管纯语义(材料/载荷/HMAC/比对),
//! 这里管 IO——读密钥文件、读锚点文件、落盘锚点(**先写临时文件再原子改名**,
//! 与 W-22 [`crate::audit_html::export_audit`] 同一纪律:失败不留半个文件、
//! 不碰已有输出)、签名前先对账(复用 [`crate::audit_html::build_report`]:
//! 验完整性链 + 回放两遍,坏账绝不锚)。
//!
//! **密钥保管是人的程序**:签名密钥 = 所有者的 32 字节(64 位十六进制)文件,
//! 不在任何 Wanning 进程手里;`--anchor-sign` 没有它就是废铁(fail-closed),
//! Debug/日志一律打码。锚点文件要**另行保管**(不同目录/上传/打印),与 WAL
//! 分开——锚点若与 WAL 同放一处、都能被写进程改到,锚点就退化成自说自话。
//!
//! **验锚点也要密钥(HMAC 的诚实边界)**:核心提供的是对称锚点,验证方 = 持
//! 密钥的所有者;没有密钥的「只比内容不比 MAC」模式刻意不做——验不了真伪的
//! 通过等于给伪造的「WAL+锚点」对开绿灯,审计工具宁可少一种用法。

use std::path::Path;

use wanning_core::anchor::{self, AnchorFile, AnchorMaterial};
use wanning_core::error::CoreError;
use wanning_core::wal::WalRecord;

use crate::audit_html::build_report;

/// 签名/验签密钥(32 字节)。[`Debug`](std::fmt::Debug) 一律打码,
/// 与 [`crate::guard::RealSpendConfig`] 同一规矩:密钥不进日志。
#[derive(Clone, PartialEq, Eq)]
pub struct AnchorKey([u8; 32]);

impl std::fmt::Debug for AnchorKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AnchorKey(**打码**)")
    }
}

impl AnchorKey {
    /// 从密钥文件读:内容 trim 后必须是恰好 64 位十六进制(32 字节)。
    /// 太长/太短/非十六进制/文件缺失,一律 fail-closed 并说清缺什么。
    pub fn from_hex_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("读密钥文件 {} 失败(fail-closed): {e}", path.display()))?;
        let bytes = anchor::parse_hex_32(text.trim()).map_err(|e| {
            format!(
                "密钥文件 {} 内容非法(要恰好 64 位十六进制 = 32 字节): {e}",
                path.display()
            )
        })?;
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// 签出锚点:对账 → 材料 → HMAC → 原子落盘。
///
/// 对账先行([`build_report`]):完整性链断裂 / 回放对账不过的账,绝不签——
/// 给一份坏账签锚点等于替坏账背书。空账(0 行)同样拒:锚住「什么都没有」
/// 只会让所有者误以为账已被锚,多半是 `--wal` 指错了文件。
pub fn sign(
    wal_path: &Path,
    key: &AnchorKey,
    anchored_at_unix: u64,
    out_path: &Path,
) -> Result<AnchorFile, CoreError> {
    let report = build_report(wal_path)?;
    if report.rows.is_empty() {
        return Err(CoreError::AnchorInvalid(format!(
            "{} 是空账(0 行),无从锚起——确认 --wal 指向的是要锚的审计日志",
            wal_path.display()
        )));
    }
    let records: Vec<(u64, WalRecord)> = report
        .rows
        .iter()
        .map(|row| (row.line_no, row.record.clone()))
        .collect();
    let material = anchor::material_from_records(&records)?;
    let file = anchor::sign_anchor(&material, key.as_bytes(), anchored_at_unix);
    let json = serde_json::to_string_pretty(&file)
        .map_err(|e| CoreError::AnchorInvalid(format!("锚点序列化失败: {e}")))?;
    write_atomic(out_path, (json + "\n").as_bytes())?;
    Ok(file)
}

/// 验锚点的产出(打印给所有者看的对账结果)。
#[derive(Debug, PartialEq, Eq)]
pub struct VerifyOutcome {
    /// 锚点声明的行数。
    pub anchored_lines: u64,
    /// 当前 WAL 实际行数(≥ 锚点行数:锚定后合法追加不算事)。
    pub current_lines: u64,
    /// 当前 WAL 前 `anchored_lines` 行的链尾(应与锚点声明一致)。
    pub chain_tail: u64,
    /// 锚点声明的前缀内容 SHA-256(已重算比对通过)。
    pub records_sha256_hex: String,
    pub anchored_at_unix: u64,
}

/// 验锚点:锚点自身可信(MAC)→ WAL 完整性链 → 前缀逐字段比对。
///
/// 顺序即 fail-closed 顺序:锚点本身对不上所有者密钥,就不必读 WAL 了——
/// 拿不可信的参照物对账,结论没有意义。
pub fn verify(
    wal_path: &Path,
    anchor_path: &Path,
    key: &AnchorKey,
) -> Result<VerifyOutcome, CoreError> {
    let text = std::fs::read_to_string(anchor_path).map_err(|e| {
        CoreError::AnchorInvalid(format!("读锚点文件 {} 失败: {e}", anchor_path.display()))
    })?;
    let file: AnchorFile = serde_json::from_str(&text).map_err(|e| {
        CoreError::AnchorInvalid(format!("锚点文件 {} 解析失败: {e}", anchor_path.display()))
    })?;
    let material: AnchorMaterial = anchor::verify_anchor_file(&file, key.as_bytes())?;

    // 先验完整性链(中间行被改在这里就现形),再做前缀锚比对(尾行被改/截尾)。
    let verified = wanning_core::wal::read_verified(wal_path)?;
    anchor::assert_wal_matches_anchor(&verified.records, &material)?;
    // 打印用:当前 WAL 第 N 行的链值(锚定前缀的链尾;0 行锚点用创世值 0)。
    let prefix_chain_tail = if material.lines == 0 {
        0
    } else {
        verified.links[material.lines as usize - 1].value
    };
    Ok(VerifyOutcome {
        anchored_lines: material.lines,
        current_lines: verified.records.len() as u64,
        chain_tail: prefix_chain_tail,
        records_sha256_hex: file.records_sha256_hex,
        anchored_at_unix: file.anchored_at_unix,
    })
}

/// 先写临时文件再原子改名:任何一步失败,已有输出一个字节都不动,也不留
/// 临时文件(与 [`crate::audit_html::export_audit`] 同一纪律)。
/// `pub(crate)`:v2 锚点(W-31 `anchor_v2`)共用同一落盘纪律。
pub(crate) fn write_atomic(out_path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    let tmp_path = {
        let mut name = out_path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        name.push(".tmp");
        out_path.with_file_name(name)
    };
    std::fs::write(&tmp_path, bytes)
        .map_err(|e| CoreError::WalIo(format!("写锚点临时文件 {tmp_path:?} 失败: {e}")))?;
    if let Err(e) = std::fs::rename(&tmp_path, out_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(CoreError::WalIo(format!(
            "锚点改名 {tmp_path:?} → {out_path:?} 失败(已有输出未被改动): {e}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use wanning_core::anchor::ANCHOR_SCHEMA;
    use wanning_core::clock::MockClock;
    use wanning_core::delegation::Delegation;
    use wanning_core::intent::SpendIntent;
    use wanning_core::state::WanningState;

    /// 进程内原子序号:防同 tick 撞名(W-21 教训)。
    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn tmp_path(tag: &str, ext: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("wanning-demo-anchor-tests");
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间早于 Unix 纪元")
            .as_nanos();
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        dir.join(format!("{tag}-{nanos}-{seq}-{}.{ext}", std::process::id()))
    }

    /// 三行样本账:注册 → 放行 ¥5 → 超额拒。
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

    fn key_file(hex: &str) -> (PathBuf, AnchorKey) {
        let path = tmp_path("key", "hex");
        std::fs::write(&path, format!("{hex}\n")).expect("写密钥文件");
        let key = AnchorKey::from_hex_file(&path).expect("读密钥");
        (path, key)
    }

    const KEY_HEX_32B: &str = "a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5";
    const KEY_HEX_ALT: &str = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";

    #[test]
    fn sign_then_verify_roundtrip_through_files() {
        let wal = tmp_path("wal", "jsonl");
        build_sample_wal(&wal);
        let tail_at_sign = wanning_core::wal::read_verified(&wal).expect("读账").tail;
        let (_key_path, key) = key_file(KEY_HEX_32B);
        let anchor_path = tmp_path("anchor", "json");

        let file = sign(&wal, &key, 1_700_010_000, &anchor_path).expect("签出");
        assert_eq!(file.lines, 3);
        assert_eq!(file.schema, ANCHOR_SCHEMA);

        // 落盘形态可读回、带换行结尾(人能打开看)。
        let text = std::fs::read_to_string(&anchor_path).expect("锚点文件存在");
        assert!(text.ends_with('\n'));
        assert!(text.contains(&format!("\"lines\": {}", file.lines)));

        let outcome = verify(&wal, &anchor_path, &key).expect("验得过");
        assert_eq!(outcome.anchored_lines, 3);
        assert_eq!(outcome.current_lines, 3);
        assert_eq!(
            outcome.chain_tail, tail_at_sign,
            "打印用链尾 = 被锚前缀的链尾"
        );
    }

    #[test]
    fn verify_passes_after_legitimate_append() {
        // 锚定后合法追加:前缀锚不挡正常营业(锚定后续写一笔,真实系统时钟——
        // 样本委托早已过期,该意图按过期拒绝,拒绝行同样落审计,账继续长)。
        let wal = tmp_path("wal-append", "jsonl");
        build_sample_wal(&wal);
        let (_key_path, key) = key_file(KEY_HEX_32B);
        let anchor_path = tmp_path("anchor-append", "json");
        sign(&wal, &key, 1_700_010_000, &anchor_path).expect("签出");

        {
            let mut state = WanningState::live_resuming(&wal).expect("续写同一份账");
            state
                .decide(&SpendIntent::new(
                    "d1",
                    3,
                    100,
                    "jd:shop-1",
                    "grocery",
                    "锚定后的一笔(过期拒,但审计照落)",
                ))
                .expect("判定落账");
        } // 释放单写者锁,验锚点要重开读。

        let outcome = verify(&wal, &anchor_path, &key).expect("追加后仍验得过");
        assert_eq!(outcome.anchored_lines, 3);
        assert_eq!(outcome.current_lines, 4, "当前账比锚点长,正常");
    }

    #[test]
    fn verify_fails_closed_on_truncation_and_tail_tamper() {
        let wal = tmp_path("wal-tamper", "jsonl");
        build_sample_wal(&wal);
        let (_key_path, key) = key_file(KEY_HEX_32B);
        let anchor_path = tmp_path("anchor-tamper", "json");
        sign(&wal, &key, 1_700_010_000, &anchor_path).expect("签出");

        // 截尾:删掉最后一行。
        let truncated = tmp_path("wal-truncated", "jsonl");
        let lines = wanning_core::wal::raw_lines(&wal).expect("读");
        std::fs::write(&truncated, lines[..2].join("\n") + "\n").expect("写截尾副本");
        let err = verify(&truncated, &anchor_path, &key).unwrap_err();
        assert!(
            matches!(err, CoreError::AnchorMismatch(ref m) if m.contains("截尾")),
            "截尾要现形: {err}"
        );

        // 改尾行 memo:W-21 完整性链的盲区,锚点抓住——这是本功能的立身之本。
        let tail_tampered = tmp_path("wal-tail", "jsonl");
        let mut lines = lines.clone();
        let mut value: serde_json::Value = serde_json::from_str(&lines[2]).expect("行是 JSON");
        value["rec"]["intent"]["memo"] = serde_json::json!("尾行被改写");
        lines[2] = value.to_string();
        std::fs::write(&tail_tampered, lines.join("\n") + "\n").expect("写改尾副本");
        let err = verify(&tail_tampered, &anchor_path, &key).unwrap_err();
        assert!(
            matches!(err, CoreError::AnchorMismatch(ref m) if m.contains("被改")),
            "改尾行要现形(W-21 盲区): {err}"
        );
    }

    #[test]
    fn verify_reports_chain_break_before_anchor_comparison() {
        // 改中间行:完整性链先拒(轮不到锚点比对),报错要指到行。
        let wal = tmp_path("wal-mid", "jsonl");
        build_sample_wal(&wal);
        let (_key_path, key) = key_file(KEY_HEX_32B);
        let anchor_path = tmp_path("anchor-mid", "json");
        sign(&wal, &key, 1_700_010_000, &anchor_path).expect("签出");

        let tampered = tmp_path("wal-mid-tampered", "jsonl");
        let mut lines = wanning_core::wal::raw_lines(&wal).expect("读");
        let mut value: serde_json::Value = serde_json::from_str(&lines[1]).expect("行是 JSON");
        value["rec"]["intent"]["memo"] = serde_json::json!("中间行被改写");
        lines[1] = value.to_string();
        std::fs::write(&tampered, lines.join("\n") + "\n").expect("写");
        let err = verify(&tampered, &anchor_path, &key).unwrap_err();
        // 改第 2 行 → 第 2 行链值变 → 第 3 行的 prev 对不上(链从断点之后现形)。
        assert!(
            matches!(err, CoreError::WalChainBroken { line: 3, .. }),
            "中间行被动过,完整性链先现形(轮不到锚点比对): {err}"
        );
    }

    #[test]
    fn verify_rejects_forged_or_foreign_anchor() {
        let wal = tmp_path("wal-forge", "jsonl");
        build_sample_wal(&wal);
        let (_key_path, key) = key_file(KEY_HEX_32B);
        let anchor_path = tmp_path("anchor-forge", "json");
        sign(&wal, &key, 1_700_010_000, &anchor_path).expect("签出");

        // 伪造:改声明字段但 MAC 还是旧的 → 锚点不可信。
        let forged_path = tmp_path("anchor-forged", "json");
        let mut file: AnchorFile =
            serde_json::from_str(&std::fs::read_to_string(&anchor_path).expect("读锚点"))
                .expect("锚点是 JSON");
        file.lines = 99;
        std::fs::write(
            &forged_path,
            serde_json::to_string_pretty(&file).expect("序列化"),
        )
        .expect("写伪造锚点");
        let err = verify(&wal, &forged_path, &key).unwrap_err();
        assert!(
            matches!(err, CoreError::AnchorInvalid(ref m) if m.contains("MAC")),
            "伪造锚点要点名 MAC 不符: {err}"
        );

        // 换密钥验:所有者之外的人验不过。
        let (_alt_path, alt_key) = key_file(KEY_HEX_ALT);
        let err = verify(&wal, &anchor_path, &alt_key).unwrap_err();
        assert!(matches!(err, CoreError::AnchorInvalid(_)), "{err}");

        // 锚点缺文件/坏 JSON,同样 fail-closed 且指名锚点文件。
        let err = verify(&wal, &tmp_path("nope", "json"), &key).unwrap_err();
        assert!(matches!(err, CoreError::AnchorInvalid(_)), "{err}");
        let bad_json = tmp_path("anchor-bad", "json");
        std::fs::write(&bad_json, "{ not json").expect("写坏 JSON");
        let err = verify(&wal, &bad_json, &key).unwrap_err();
        assert!(matches!(err, CoreError::AnchorInvalid(_)), "{err}");
    }

    #[test]
    fn sign_refuses_broken_wal_and_never_touches_output() {
        let wal = tmp_path("wal-broken", "jsonl");
        build_sample_wal(&wal);
        let (_key_path, key) = key_file(KEY_HEX_32B);
        let out = tmp_path("anchor-out", "json");
        std::fs::write(&out, "SENTINEL").expect("预置旧输出");

        // 改中间行 → 坏账 → 拒签(对账先行)。
        let mut lines = wanning_core::wal::raw_lines(&wal).expect("读");
        let mut value: serde_json::Value = serde_json::from_str(&lines[1]).expect("行是 JSON");
        value["rec"]["intent"]["memo"] = serde_json::json!("被改写");
        lines[1] = value.to_string();
        std::fs::write(&wal, lines.join("\n") + "\n").expect("写坏账");

        let err = sign(&wal, &key, 1_700_010_000, &out).unwrap_err();
        assert!(
            matches!(err, CoreError::WalChainBroken { .. }),
            "坏账绝不签: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&out).expect("旧输出还在"),
            "SENTINEL",
            "拒签路径绝不碰输出文件"
        );
        let tmp = out.with_file_name({
            let mut name = out.file_name().unwrap().to_os_string();
            name.push(".tmp");
            name
        });
        assert!(!tmp.exists(), "不留临时文件");
    }

    #[test]
    fn sign_refuses_empty_wal() {
        let wal = tmp_path("wal-empty", "jsonl");
        std::fs::write(&wal, "").expect("写空账");
        let (_key_path, key) = key_file(KEY_HEX_32B);
        let err = sign(&wal, &key, 1_700_010_000, &tmp_path("anchor-empty", "json")).unwrap_err();
        assert!(
            matches!(err, CoreError::AnchorInvalid(ref m) if m.contains("空账")),
            "空账拒锚且说清原因: {err}"
        );
    }

    #[test]
    fn key_file_parsing_is_strict() {
        // 缺文件。
        let err = AnchorKey::from_hex_file(Path::new("D:/definitely/not/here.key")).unwrap_err();
        assert!(err.contains("读密钥文件"), "{err}");
        // 长度不对(31 字节)。
        let path = tmp_path("key-short", "hex");
        std::fs::write(&path, "ab".repeat(31)).expect("写");
        let err = AnchorKey::from_hex_file(&path).unwrap_err();
        assert!(err.contains("64 位十六进制"), "{err}");
        // 非十六进制。
        let path = tmp_path("key-bad", "hex");
        std::fs::write(&path, "zz".repeat(32)).expect("写");
        let err = AnchorKey::from_hex_file(&path).unwrap_err();
        assert!(err.contains("十六进制"), "{err}");
        // 空文件。
        let path = tmp_path("key-empty", "hex");
        std::fs::write(&path, "").expect("写");
        assert!(AnchorKey::from_hex_file(&path).is_err());
        // 带换行/空白容忍(所有者用编辑器存的,末尾换行是常态)。
        let (path, key) = key_file(KEY_HEX_32B);
        assert_eq!(
            AnchorKey::from_hex_file(&path).expect("重读"),
            key,
            "同文件重复读出同密钥"
        );
        // Debug 打码:密钥不进日志。
        assert!(!format!("{key:?}").contains("a5a5"), "Debug 必须打码");
        assert!(format!("{key:?}").contains("打码"));
    }

    #[test]
    fn atomic_write_keeps_old_output_on_rename_failure() {
        // 目标路径是个目录 → rename 失败 → 临时文件被清掉,原样报错。
        let dir_out = tmp_path("out-dir", "d");
        std::fs::create_dir_all(&dir_out).expect("建目录");
        let err = write_atomic(&dir_out, b"x").unwrap_err();
        assert!(matches!(err, CoreError::WalIo(_)), "{err}");
        let tmp = dir_out.with_file_name({
            let mut name = dir_out.file_name().unwrap().to_os_string();
            name.push(".tmp");
            name
        });
        assert!(!tmp.exists(), "失败后临时文件必须清掉");
    }
}
