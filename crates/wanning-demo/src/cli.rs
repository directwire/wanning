//! wanning-demo 的 CLI 主体(W-43a 起在 lib,不再埋在 bin 里)。
//!
//! 统一入口 `wanning demo`(W-43a)与旧 bin `wanning-demo` 走**同一段**参数解析
//! 与运行逻辑——保证两个入口的用法、报错口径、护栏行为永远一致,不会漂移成两套。
//! bin(`src/main.rs`)只剩薄壳。
//!
//! ```text
//! wanning-demo --scenario <name> [--dry-run true|false]
//! wanning-demo --export-audit <wal> --out <report.html>
//! wanning-demo --anchor-sign <wal> --key <key.hex> --out <anchor.json>   (所有者侧,W-23,v1 HMAC)
//! wanning-demo --anchor-verify <wal> --anchor <anchor.json> --key <key.hex>
//! wanning-demo --anchor-sign-v2 <wal> --seed <seed.hex> --out <anchor.json>  (所有者侧,W-31,ed25519)
//! ```
//!
//! `--dry-run` 默认 true(离线);设为 false 走真实消费路径,**先过 fail-closed 护栏**
//! ([`crate::guard`]):任何一项 env 缺失立即拒绝并打印缺什么。
//! `--export-audit` 把一份审计日志渲染成自包含 HTML 回放页(零 JS 零外链,
//! 先对账后产出,坏账绝不写输出文件)。
//! `--anchor-sign`/`--anchor-verify` 是**所有者侧**审计锚点(W-23,v1 HMAC):用
//! 自己的密钥锚住 WAL 前缀,堵住完整性链「改尾行/整体截尾验不住」的已知边界。
//! `--anchor-sign-v2`(W-31)是同一动作的 ed25519 版:公钥随锚点走,第三方零
//! 密钥即可验(独立 bin `wanning-anchor-verify`)。签名密钥/种子不在任何
//! Wanning 进程手里,MCP 工具面永不提供锚点能力。各模式互斥。
//! bin 只做参数解析与终端展示;可测逻辑在 lib。

use std::path::{Path, PathBuf};

use wanning_core::clock::{Clock, SystemClock};

use crate::anchor_cmd;
use crate::anchor_v2;
use crate::audit_html;
use crate::full_loop;
use crate::guard;
use crate::scenario;

/// 运行模式:四种互斥(一次只做一件事,两义性即拒)。
enum Mode {
    Scenario {
        name: String,
        dry_run: bool,
    },
    ExportAudit {
        wal: PathBuf,
        out: PathBuf,
    },
    /// 所有者侧:用自己的密钥签出锚点。
    AnchorSign {
        wal: PathBuf,
        key: PathBuf,
        out: PathBuf,
    },
    /// 所有者侧:拿锚点 + 密钥验当前账本(HMAC 验证需要密钥,见 anchor_cmd 模块注释)。
    AnchorVerify {
        wal: PathBuf,
        anchor: PathBuf,
        key: PathBuf,
    },
    /// 所有者侧:ed25519 种子签出锚点 v2(公钥随锚点走,第三方零密钥可验,W-31)。
    AnchorSignV2 {
        wal: PathBuf,
        seed: PathBuf,
        out: PathBuf,
    },
}

struct Cli {
    mode: Mode,
}

pub fn run(args: &[String]) -> Result<(), String> {
    let cli = parse_args(args)?;
    match cli.mode {
        Mode::ExportAudit { wal, out } => run_export(&wal, &out),
        Mode::Scenario { name, dry_run } => run_scenario_mode(&name, dry_run),
        Mode::AnchorSign { wal, key, out } => run_anchor_sign(&wal, &key, &out),
        Mode::AnchorVerify { wal, anchor, key } => run_anchor_verify(&wal, &anchor, &key),
        Mode::AnchorSignV2 { wal, seed, out } => run_anchor_sign_v2(&wal, &seed, &out),
    }
}

