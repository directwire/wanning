//! `wanning channel-test`:渠道钥匙验证(W-52,L0→L3 分级阶梯,绝不跳级)。
//!
//! **定位注记(任务书原文)**:本命令是「免密代扣(平台侧,第二形式)」的钥匙
//! 验证工具——验证所有者在支付平台侧的商户密钥能不能用;**个人用户旅程用不到
//! 本命令**(个人 = 人在环、零开户,W-53 第一形式)。
//!
//! # 阶梯(L1→L2→L3 绝不跳级)
//!
//! - **L0 环境齐套**:零网络,逐槽位报 已设/未设(值绝不回显),缺项给 ✗ 修复
//!   指引,缺 L1 前置密钥就停在这里;
//! - **L1 签名自测**:零网络——env 注入的商户私钥真签一笔 `alipay.trade.precreate`
//!   自测报文(不碰真 app_id、不碰网关),再用支付宝公钥验回;密钥格式错误或
//!   「私钥/公钥不是一对」当场现形,这是所有真实路径之前的免费保险;
//! - **L2 网关探针**:真网关,**零资金移动**——`alipay.trade.precreate`(当面付
//!   预下单)只生成二维码,买家不扫码即不产生资金流,探针不扫码。回答的问题 =
//!   **服务器认不认签名**(W-50「待实签」清单第一格);业务权限被拒也是已验签
//!   响应,同样回答这个问题。过 ≠ 能扣款;
//! - **L3 协议内真实扣款**:真实扣款 0.01 元(`alipay.trade.pay`,协议内扣款,
//!   本人账户),在 L2 之上追加 `--real-spend` + 专属确认文案。
//!
//! # 三重明示 fail-closed(缺一即拒)
//!
//! ① env `WANNING_ALLOW_REAL_SPEND=1`(W-07 同一开关,过账本不豁免护栏);
//! ② `--real` 显式给出(缺省 = 只到 L1,零网络);
//! ③ TTY 交互确认(非交互环境一律拒——防脚本/agent 无人值守误触)。
//! L3 追加第四重:`--real-spend` + 专属确认文案(真实扣款,不可撤销)。
//!
//! # 诚实边界
//!
//! - 京东(jd)渠道 channel-test **不支持**:签名算法公开面查不到(W-50 复核在
//!   档),做不了真签名就不硬做;微信(wechat)/美团(meituan)同理,原因见
//!   [`UNSUPPORTED_CHANNELS`];
//! - 探针意图照落审计账本(**过账本不豁免**),账本独立于产品账本(缺省
//!   `~/.wanning/channel-test.jsonl`,每日探针预算 100 分,用尽即明天再探);
//! - 取证档脱敏落 `--evidence` 目录(缺省 `target/channel-test/`):**密钥只从
//!   env 现取现用,绝不落盘、绝不 echo、绝不进取证档**;失败与用户取消零落盘;
//! - `code≠10000` 的判读零编造:响应已验签 = 公钥与验签管线正确;拒绝原因以
//!   `sub_code` 为准(官方《公共错误码》表在档),绝不硬编码语义映射;
//! - 状态映射(`async_payment_mode`)是模板决策非官方规定,L3 实跑时核对。
//!
//! # 报文同源
//!
//! L2/L3 报文全部复用 `wanning_demo::alipay` 的 W-50 官方模板管线
//! ([`wanning_demo::alipay::build_precreate_request`] /
//! [`wanning_demo::alipay::build_trade_pay_request`]),**不另写字段面**(任务书
//! 纪律);真实路径护栏复用 W-07([`wanning_demo::guard`]),探针配置走
//! `from_snapshot_probe`(无协议号,拿去扣款会在构建层 fail-closed)。
//!
//! # 测试面
//!
//! [`Confirmer`] 是确认通道的接缝:库层只要求「给提示、回是/否」;CLI 装的是
//! [`TtyConfirmer`](非 TTY 一律拒),测试装脚本替身。全部测试打本地替身网关
//! (`WANNING_ALIPAY_ENDPOINT`)+ 自生成测试密钥对(现场生成自销毁,绝不真
//! 密钥绝不落仓),零真网关零真实消费。

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use wanning_core::clock::{Clock, SystemClock};
use wanning_core::error::CoreError;
use wanning_core::gate::GateDecision;
use wanning_core::state::WanningState;
use wanning_demo::alipay::{
    self, AlipayBackend, PayRequest, PaymentChannel, PaymentError, PrecreateProbe,
    PRECREATE_PROBE_AMOUNT_CENTS,
};
use wanning_demo::audit_html::format_utc;
use wanning_demo::guard::{self, EnvSnapshot};
use wanning_demo::http::ApiTransport;
use wanning_demo::signing::{
    self, canonical_query, EnvRsaSigner, EnvRsaVerifier, MessageSigner, SignatureVerifier,
};

use crate::slash;

/// 用法说明(每条都是给所有者的操作面;密钥纪律写死在这里)。
const USAGE: &str = "wanning channel-test:渠道钥匙验证(L0→L3 分级阶梯,绝不跳级)

定位:本命令是「免密代扣(平台侧,第二形式)」的钥匙验证工具;个人用户旅程
(人在环、零开户)用不到它。

用法: wanning channel-test --channel <名> [--wal <账本>] [--evidence <目录>] [--real] [--real-spend]

  --channel <名>    渠道:alipay(唯一支持);京东/微信/美团如实标不支持,原因见下
  --wal <账本>      探针审计账本(缺省 ~/.wanning/channel-test.jsonl,独立于产品账本)
  --evidence <目录> 取证档目录(缺省 target/channel-test;脱敏,密钥绝不入档)
  --real            放行到 L2(真网关探针;缺省只到 L1 零网络自测)
  --real-spend      放行到 L3(真实扣款 0.01 元;只在 --real 之下有效)
  -h / --help       本说明

