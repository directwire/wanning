//! wanning-anchor-verify(W-31):**独立第三方验签 bin**。
//!
//! ```text
//! wanning-anchor-verify --anchor <anchor.json> --wal <audit.jsonl> [--expect-key <64位hex>]
//! ```
//!
//! 只验 ed25519 锚点 v2([`wanning_demo::anchor_v2`])。**没有 --key 选项**:
//! 公钥随锚点走,第三方不需要任何密钥文件——这是 v2 对 W-23「HMAC 对称,
//! 第三方不可独立验证」边界的升级收口。
//!
//! `--expect-key`(可选)= 第三方从老板公开渠道核对过的公钥(带外身份绑定):
//! 钉定后,「换公钥重签」的锚点当场 fail-closed。不钉定时,内部自洽的锚点
//! 验得过——签名只证明「持对应私钥者签的」,不证明「持钥者是老板」;
//! 这一半是密码学解决不了的,如实打印在回执里,不装不存在。
//!
//! 验证顺序即 fail-closed 顺序:版本/schema → 公钥钉定 → ed25519 签名 →
//! WAL 完整性链 → 前缀逐字段比对。任何一步不过 → 非零退出。
//!
//! 用法示例(第三方视角):
//!
//! ```bash
//! wanning-anchor-verify --anchor boss-anchor.json --wal audit.jsonl \
//!     --expect-key d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use wanning_demo::anchor_v2::verify_v2;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let parsed = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::FAILURE;
        }
    };
    let ParsedArgs {
        anchor,
        wal,
        expect_key,
    } = parsed;

    match verify_v2(&wal, &anchor, expect_key.as_deref()) {
        Ok(outcome) => {
            println!("锚点验证通过(v2,ed25519):{}", anchor.display());
            println!("  schema/version:wanning-anchor-v2 / 2");
            println!("  公钥(hex):{}", outcome.public_key_hex);
            println!("  锚定行数:{}", outcome.anchored_lines);
            println!(
                "  当前账本行数:{}(锚定后新增 {} 行,前缀锚不挡合法追加)",
                outcome.current_lines,
                outcome.current_lines - outcome.anchored_lines
            );
            println!("  前缀链尾:0x{:016x}", outcome.chain_tail);
            println!("  前缀内容 SHA-256:{}", outcome.records_sha256_hex);
            println!("  锚定时刻:{}(Unix 秒)", outcome.anchored_at_unix);
            if expect_key.is_none() {
                println!(
                    "注意:未钉定 --expect-key。签名只证明「持对应私钥者签的」,\
                     不证明「持钥者是老板」——请从老板公开渠道核对上面这行公钥。"
                );
            } else {
                println!("  期望公钥已钉定并与锚点一致(带外身份核对通过)。");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("锚点验证失败:{}", err);
            ExitCode::FAILURE
        }
    }
}

struct ParsedArgs {
    anchor: PathBuf,
    wal: PathBuf,
    expect_key: Option<String>,
}

fn parse_args(args: &[String]) -> Result<ParsedArgs, String> {
    let mut anchor: Option<PathBuf> = None;
    let mut wal: Option<PathBuf> = None;
    let mut expect_key: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => return Err(usage()),
            "--anchor" => {
                anchor = Some(PathBuf::from(take_value(args, &mut i, "--anchor")?));
            }
            "--wal" => {
                wal = Some(PathBuf::from(take_value(args, &mut i, "--wal")?));
            }
            "--expect-key" => {
                expect_key = Some(take_value(args, &mut i, "--expect-key")?.to_string());
            }
            other => {
                return Err(format!("未知参数 {other:?}。\n{}", usage()));
            }
        }
        i += 1;
    }

    let anchor = anchor.ok_or_else(|| format!("缺少 --anchor <锚点文件>。\n{}", usage()))?;
    let wal = wal.ok_or_else(|| format!("缺少 --wal <审计文件>。\n{}", usage()))?;
    Ok(ParsedArgs {
        anchor,
        wal,
        expect_key,
    })
}

fn take_value<'a>(args: &'a [String], i: &mut usize, flag: &str) -> Result<&'a str, String> {
    *i += 1;
    args.get(*i)
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} 需要一个值。\n{}", usage()))
}

fn usage() -> String {
    "用法:wanning-anchor-verify --anchor <anchor.json> --wal <audit.jsonl> [--expect-key <64位hex>]\n\
     \x20 第三方验签:公钥随锚点走,无需任何密钥文件。\n\
     \x20 --expect-key 可选:从老板公开渠道核对过的公钥;钉定后「换公钥重签」当场 fail-closed。"
        .to_string()
}