/// 导出模式:对账 → 渲染 → 落盘(库面 [`audit_html::export_audit`] 保证
/// fail-closed:坏账绝不产出/覆盖输出文件)。
fn run_export(wal: &std::path::Path, out: &std::path::Path) -> Result<(), String> {
    let report =
        audit_html::export_audit(wal, out, Some(SystemClock.now())).map_err(|e| e.to_string())?;
    println!("审计回放页已导出:{}", out.display());
    println!(
        "审计原文:{}({} 行,完整性链逐行验证通过,链尾 0x{:016x})",
        report.wal_display,
        report.rows.len(),
        report.chain_tail
    );
    println!(
        "回放对账:两遍回放 hash 一致(0x{:016x});预算台账 {} 份委托,放行 {} 笔 / 拒绝 {} 笔",
        report.replay_state_hash,
        report.delegations.len(),
        report.counts.allow,
        report.counts.deny
    );
    println!("页面自包含(零 JS 零外链,file:// 离线可开);证据以审计原文为准。");
    Ok(())
}

/// 所有者侧签锚点:读密钥 → 对账 → 签 → 原子落盘(库面 [`anchor_cmd::sign`])。
fn run_anchor_sign(wal: &Path, key_path: &Path, out: &Path) -> Result<(), String> {
    let key = anchor_cmd::AnchorKey::from_hex_file(key_path)?;
    let anchored_at = SystemClock.now();
    let file = anchor_cmd::sign(wal, &key, anchored_at, out).map_err(|e| e.to_string())?;
    println!("审计锚点已签出:{}", out.display());
    println!(
        "被锚账本:{} 前 {} 行(链尾 {},与审计回放页展示的链尾肉眼可对)",
        wal.display(),
        file.lines,
        file.chain_tail_hex
    );
    println!("前缀内容 SHA-256:{}", file.records_sha256_hex);
    println!("锚定时刻:{}(Unix 秒)", file.anchored_at_unix);
    println!("保管要求:锚点文件另行保管(与 WAL 分开存放/离机备份)——锚点和账本放");
    println!("在同一处、都能被写进程改到,锚点就成了自说自话;密钥文件绝不入仓。");
    println!("此后随时可 --anchor-verify:整体截尾 / 改尾行当场现形。");
    Ok(())
}

/// 所有者侧验锚点:锚点可信(MAC)→ 完整性链 → 前缀逐字段比对(库面
/// [`anchor_cmd::verify`]);任何一步不过都非零退出。
fn run_anchor_verify(wal: &Path, anchor_path: &Path, key_path: &Path) -> Result<(), String> {
    let key = anchor_cmd::AnchorKey::from_hex_file(key_path)?;
    let outcome = anchor_cmd::verify(wal, anchor_path, &key).map_err(|e| e.to_string())?;
    println!("锚点验证通过:{}", anchor_path.display());
    println!(
        "被锚前缀 {} 行 / 当前账本 {} 行(锚定后新增 {} 行,前缀锚不挡正常追加)",
        outcome.anchored_lines,
        outcome.current_lines,
        outcome.current_lines - outcome.anchored_lines
    );
    println!(
        "链尾 0x{:016x}  内容 SHA-256:{}",
        outcome.chain_tail, outcome.records_sha256_hex
    );
    println!(
        "锚定时刻:{}(Unix 秒);证据以审计原文为准。",
        outcome.anchored_at_unix
    );
    Ok(())
}

/// 所有者侧签锚点 v2:读 ed25519 种子 → 对账 → 签 → 原子落盘(库面
/// [`anchor_v2::sign_v2`])。公钥随锚点走;第三方用 `wanning-anchor-verify`
/// 零密钥即可验。
fn run_anchor_sign_v2(wal: &Path, seed_path: &Path, out: &Path) -> Result<(), String> {
    let seed = anchor_v2::Ed25519Seed::from_hex_file(seed_path)?;
    let anchored_at = SystemClock.now();
    let file = anchor_v2::sign_v2(wal, &seed, anchored_at, out).map_err(|e| e.to_string())?;
    println!("审计锚点(v2,ed25519)已签出:{}", out.display());
    println!(
        "被锚账本:{} 前 {} 行(链尾 {},与审计回放页展示的链尾肉眼可对)",
        wal.display(),
        file.lines,
        file.chain_tail_hex
    );
    println!("前缀内容 SHA-256:{}", file.records_sha256_hex);
    println!("公钥(hex):{}", file.public_key_hex);
    println!("锚定时刻:{}(Unix 秒)", file.anchored_at_unix);
    println!("第三方验法(零密钥):wanning-anchor-verify --anchor <本锚点> --wal <账本> \\");
    println!(
        "    --expect-key {}(公钥带外核对后钉定,换钥重签当场现形)",
        file.public_key_hex
    );
    println!("保管要求:锚点文件另行保管(与 WAL 分开存放/离机备份);种子文件绝不入仓。");
    Ok(())
}