阶梯(绝不跳级):
  L0 环境齐套    零网络,缺项给 ✗ 修复指引(密钥值绝不回显)
  L1 签名自测    零网络:env 商户私钥真签 + 支付宝公钥验回(不配对当场现形)
  L2 网关探针    真网关零资金移动(alipay.trade.precreate 预下单,探针不扫码);
                需要 --real + WANNING_ALLOW_REAL_SPEND=1 + TTY 交互确认
  L3 协议内扣款  真实扣款 0.01 元(alipay.trade.pay,协议内扣款,本人账户);
                在 L2 之上追加 --real-spend + 专属确认

三重明示(缺一即拒,fail-closed):① env WANNING_ALLOW_REAL_SPEND=1;② --real 显式;
  ③ TTY 交互确认;L3 追加第四重 --real-spend 与专属确认文案。

不支持渠道(如实标注,不硬做):
  jd      京东 VOP:签名算法公开面查不到(调研在档),做不了真签名
  wechat  微信:V3 签名/回调验签面未实签(账户未开通)
  meituan 美团:契约占位(公开面查不到用户侧免密 API)

诚实边界:L2 过 = 服务器认了签名,过 ≠ 能扣款;探针意图照落审计账本(过账本
不豁免);密钥只从环境变量现取现用,绝不落盘、绝不 echo、绝不进取证档。

退出码:0 走到的阶梯全过(L1 起);1 运行失败(缺密钥/用户取消/网关拒/验签
不过);2 用法错(未知渠道/缺 --channel/未知参数)。
";

/// L3 探针的每日预算上限(分)。0.01 元 × 100 次/日——探针预算用尽 = 明天再探,
/// 绝不放大探针预算绕过闸。
const PROBE_DAILY_BUDGET_CENTS: u64 = 100;

/// 本命令支持的渠道(当前只有支付宝:签名管线 W-50 官方规则已直核填实)。
pub const SUPPORTED_CHANNELS: &[&str] = &["alipay"];

/// 已知但不支持的渠道与诚实原因(对应 --help「不支持渠道」节;未知渠道单独报)。
pub const UNSUPPORTED_CHANNELS: &[(&str, &str)] = &[
    (
        "jd",
        "京东 VOP 签名算法公开面查不到(W-50 复核在档),做不了真签名,绝不硬做",
    ),
    (
        "wechat",
        "微信 V3 签名/回调验签面未实签(账户未开通),账面撑不起真网关探针",
    ),
    (
        "meituan",
        "美团为契约占位(公开面查不到用户侧免密 API),没有可探的报文面",
    ),
];

// ── env 槽位(值只进槽位,绝不回显/落盘/进取证档) ─────────────────────────

/// 支付宝开放平台应用 app_id。
pub const SLOT_ALIPAY_APP_ID: &str = "WANNING_ALIPAY_APP_ID";
/// 商户应用私钥(签名槽位;W-52 env 注入位)。
pub const SLOT_ALIPAY_MERCHANT_PRIVATE_KEY: &str = signing::ENV_MERCHANT_PRIVATE_KEY;
/// 支付宝公钥(验签槽位;W-52 env 注入位)。
pub const SLOT_ALIPAY_ALIPAY_PUBLIC_KEY: &str = signing::ENV_ALIPAY_PUBLIC_KEY;
/// 用户签约协议号(L3 协议内扣款凭证;没有协议号的扣款 = 裸转账,绝不发)。
pub const SLOT_ALIPAY_AGREEMENT_NO: &str = "WANNING_ALIPAY_AGREEMENT_NO";
/// 覆盖官方网关(测试打本地替身;生产不要设)。
pub const SLOT_ALIPAY_ENDPOINT: &str = "WANNING_ALIPAY_ENDPOINT";
/// 异步通知地址(L3 扣款回调;L2 探针不带)。
pub const SLOT_ALIPAY_NOTIFY_URL: &str = "WANNING_ALIPAY_NOTIFY_URL";

/// 槽位表:名字 + 所属阶梯 + 缺了会怎样(给 ✗ 修复指引用)。
struct SlotRow {
    slot: &'static str,
    level: &'static str,
    why: &'static str,
}

/// 支付宝渠道的槽位清单(L0 表按此逐行报;顺序 = 展示顺序)。
const ALIPAY_SLOT_TABLE: [SlotRow; 11] = [
    SlotRow {
        slot: SLOT_ALIPAY_MERCHANT_PRIVATE_KEY,
        level: "L1",
        why: "商户应用私钥(开放平台密钥工具生成;PKCS#8/PKCS#1 PEM 或裸 base64 DER)",
    },
    SlotRow {
        slot: SLOT_ALIPAY_ALIPAY_PUBLIC_KEY,
        level: "L1",
        why: "支付宝公钥(应用详情页那把平台公钥,不是你自己的应用公钥)",
    },
    SlotRow {
        slot: SLOT_ALIPAY_APP_ID,
        level: "L2",
        why: "支付宝开放平台应用 app_id",
    },
    SlotRow {
        slot: guard::ENV_ALLOW_REAL_SPEND,
        level: "L2",
        why: "真实路径护栏(W-07):明示授权开关,值必须为 1",
    },
    SlotRow {
        slot: SLOT_ALIPAY_ENDPOINT,
        level: "可选",
        why: "覆盖官方网关(测试打本地替身;生产不要设)",
    },
    SlotRow {
        slot: SLOT_ALIPAY_NOTIFY_URL,
        level: "可选",
        why: "异步通知地址(L3 扣款回调;L2 探针不带)",
    },
    SlotRow {
        slot: SLOT_ALIPAY_AGREEMENT_NO,
        level: "L3",
        why: "用户签约协议号(协议内扣款凭证;没有协议号的扣款=裸转账,绝不发)",
    },
    SlotRow {
        slot: guard::REQUIRED_KEYS[0],
        level: "L3",
        why: "W-07 全链护栏四密钥之一:L3 复用 from_snapshot_real,四密钥须齐",
    },
    SlotRow {
        slot: guard::REQUIRED_KEYS[1],
        level: "L3",
        why: "同上(W-07 全链护栏)",
    },
    SlotRow {
        slot: guard::REQUIRED_KEYS[2],
        level: "L3",
        why: "同上(W-07 全链护栏)",
    },
    SlotRow {
        slot: guard::REQUIRED_KEYS[3],
        level: "L3",
        why: "同上(W-07 全链护栏)",
    },
];

/// 探针可见的环境快照:只收本命令的槽位,值只进槽位。
///
/// CLI 层读进程环境、显式传进来(库层确定性,W-51a InstallEnv 同一纪律);测试
/// 直接 `set` 构造,零进程环境依赖。空值/纯空白 = 未设(fail-closed 语义)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeEnv {
    slots: BTreeMap<&'static str, String>,
}

impl ProbeEnv {
    /// 从当前进程环境读本命令的槽位(空值按未设处理)。
    pub fn from_process_env() -> Self {
        let mut env = Self::default();
        for row in &ALIPAY_SLOT_TABLE {
            if let Ok(value) = std::env::var(row.slot) {
                if !value.trim().is_empty() {
                    env.slots.insert(row.slot, value);
                }
            }
        }
        env
    }

    /// 取槽位值(未设/空 = `None`)。
    pub fn get(&self, slot: &str) -> Option<&str> {
        self.slots.get(slot).map(String::as_str)
    }

    /// 设槽位(测试构造用)。
    pub fn set(&mut self, slot: &'static str, value: &str) {
        self.slots.insert(slot, value.to_string());
    }

    /// 槽位是否已设(非空)。
    pub fn is_set(&self, slot: &str) -> bool {
        self.get(slot)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    }

    /// 转 W-07 护栏的环境快照(同一份值喂 [`guard`],不读第二遍进程环境)。
    pub fn guard_snapshot(&self) -> EnvSnapshot {
        let mut snapshot = EnvSnapshot::default();
        for (slot, value) in &self.slots {
            snapshot.insert(slot, value);
        }
        snapshot
    }
}

/// 交互确认通道(测试接缝):回 `Ok(false)` = 用户拒绝;回 `Err` = 确认通道本身
/// 不可用(fail-closed 终止,与拒绝同义——问不出口的确认绝不能当成已确认)。
pub trait Confirmer {
    fn confirm(&mut self, prompt: &str) -> Result<bool, ChannelTestError>;
}

/// CLI 的确认实现:非 TTY 一律拒(三重明示的第三重)。
pub struct TtyConfirmer;

impl Confirmer for TtyConfirmer {
    fn confirm(&mut self, prompt: &str) -> Result<bool, ChannelTestError> {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return Err(ChannelTestError::Failed(
                "非交互环境(标准输入不是 TTY):交互确认不可用,fail-closed 拒绝继续。\
                 这是防脚本/agent 无人值守误触的硬门;真要跑请在本机终端亲手执行。"
                    .to_string(),
            ));
        }
        print!("{prompt}");
        std::io::stdout()
            .flush()
            .map_err(|e| ChannelTestError::Failed(format!("确认提示写出失败: {e}")))?;
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| ChannelTestError::Failed(format!("确认输入读取失败: {e}")))?;
        Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes"))
    }
}

/// 错误分层:用法错(退出码 2)与运行失败(退出码 1)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelTestError {
    Usage(String),
    Failed(String),
}

/// 探针选项(`run` 从 argv 组装;测试直接构造)。
#[derive(Debug, Clone)]
pub struct ProbeOptions {
    pub channel: String,
    pub wal: PathBuf,
    pub evidence_dir: PathBuf,
    pub real: bool,
    pub real_spend: bool,
    pub env: ProbeEnv,
    /// 传输替身注入点(测试唯一入口;`None` = 生产 ureq)。`run` 恒为 `None`
    /// ——真网关探针只从进程环境出发;测试经此把 L2/L3 指向本地替身网关,
    /// 与 `AlipayBackend::with_transport` 同一先例(W-10/W-50)。
    pub transport: Option<Arc<dyn ApiTransport + Send + Sync>>,
}

/// 探针结果:走到的阶梯 + 取证档路径(失败时用户取消/缺项不产档)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutcome {
    pub reached: &'static str,
    pub evidence: Vec<PathBuf>,
}