/// 场景模式(W-07 起的原路径,语义不变)。
fn run_scenario_mode(name: &str, dry_run: bool) -> Result<(), String> {
    if !scenario::AVAILABLE_SCENARIOS.contains(&name) {
        return Err(format!(
            "未知场景 {name:?};可用场景:{}",
            scenario::AVAILABLE_SCENARIOS.join(", ")
        ));
    }

    if !dry_run {
        println!("--dry-run false:进入真实消费路径,先过 fail-closed 护栏…");
        let config = match guard::real_spend_from_process_env() {
            Ok(config) => config,
            Err(denied) => return Err(denied.to_string()),
        };
        // 护栏通过也只意味着「密钥齐」;通道接线与否是下一道门(今晚未接线)。
        println!("护栏通过(密钥已配置,明细打码:{config:?})。");
        return guard::open_real_channel(config);
    }

    run_scenario(name)
}

/// 场景分发(合法性已在 AVAILABLE_SCENARIOS 检查)。
fn run_scenario(name: &str) -> Result<(), String> {
    let result = match name {
        scenario::SCENARIO_SMOKE => scenario::run_smoke().map(|_| ()),
        scenario::SCENARIO_FOUR_SELLING_POINTS => scenario::run_four_selling_points().map(|_| ()),
        scenario::SCENARIO_FULL_LOOP_MOCK => full_loop::run_full_loop_mock().map(|_| ()),
        other => {
            return Err(format!(
                "未知场景 {other:?};可用场景:{}",
                scenario::AVAILABLE_SCENARIOS.join(", ")
            ))
        }
    };
    result.map_err(|e| e.to_string())
}