/// `wanning channel-test` 子命令入口(参数解析 + TTY 确认器)。
pub fn run(args: &[String]) -> Result<(), ChannelTestError> {
    let mut channel: Option<String> = None;
    let mut wal: Option<PathBuf> = None;
    let mut evidence_dir: Option<PathBuf> = None;
    let mut real = false;
    let mut real_spend = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--channel" => {
                channel = Some(next_value(args, &mut index, "--channel")?.to_string());
            }
            "--wal" => wal = Some(next_value(args, &mut index, "--wal")?.into()),
            "--evidence" => evidence_dir = Some(next_value(args, &mut index, "--evidence")?.into()),
            "--real" => real = true,
            "--real-spend" => real_spend = true,
            other => {
                return Err(ChannelTestError::Usage(format!(
                    "未知参数 '{other}'(用法:wanning channel-test --channel <名> \
                     [--wal <账本>] [--evidence <目录>] [--real] [--real-spend])"
                )))
            }
        }
        index += 1;
    }
    if real_spend && !real {
        return Err(ChannelTestError::Usage(
            "--real-spend 只在 --real 之下有效(L3 是 L2 的追加,不是独立入口;阶梯绝不跳级)。\
             先给 --real。"
                .to_string(),
        ));
    }
    let channel = channel.ok_or_else(|| {
        ChannelTestError::Usage(
            "缺少 --channel <名>(支持:alipay;京东/微信/美团如实标不支持,原因见 --help)".to_string(),
        )
    })?;
    let wal = match wal {
        Some(wal) => wal,
        // 探针账本独立于产品账本(~/.wanning/wal.jsonl 不混账),但同一条审计纪律。
        None => wanning_core::paths::default_home()
            .map(|home| home.join("channel-test.jsonl"))
            .ok_or_else(|| {
                ChannelTestError::Failed(
                    "解析不出默认账本路径(WANNING_HOME / USERPROFILE / HOME 都没有):\
                     用 --wal 显式给一个"
                        .to_string(),
                )
            })?,
    };
    let mut options = ProbeOptions {
        channel,
        wal,
        evidence_dir: evidence_dir.unwrap_or_else(|| PathBuf::from("target/channel-test")),
        real,
        real_spend,
        env: ProbeEnv::from_process_env(),
        transport: None,
    };
    let mut confirmer = TtyConfirmer;
    let outcome = run_probe(&mut options, &mut confirmer)?;
    println!();
    println!("channel-test 结束:到达 {};取证档(脱敏):", outcome.reached);
    for path in &outcome.evidence {
        println!("  {}", slash(path));
    }
    Ok(())
}

/// 探针主体(阶梯 L0→L1→L2→L3;[`Confirmer`] 注入,测试可换脚本替身)。
pub fn run_probe(
    options: &mut ProbeOptions,
    confirmer: &mut dyn Confirmer,
) -> Result<ProbeOutcome, ChannelTestError> {
    // --real-spend 依赖 --real:L3 是 L2 的追加(run 已查一遍,这里保底——两个
    // 入口都能到达同一道门,与「护栏要立在通道上不立在门口」同一原则)。
    if options.real_spend && !options.real {
        return Err(ChannelTestError::Failed(
            "缺少 --real:--real-spend 只在 --real 之下有效(阶梯 L1→L2→L3 绝不跳级)".to_string(),
        ));
    }
    check_channel(&options.channel)?;

    print_preamble(&options.env);
    let evidence = run_alipay_ladder(options, confirmer)?;
    Ok(evidence)
}

/// 渠道支持矩阵:未知渠道与已知不支持渠道都走用法错(诚实标注,不硬做)。
fn check_channel(channel: &str) -> Result<(), ChannelTestError> {
    if SUPPORTED_CHANNELS.contains(&channel) {
        return Ok(());
    }
    if let Some((_, reason)) = UNSUPPORTED_CHANNELS
        .iter()
        .find(|(name, _)| *name == channel)
    {
        return Err(ChannelTestError::Usage(format!(
            "channel-test 暂不支持渠道 '{channel}':{reason}\n当前支持:alipay(详见 --help)"
        )));
    }
    let unsupported: Vec<&str> = UNSUPPORTED_CHANNELS.iter().map(|(name, _)| *name).collect();
    Err(ChannelTestError::Usage(format!(
        "未知渠道 '{channel}'。支持:{};不支持(原因见 --help):{}",
        SUPPORTED_CHANNELS.join("/"),
        unsupported.join("/")
    )))
}

fn print_preamble(env: &ProbeEnv) {
    println!("Wanning channel-test —— 渠道钥匙验证(阶梯 L0→L1→L2→L3,绝不跳级)");
    println!(
        "定位:本命令是「免密代扣(平台侧,第二形式)」的钥匙验证工具;个人用户旅程\
         (人在环、零开户)用不到本命令。"
    );
    println!();
    println!("[L0] 环境齐套(零网络;只报 已设/未设,值绝不回显)");
    for row in &ALIPAY_SLOT_TABLE {
        let mark = if env.is_set(row.slot) { "✅" } else { "❌" };
        println!(
            "  {mark} [{level}] {slot} —— {why}",
            level = row.level,
            slot = row.slot,
            why = row.why
        );
    }
}