fn parse_args(args: &[String]) -> Result<Cli, String> {
    let mut scenario_name: Option<String> = None;
    let mut dry_run = true;
    let mut export_wal: Option<PathBuf> = None;
    let mut anchor_wal: Option<PathBuf> = None;
    let mut anchor_verify_mode = false;
    let mut anchor_v2_sign_mode = false;
    let mut out_path: Option<PathBuf> = None;
    let mut key_path: Option<PathBuf> = None;
    let mut seed_path: Option<PathBuf> = None;
    let mut anchor_file: Option<PathBuf> = None;
    let mut idx = 0;

    while idx < args.len() {
        match args[idx].as_str() {
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            "--scenario" => {
                let value = args
                    .get(idx + 1)
                    .ok_or_else(|| "--scenario 需要一个场景名;--help 看用法".to_string())?;
                scenario_name = Some(value.clone());
                idx += 2;
            }
            "--export-audit" => {
                let value = args.get(idx + 1).ok_or_else(|| {
                    "--export-audit 需要一个审计文件路径;--help 看用法".to_string()
                })?;
                export_wal = Some(PathBuf::from(value));
                idx += 2;
            }
            "--anchor-sign" => {
                let value = args.get(idx + 1).ok_or_else(|| {
                    "--anchor-sign 需要一个审计文件路径;--help 看用法".to_string()
                })?;
                anchor_wal = Some(PathBuf::from(value));
                anchor_verify_mode = false;
                idx += 2;
            }
            "--anchor-verify" => {
                let value = args.get(idx + 1).ok_or_else(|| {
                    "--anchor-verify 需要一个审计文件路径;--help 看用法".to_string()
                })?;
                anchor_wal = Some(PathBuf::from(value));
                anchor_verify_mode = true;
                idx += 2;
            }
            "--anchor-sign-v2" => {
                let value = args.get(idx + 1).ok_or_else(|| {
                    "--anchor-sign-v2 需要一个审计文件路径;--help 看用法".to_string()
                })?;
                anchor_wal = Some(PathBuf::from(value));
                anchor_v2_sign_mode = true;
                idx += 2;
            }
            "--seed" => {
                let value = args.get(idx + 1).ok_or_else(|| {
                    "--seed 需要一个 ed25519 种子文件路径;--help 看用法".to_string()
                })?;
                seed_path = Some(PathBuf::from(value));
                idx += 2;
            }
            "--key" => {
                let value = args
                    .get(idx + 1)
                    .ok_or_else(|| "--key 需要一个密钥文件路径;--help 看用法".to_string())?;
                key_path = Some(PathBuf::from(value));
                idx += 2;
            }
            "--anchor" => {
                let value = args
                    .get(idx + 1)
                    .ok_or_else(|| "--anchor 需要一个锚点文件路径;--help 看用法".to_string())?;
                anchor_file = Some(PathBuf::from(value));
                idx += 2;
            }
            "--out" => {
                let value = args
                    .get(idx + 1)
                    .ok_or_else(|| "--out 需要一个输出文件路径;--help 看用法".to_string())?;
                out_path = Some(PathBuf::from(value));
                idx += 2;
            }
            "--dry-run" => match args.get(idx + 1) {
                // 带值:true/false;裸旗标 = 显式 true。
                Some(value) if value == "true" || value == "false" => {
                    dry_run = value == "true";
                    idx += 2;
                }
                _ => {
                    dry_run = true;
                    idx += 1;
                }
            },
            other => return Err(format!("未知参数 {other:?};--help 看用法")),
        }
    }

    if scenario_name.is_some() && (export_wal.is_some() || out_path.is_some()) {
        return Err(
            "--scenario 与 --export-audit/--out 互斥,一次只做一件事;--help 看用法".to_string(),
        );
    }
    if anchor_wal.is_some() && (scenario_name.is_some() || export_wal.is_some()) {
        return Err(
            "--anchor-sign/--anchor-verify 与 --scenario/--export-audit 互斥,一次只做一件事;\
             --help 看用法"
                .to_string(),
        );
    }

    // 锚点三模式:v1 签/验要 --key(HMAC 对称);v2 签要 --seed(ed25519);
    // v2 验在独立 bin wanning-anchor-verify(第三方零密钥)。
    if let Some(wal) = anchor_wal {
        if anchor_v2_sign_mode {
            if key_path.is_some() {
                return Err(
                    "--key 是 v1(HMAC)的密钥;v2 签名用 --seed <ed25519 种子文件>;--help 看用法"
                        .to_string(),
                );
            }
            if anchor_verify_mode {
                return Err(
                    "--anchor-sign-v2 与 --anchor-verify 互斥,一次只做一件事;--help 看用法"
                        .to_string(),
                );
            }
            if anchor_file.is_some() {
                return Err("--anchor 只随 --anchor-verify 使用;--help 看用法".to_string());
            }
            let seed = seed_path.ok_or_else(|| {
                "--anchor-sign-v2 需要 --seed <种子文件>(64 位十六进制 = 32 字节 \
                 ed25519 种子);--help 看用法"
                    .to_string()
            })?;
            let out = out_path.ok_or_else(|| {
                "--anchor-sign-v2 需要 --out <锚点输出路径> 指定锚点文件落哪;--help 看用法"
                    .to_string()
            })?;
            return Ok(Cli {
                mode: Mode::AnchorSignV2 { wal, seed, out },
            });
        }
        let key = key_path.ok_or_else(|| {
            "--anchor-sign/--anchor-verify 需要 --key <密钥文件>(64 位十六进制的所有者密钥);\
             --help 看用法"
                .to_string()
        })?;
        if seed_path.is_some() {
            return Err(
                "--seed 是 v2(ed25519)的签名种子;v1 模式用 --key;--help 看用法".to_string(),
            );
        }
        if anchor_verify_mode {
            let anchor = anchor_file.ok_or_else(|| {
                "--anchor-verify 需要 --anchor <锚点文件>;--help 看用法".to_string()
            })?;
            if out_path.is_some() {
                return Err(
                    "--out 只随 --export-audit/--anchor-sign 使用;--help 看用法".to_string()
                );
            }
            return Ok(Cli {
                mode: Mode::AnchorVerify { wal, anchor, key },
            });
        }
        if anchor_file.is_some() {
            return Err("--anchor 只随 --anchor-verify 使用;--help 看用法".to_string());
        }
        let out = out_path.ok_or_else(|| {
            "--anchor-sign 需要 --out <锚点输出路径> 指定锚点文件落哪;--help 看用法".to_string()
        })?;
        return Ok(Cli {
            mode: Mode::AnchorSign { wal, key, out },
        });
    }

    if let Some(wal) = export_wal {
        let out = out_path.ok_or_else(|| {
            "--export-audit 需要 --out <html 路径> 指定输出文件;--help 看用法".to_string()
        })?;
        return Ok(Cli {
            mode: Mode::ExportAudit { wal, out },
        });
    }

    if out_path.is_some() {
        return Err("--out 只随 --export-audit/--anchor-sign 使用;--help 看用法".to_string());
    }
    if key_path.is_some() {
        return Err("--key 只随 --anchor-sign/--anchor-verify 使用;--help 看用法".to_string());
    }
    if seed_path.is_some() {
        return Err("--seed 只随 --anchor-sign-v2 使用;--help 看用法".to_string());
    }
    if anchor_file.is_some() {
        return Err("--anchor 只随 --anchor-verify 使用;--help 看用法".to_string());
    }

    let scenario = scenario_name.ok_or_else(|| {
        "缺少 --scenario <name> / --export-audit <wal> / --anchor-sign|--anchor-verify <wal>;\
         --help 看用法"
            .to_string()
    })?;
    Ok(Cli {
        mode: Mode::Scenario {
            name: scenario,
            dry_run,
        },
    })
}