/// 阶梯主体(支付宝渠道;唯一支持渠道,渠道检查已在上游)。
fn run_alipay_ladder(
    options: &mut ProbeOptions,
    confirmer: &mut dyn Confirmer,
) -> Result<ProbeOutcome, ChannelTestError> {
    let env = &options.env;
    // ── L0:环境齐套 ──────────────────────────────────────────────────────
    let mut evidence: Vec<PathBuf> = Vec::new();
    let missing_l1: Vec<&SlotRow> = ALIPAY_SLOT_TABLE
        .iter()
        .filter(|row| row.level == "L1" && !env.is_set(row.slot))
        .collect();
    if !missing_l1.is_empty() {
        eprintln!();
        eprintln!("❌ L1 前置密钥未齐,停在 L0(阶梯绝不跳级;零网络零落账):");
        for row in &missing_l1 {
            eprintln!("  ✗ {} 未设 —— {}", row.slot, row.why);
        }
        eprintln!("  修复:把密钥材料放进上面两个环境变量后重跑(应用私钥用开放平台");
        eprintln!("  密钥工具生成;支付宝公钥在应用详情页)。");
        eprintln!("  纪律:密钥绝不写进配置文件、绝不作为命令行参数(进程列表可见)、");
        eprintln!("  绝不进取证档;只从环境变量现取现用。");
        return Err(ChannelTestError::Failed("L0 未过:渠道密钥未齐".to_string()));
    }

    let now = SystemClock.now();
    // ── L1:签名自测(零网络) ────────────────────────────────────────────
    println!();
    println!("[L1] 签名自测(零网络):自签 alipay.trade.precreate 自测报文 → 用支付宝公钥验回");
    let signer =
        EnvRsaSigner::from_material(env.get(SLOT_ALIPAY_MERCHANT_PRIVATE_KEY).expect("L0 已查"))
            .map_err(|e| {
                ChannelTestError::Failed(format!(
                    "L1 失败:商户私钥解析不过,拒绝前进(不碰网关): {e}"
                ))
            })?;
    let verifier =
        EnvRsaVerifier::from_material(env.get(SLOT_ALIPAY_ALIPAY_PUBLIC_KEY).expect("L0 已查"))
            .map_err(|e| {
                ChannelTestError::Failed(format!(
                    "L1 失败:支付宝公钥解析不过,拒绝前进(不碰网关): {e}"
                ))
            })?;
    println!("  ✅ 商户私钥解析成功(算法 RSA2 = SHA256WithRSA)");
    println!("  ✅ 支付宝公钥解析成功");

    // 自测报文:确定性身份(不碰真 app_id、不碰网关),真签名。
    let selftest_cfg = alipay::AlipayRealConfig {
        gateway: "https://selftest.invalid/gateway.do".to_string(),
        app_id: "wanning-channel-test-l1".to_string(),
        agreement_no: String::new(),
        notify_url: None,
    };
    let out_trade_no = format!("wanning_l1_selftest_{}", &date_compact(now)[..8]);
    let outgoing = alipay::build_precreate_request(
        &selftest_cfg,
        &out_trade_no,
        PRECREATE_PROBE_AMOUNT_CENTS,
        "Wanning channel-test L1 签名自测",
        &alipay::beijing_timestamp(now),
        &signer,
    )
    .map_err(|e| ChannelTestError::Failed(format!("L1 失败:自测报文构建被拒(不碰网关): {e}")))?;
    // 复算待签串:从报文拆回参数对(percent 解码)→ 剔除 sign → 官方规则规范化 →
    // 用支付宝公钥验自己的签名。能验过 = 「私钥 ↔ 公钥」配对自洽。
    // 签名覆盖面 = 平台参数(query)+ biz_content(body)——**两边都要拆回**,只拆
    // query 会漏掉 biz_content,待签串与签名覆盖面不一致,自测永远验不过。
    let mut pairs = alipay::query_pairs_of(&outgoing.url);
    pairs.extend(alipay::query_pairs_of(&outgoing.body));
    let sign_b64 = pairs
        .iter()
        .find(|(key, _)| key == "sign")
        .map(|(_, value)| value.clone())
        .ok_or_else(|| {
            ChannelTestError::Failed("L1 失败:自测报文缺 sign(内部一致性错误)".to_string())
        })?;
    let signature = base64::engine::general_purpose::STANDARD
        .decode(sign_b64.as_bytes())
        .map_err(|e| {
            ChannelTestError::Failed(format!("L1 失败:自测报文 sign 不是合法 base64: {e}"))
        })?;
    let rest: Vec<(&str, &str)> = pairs
        .iter()
        .filter(|(key, _)| key != "sign")
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let canonical = canonical_query(&rest)
        .map_err(|e| ChannelTestError::Failed(format!("L1 失败:待签串复算被拒: {e}")))?;
    if !verifier.verify(&canonical, &signature) {
        return Err(ChannelTestError::Failed(
            "L1 不过:用支付宝公钥验自己的签名没验过。最常见原因 = 两个 env 里放的不是\
             同一对密钥(支付宝公钥必须是平台侧那把,不是你自己的应用公钥)。请核对后\
             重跑;全程零网络,网关未被触碰。"
                .to_string(),
        ));
    }
    println!("  ✅ 签名→验签往返通过(out_trade_no={out_trade_no};报文零出网)");
    println!(
        "  L1 过:密钥可用且「商户私钥 ↔ 支付宝公钥」配对自洽。这不证明服务器认——L2 才见真网关。"
    );

    // L1 取证档(脱敏:只有槽位名与待签串,无任何密钥材料)。
    let l1_evidence = format!(
        "Wanning channel-test 取证档(脱敏;密钥材料绝不入档)\n\
         时刻(UTC): {}\n\
         渠道: {}\n\
         账本: {}\n\
         [L0] 槽位(只记 已设/未设)\n\
         [L1] 签名自测(零网络)\n\
         out_trade_no: {out_trade_no}\n\
         method: alipay.trade.precreate(自测报文,gateway=selftest.invalid,零出网)\n\
         待签串(无密钥材料): {canonical}\n\
         结论: 签名→验签往返通过\n",
        format_utc(now),
        options.channel,
        slash(&options.wal),
    );
    let l1_path = write_evidence(&options.evidence_dir, now, "1", &l1_evidence)?;
    evidence.push(l1_path);

    if !options.real {
        println!();
        println!("止步 L1(缺省不带 --real = 只做零网络自测)。");
        println!(
            "下一步(L2 真网关探针,零资金移动)需要同时满足:① --real 显式;\
             ② env WANNING_ALLOW_REAL_SPEND=1;③ 交互终端亲手确认。缺一即拒。"
        );
        return Ok(ProbeOutcome {
            reached: "L1",
            evidence,
        });
    }

    // ── L2:网关探针(真网关,零资金移动) ────────────────────────────────
    println!();
    println!("[L2] 网关探针(真网关,零资金移动)");
    if env.get(guard::ENV_ALLOW_REAL_SPEND) != Some("1") {
        return Err(ChannelTestError::Failed(format!(
            "L2 护栏未开:{} 必须为 1(W-07 真实路径护栏;明示授权,缺省即拒)。\
             ✗ 修复:把它设为 1 后重跑。",
            guard::ENV_ALLOW_REAL_SPEND
        )));
    }
    if !env.is_set(SLOT_ALIPAY_APP_ID) {
        return Err(ChannelTestError::Failed(format!(
            "缺少 {SLOT_ALIPAY_APP_ID}:支付宝开放平台应用 app_id。✗ 修复:设好 app_id 后重跑。"
        )));
    }
    let snapshot = env.guard_snapshot();
    let mut probe_backend = AlipayBackend::from_snapshot_probe(&snapshot)
        .map_err(|e| ChannelTestError::Failed(format!("L2 配置构建被拒(fail-closed): {e}")))?;
    if let Some(transport) = &options.transport {
        probe_backend = probe_backend.with_transport(Arc::clone(transport));
    }
    let signer: Arc<dyn MessageSigner + Send + Sync> = Arc::new(signer);
    let verifier: Arc<dyn SignatureVerifier + Send + Sync> = Arc::new(verifier);
    let mut backend = probe_backend
        .with_signer(Arc::clone(&signer))
        .with_verifier(Arc::clone(&verifier));
    let gateway = backend.endpoint().to_string();
    println!("  网关: {gateway}");

    let now2 = SystemClock.now();
    let out_trade_no = format!("wanning_l2_{}", date_compact(now2));
    let prompt = format!(
        "\n[L2 确认] 即将调用真网关 {gateway}\n接口 alipay.trade.precreate(预创建下单;零资金移动:\
         只生成二维码,买家不扫码即无资金流,探针不扫码)。\n金额 0.01 元(官方 total_amount 最小值),\
         订单号 {out_trade_no}。\n确认? [y/N] "
    );
    if !confirmer.confirm(&prompt)? {
        return Err(ChannelTestError::Failed(
            "用户在交互确认处取消:L2 未执行,零外联零落账".to_string(),
        ));
    }

    // 过账本不豁免:探针意图照落审计(独立探针账本,每日预算 100 分)。
    let mut state = WanningState::live_resuming(&options.wal).map_err(|e| {
        ChannelTestError::Failed(format!(
            "探针账本打开失败(过账本不豁免,探针意图必须落审计): {e}"
        ))
    })?;
    let probe_id = format!("channel-test-probe-{}", &date_compact(now2)[..8]);
    ensure_probe_delegation(&mut state, &probe_id, now2)?;
    let nonce = next_nonce(&state, &probe_id);
    let intent = wanning_core::intent::SpendIntent::new(
        probe_id.clone(),
        nonce,
        PRECREATE_PROBE_AMOUNT_CENTS,
        "alipay:channel-test",
        "channel-test",
        "channel-test L2 网关探针(alipay.trade.precreate,零资金移动)",
    );
    let decision = state
        .decide(&intent)
        .map_err(|e| ChannelTestError::Failed(format!("闸判定失败: {e}")))?;
    match decision {
        GateDecision::Allow { budget_after_cents } => {
            println!(
                "  ✅ 闸放行并落审计(账本 {};探针计 1 分,日累计 {budget_after_cents}/{PROBE_DAILY_BUDGET_CENTS} 分)",
                slash(&options.wal)
            );
        }
        GateDecision::Deny { reason } => {
            return Err(ChannelTestError::Failed(format!(
                "闸拒绝探针意图({reason}):不碰网关。探针预算 = 每日 {PROBE_DAILY_BUDGET_CENTS} 分\
                 (0.01 元 × 100 次),用尽即明天再探;绝不放大探针预算绕过闸。"
            )));
        }
    }

    let call = backend.probe_precreate(
        &out_trade_no,
        PRECREATE_PROBE_AMOUNT_CENTS,
        "Wanning channel-test L2 网关探针",
    );
    let probe = match call {
        Ok(probe) => probe,
        Err(PaymentError::GatewayRejected {
            code,
            sub_code,
            sub_msg,
        }) => {
            // 响应可信(已验签)——这半边已被证明;拒绝原因原样留存,零编造。
            println!("  ✅ 网关响应已验签(拒绝响应同样可信:支付宝公钥与验签链路是对的)");
            println!(
                "  ❌ 网关拒绝:code={code} sub_code={} sub_msg={}",
                sub_code.as_deref().unwrap_or("-"),
                sub_msg.as_deref().unwrap_or("-")
            );
            println!(
                "  判读:响应已验签 = 公钥配对正确;code≠10000 = 网关侧拒绝(签名/权限/参数\
                 类,以 sub_code 为准,官方《公共错误码》表在档可查,此处绝不硬编码映射)。"
            );
            let detail = format!(
                "网关响应(已验签,拒绝):code={code} sub_code={} sub_msg={}",
                sub_code.as_deref().unwrap_or("-"),
                sub_msg.as_deref().unwrap_or("-")
            );
            // 拒绝路径的取证档不含请求原文:parse 层在验签后短路返回,报文半边
            // (request_url/body)不随 Err 带回——这是库层边界,如实记录不补造。
            let text = evidence_l2_rejected(now2, &gateway, &out_trade_no, &detail);
            let path = write_evidence(&options.evidence_dir, now2, "2-rejected", &text)?;
            evidence.push(path);
            return Err(ChannelTestError::Failed(
                "L2 未过:网关拒绝了探针(响应可信,码值原样在取证档)。\
                 诚实边界:闸已放行并记账;渠道拒绝不退预算。"
                    .to_string(),
            ));
        }
        Err(e) => return Err(ChannelTestError::Failed(format!("L2 探针失败: {e}"))),
    };

    println!(
        "  ✅ 网关响应已验签,out_trade_no 对账一致({})",
        probe.out_trade_no
    );
    println!("  prepay_id:{}(预下单成立)", probe.prepay_id);
    println!(
        "  qr_code:{}(内容不打印;买家扫码才产生资金流,探针不扫)",
        if probe.qr_code.is_some() {
            "有"
        } else {
            "无"
        }
    );
    println!(
        "  share_code:{}",
        if probe.share_code.is_some() {
            "有"
        } else {
            "无"
        }
    );
    println!(
        "  L2 过 = 服务器认了签名(W-50「待实签」清单第一格已答)。过 ≠ 能扣款:\
         扣款是协议内扣款(L3),且过账本不豁免——本次探针意图已落审计。"
    );

    let text = evidence_l2(
        now2,
        &gateway,
        &out_trade_no,
        &probe,
        "结论: 服务器认了签名(预下单成立);过 ≠ 能扣款。",
    );
    let path = write_evidence(&options.evidence_dir, now2, "2", &text)?;
    evidence.push(path);

    if !options.real_spend {
        println!();
        println!(
            "止步 L2。L3(真实扣款 0.01 元)需要:① 在 --real 之外再显式给 --real-spend;\
             ② 专属确认文案的交互确认;③ {} + W-07 全链护栏(env 四密钥)齐。\
             本命令绝不替你跳这一步。",
            SLOT_ALIPAY_AGREEMENT_NO
        );
        return Ok(ProbeOutcome {
            reached: "L2",
            evidence,
        });
    }

    // ── L3:协议内真实扣款(真金白银,不可撤销) ───────────────────────────
    println!();
    println!("[L3] 协议内真实扣款(真金白银,不可撤销)");
    if !env.is_set(SLOT_ALIPAY_AGREEMENT_NO) {
        return Err(ChannelTestError::Failed(format!(
            "缺少 {SLOT_ALIPAY_AGREEMENT_NO}:用户签约协议号(协议内扣款凭证;没有协议号的\
             扣款=裸转账,绝不发)。✗ 修复:先完成平台侧签约拿到协议号,再设进该环境变量。"
        )));
    }
    let prompt = format!(
        "\n[L3 确认] 即将真实扣款 0.01 元(协议内扣款,本人账户;接口 alipay.trade.pay)。\n\
         协议号来自 {SLOT_ALIPAY_AGREEMENT_NO}(值不回显);订单号 wanning_l3_{};\n\
         本命令不做任何自动重试,扣款不可撤销。\n确认? [y/N] ",
        date_compact(SystemClock.now())
    );
    if !confirmer.confirm(&prompt)? {
        return Err(ChannelTestError::Failed(
            "用户在交互确认处取消:L3 未执行,零真实扣款".to_string(),
        ));
    }
    // 真实路径配置:W-07 全链护栏 + 协议号(from_snapshot_real,GuardDenied 原文照印)。
    let mut real_backend = AlipayBackend::from_snapshot_real(&snapshot)
        .map_err(|e| ChannelTestError::Failed(format!("L3 配置被拒(fail-closed,零扣款): {e}")))?;
    if let Some(transport) = &options.transport {
        real_backend = real_backend.with_transport(Arc::clone(transport));
    }
    let mut real_backend = real_backend.with_signer(signer).with_verifier(verifier);

    let now3 = SystemClock.now();
    let nonce3 = next_nonce(&state, &probe_id);
    let order_id = format!("wanning_l3_{}", date_compact(now3));
    let intent3 = wanning_core::intent::SpendIntent::new(
        probe_id.clone(),
        nonce3,
        PRECREATE_PROBE_AMOUNT_CENTS,
        "alipay:channel-test",
        "channel-test",
        "channel-test L3 协议内真实扣款 0.01 元(alipay.trade.pay)",
    );
    let decision3 = state
        .decide(&intent3)
        .map_err(|e| ChannelTestError::Failed(format!("闸判定失败: {e}")))?;
    let (nonce3, budget_after) = match decision3 {
        GateDecision::Allow { budget_after_cents } => {
            println!(
                "  ✅ 闸放行并落审计(L3 计 1 分,日累计 {budget_after_cents}/{PROBE_DAILY_BUDGET_CENTS} 分)"
            );
            (nonce3, budget_after_cents)
        }
        GateDecision::Deny { reason } => {
            return Err(ChannelTestError::Failed(format!(
                "闸拒绝 L3 意图({reason}):零扣款。日预算 {PROBE_DAILY_BUDGET_CENTS} 分内,\
                 明天再试或显式调整授权。"
            )));
        }
    };
    let request = PayRequest {
        order_id: order_id.clone(),
        amount_cents: PRECREATE_PROBE_AMOUNT_CENTS,
        delegation_id: probe_id.clone(),
        intent_nonce: nonce3,
    };
    let outcome = real_backend.trigger_pay(&request);
    let result_line = match &outcome {
        Ok(paid) => format!(
            "扣款完成: out_request_no={} trade_no={} status={:?} amount={}分",
            paid.out_request_no, paid.trade_no, paid.status, paid.amount_cents
        ),
        Err(e) => format!("扣款失败: {e}"),
    };
    let text = format!(
        "[L3] 协议内真实扣款(alipay.trade.pay)\n\
         网关: {gateway}\n\
         意图: delegation={probe_id} nonce={nonce3} amount=1分 merchant=alipay:channel-test\n\
         闸判定: 放行(预算后累计 {budget_after}/{PROBE_DAILY_BUDGET_CENTS} 分)\n\
         订单号: {order_id}\n\
         {result_line}\n\
         状态映射(async_payment_mode)为模板决策非官方规定,实跑时核对。\n"
    );
    let path = write_evidence(&options.evidence_dir, now3, "3", &text)?;
    evidence.push(path);

    match outcome {
        Ok(paid) => {
            match paid.status {
                alipay::PayStatus::Success => {
                    println!(
                        "  ✅ 同步直扣完成(无 async_payment_mode / SYNC_DIRECT_PAY,模板决策)。"
                    );
                }
                alipay::PayStatus::Pending => {
                    println!("  ⏳ 异步受理:终态以已验签回调为准(notify_url;本命令不等待回调)。");
                }
                alipay::PayStatus::Failed => {
                    println!("  ❌ 渠道侧终态失败(以渠道与回调为准)。");
                }
            }
            println!("  过账本不豁免:本笔意图已落审计;闸语义没有退款/额度释放(W-29)。");
        }
        Err(e) => {
            return Err(ChannelTestError::Failed(format!(
                "L3 扣款失败: {e}\n诚实边界:闸已放行并记账,渠道报错不退预算(以在档审计为准)。"
            )));
        }
    }
    Ok(ProbeOutcome {
        reached: "L3",
        evidence,
    })
}

/// 探针委托注册:每日一条 `channel-test-probe-<yyyymmdd>`(预算 100 分,24h,
/// nonce_scope = 委托 id)。已注册则跳过;回放接续时重复注册 = 篡改审计,容忍
/// DuplicateDelegation 语义(W-17「已注册则跳过」同一纪律)。
fn ensure_probe_delegation(
    state: &mut WanningState,
    probe_id: &str,
    now: u64,
) -> Result<(), ChannelTestError> {
    if state.gate().delegation(probe_id).is_some() {
        return Ok(());
    }
    let delegation = wanning_core::delegation::Delegation::new(
        probe_id,
        "所有者",
        "channel-test",
        PROBE_DAILY_BUDGET_CENTS,
        now,
        now + 86_400,
        probe_id,
    );
    match state.register_delegation(delegation) {
        Ok(()) | Err(CoreError::DuplicateDelegation(_)) => Ok(()),
        Err(e) => Err(ChannelTestError::Failed(format!("探针委托注册失败: {e}"))),
    }
}

/// nonce 分配:作用域内已消耗最大 nonce + 1(空作用域从 1 起)——与 wanning-sdk
/// 同一口径;被拒不耗号,重试同号。
fn next_nonce(state: &WanningState, nonce_scope: &str) -> u64 {
    state
        .gate()
        .replay_registry()
        .iter()
        .filter(|(scope, _)| scope.as_str() == nonce_scope)
        .map(|(_, nonce)| *nonce)
        .max()
        .unwrap_or(0)
        + 1
}