fn print_usage() {
    println!(
        "wanning-demo —— Wanning P0 演示(全离线 + 本地 mock,零真实消费)\n\
         \n\
         用法:\n\
         \x20 wanning-demo --scenario <name> [--dry-run true|false]\n\
         \x20 wanning-demo --export-audit <wal> --out <report.html>\n\
         \x20 wanning-demo --anchor-sign <wal> --key <key.hex> --out <anchor.json>\n\
         \x20 wanning-demo --anchor-verify <wal> --anchor <anchor.json> --key <key.hex>\n\
         \x20 wanning-demo --anchor-sign-v2 <wal> --seed <seed.hex> --out <anchor.json>\n\
         \x20   (v2 验签走独立 bin:wanning-anchor-verify --anchor <a.json> --wal <w.jsonl>\n\
         \x20     [--expect-key <公钥 hex>];第三方零密钥,W-31)\n\
         \n\
         参数:\n\
         \x20 --scenario <name>   场景名;当前可用:{}\n\
         \x20 --dry-run <bool>    默认 true(离线)。设为 false 会先过真实消费护栏\n\
         \x20                     ({}=1 + {} 等,缺任一项立即拒绝并打印缺什么);\n\
         \x20                     护栏通过后,真实通道未接线同样拒绝(今晚真相)。\n\
         \x20 --export-audit <wal>  把审计日志渲染成自包含 HTML 回放页(先验完整性链\n\
         \x20                     再回放对账,坏账绝不产出输出;零 JS 零外链,离线可开)。\n\
         \x20 --out <path>        输出路径(只随 --export-audit / --anchor-sign 使用)。\n\
         \x20 --anchor-sign <wal>   所有者侧:用自己的密钥签出审计锚点(锚住「前 N 行\n\
         \x20                     内容 + 行数 + 链尾」),堵住完整性链的已知边界\n\
         \x20                     (只改尾行/整体截尾本地验不住)。对账先行,坏账不签。\n\
         \x20 --anchor-verify <wal> 所有者侧:拿锚点验当前账本——截尾/改尾行当场现形;\n\
         \x20                     锚定后合法追加的新行不影响通过(前缀锚语义)。\n\
         \x20 --anchor-sign-v2 <wal> 所有者侧(v2,ed25519):用种子签出第三方可验锚点,\n\
         \x20                     公钥随锚点走;验签无需任何密钥(独立 bin\n\
         \x20                     wanning-anchor-verify)。坏账不签,对账先行。\n\
         \x20 --key <path>        密钥文件(恰好 64 位十六进制 = 32 字节);v1 两模式\n\
         \x20                     必填。密钥保管是人的程序:不在任何 Wanning 进程\n\
         \x20                     手里,Debug/日志一律打码,绝不入仓。\n\
         \x20 --seed <path>       ed25519 种子文件(恰好 64 位十六进制 = 32 字节);\n\
         \x20                     只随 --anchor-sign-v2。保管纪律同 --key。\n\
         \x20 --anchor <path>     锚点文件(只随 --anchor-verify 使用);另行保管,\n\
         \x20                     与 WAL 分开存放。\n\
         \x20 各模式互斥。MCP 工具面永不提供锚点能力(agent 能签就能伪造锚点)。\n\
         \x20 -h / --help         本帮助",
        scenario::AVAILABLE_SCENARIOS.join(", "),
        guard::ENV_ALLOW_REAL_SPEND,
        guard::REQUIRED_KEYS[0],
    );
}