/// 取证档写入(文件名 `<yyyymmddHHMMSS>-L<级>.txt`;目录不存在则建)。密钥材料
/// 由调用方保证不进内容——本模块的取证串只放槽位名/待签串/已验签报文。
fn write_evidence(
    dir: &Path,
    now: u64,
    level: &str,
    content: &str,
) -> Result<PathBuf, ChannelTestError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| ChannelTestError::Failed(format!("取证目录创建失败 {}:{e}", slash(dir))))?;
    let path = dir.join(format!("{}-L{level}.txt", date_compact(now)));
    std::fs::write(&path, content)
        .map_err(|e| ChannelTestError::Failed(format!("取证档写入失败 {}:{e}", slash(&path))))?;
    Ok(path)
}

/// L2 取证档(脱敏:`app_id`/`sign` 打码;biz_content 解码原文;响应原文)。
fn evidence_l2(
    now: u64,
    gateway: &str,
    out_trade_no: &str,
    probe: &PrecreateProbe,
    verdict: &str,
) -> String {
    let mut text = String::new();
    text.push_str(&format!(
        "[L2] 网关探针(alipay.trade.precreate;零资金移动)\n时刻(UTC): {}\n网关: {gateway}\n订单号: {out_trade_no}\n金额: 0.01 元(官方 total_amount 最小值;探针不扫码)\n",
        format_utc(now)
    ));
    text.push_str("请求 query(脱敏:app_id/sign → ***):\n");
    for (key, value) in alipay::query_pairs_of(&probe.request_url) {
        let shown = if key == "app_id" || key == "sign" {
            "***".to_string()
        } else {
            value
        };
        text.push_str(&format!("  {key}={shown}\n"));
    }
    if let Some((_, body)) = alipay::query_pairs_of(&probe.request_body)
        .into_iter()
        .next()
    {
        text.push_str(&format!("biz_content(解码后):\n  {body}\n"));
    }
    text.push_str("网关响应(已验签,原文):\n");
    text.push_str(&format!("  {}\n", probe.verified_body));
    text.push_str(&format!("{verdict}\n"));
    text
}

/// L2 拒绝路径的取证档(网关拒绝时报文半边不随 Err 带回,只留响应码原文)。
fn evidence_l2_rejected(now: u64, gateway: &str, out_trade_no: &str, detail: &str) -> String {
    format!(
        "[L2] 网关探针(alipay.trade.precreate;零资金移动)——被网关拒绝\n\
         时刻(UTC): {}\n\
         网关: {gateway}\n\
         订单号: {out_trade_no}\n\
         金额: 0.01 元(官方 total_amount 最小值;探针不扫码)\n\
         {detail}\n\
         判读: 响应已验签 = 公钥配对正确;code≠10000 = 网关侧拒绝,原因以 sub_code \
         为准(官方《公共错误码》表在档),本文件绝不硬编码语义映射。\n\
         边界: 请求报文原文在拒绝路径不带回(parse 层短路),此处零补造。\n",
        format_utc(now)
    )
}

/// `format_utc` → `yyyymmddHHMMSS`(文件名/订单号用;零依赖 UTC 推导,复用 W-22)。
fn date_compact(now: u64) -> String {
    format_utc(now).replace(['-', ' ', ':'], "")
}

fn next_value<'a>(
    args: &'a [String],
    index: &mut usize,
    flag: &str,
) -> Result<&'a str, ChannelTestError> {
    *index += 1;
    args.get(*index)
        .map(String::as_str)
        .ok_or_else(|| ChannelTestError::Usage(format!("缺少 {flag} 的值(用法见 --help)")))
}
