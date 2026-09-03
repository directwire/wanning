//! W-52 `wanning channel-test` 的产品契约:渠道钥匙验证阶梯(L0→L1→L2→L3,绝不跳级)。
//!
//! 任务书 `Wanning-oss/W-52-channel-test.md`(定位注记:本命令是**免密代扣(平台
//! 侧,第二形式)**的钥匙验证工具,个人用户旅程用不到)。本文件锁七件事:
//! ① 三重明示 fail-closed 缺一即拒:无护栏 env / 无 `--real` / 非 TTY(L3 追加
//!    第四重 `--real-spend`),四条拒绝路径各有测试;
//! ② 缺省(无 `--real`)= 只到 L1,**零网络零落账**:传输替身捕获 0 请求 + 探针
//!    账本文件不存在 = 直接证据,stdout 契约同步锁定;
//! ③ L1 签名自测:现场自生成测试 RSA 密钥对(W-28/W-50 先例,自生成自销毁,
//!    绝不真密钥绝不落仓)真签真验;「私钥/公钥不是一对」当场现形且零出网;
//!    裸 base64 DER 与 PEM 外壳两种密钥形态都收(W-52 parse 的分派面);
//! ④ L2 网关探针报文同源:method=alipay.trade.precreate、product_code=
//!    FACE_TO_FACE_PAYMENT、金额 0.01 元(官方 total_amount 最小值)、无协议号
//!    语义——全部复用 W-50 官方模板管线,不另写字段面;响应验签共用信封管线
//!    (qr_code 带 `\/` 转义复刻官方 02mse7 示例,压验签的转义重试半边);
//! ⑤ 过账本不豁免:探针意图落独立探针账本(闸放行 + nonce 消耗);闸放行在网关
//!    调用之前——网关拒绝路径账本照记(渠道拒绝不退预算),确认拒绝路径零落账;
//! ⑥ 取证档脱敏:app_id/sign 打码,密钥材料绝不入档(档内无密钥材料断言);
//! ⑦ 用法面与诚实标注:未知渠道/已知不支持渠道(jd/wechat/meituan,原因照读
//!    `UNSUPPORTED_CHANNELS`)/`--real-spend` 单给 = 用法错(退出码 2)。
//!
//! 测试纪律(任务书):全部打本地替身网关([`MockGateway`]),零真网关零真实消费;
//! 交互确认走 [`ScriptConfirmer`] 脚本替身;子进程测试显式剥离再注入全部槽位 env、
//! stdin 恒为 null(非 TTY = 第三重明示的天然测试床)、`--wal`/`--evidence` 恒显式
//! 指向临时目录(绝不触碰真实家目录与真实账本);进程 env 注入口
//! (`ProbeEnv::from_process_env`)只经子进程测试覆盖,绝不与并行测试抢进程 env。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey};
use serde_json::{json, Value};

use wanning_cli::channel_test::{
    run_probe, ChannelTestError, Confirmer, ProbeEnv, ProbeOptions, SLOT_ALIPAY_AGREEMENT_NO,
    SLOT_ALIPAY_ALIPAY_PUBLIC_KEY, SLOT_ALIPAY_APP_ID, SLOT_ALIPAY_ENDPOINT,
    SLOT_ALIPAY_MERCHANT_PRIVATE_KEY, SUPPORTED_CHANNELS, UNSUPPORTED_CHANNELS,
};
use wanning_core::state::WanningState;
use wanning_demo::guard::{self, ENV_ALLOW_REAL_SPEND};
use wanning_demo::http::{ApiTransport, HttpFailure};
use wanning_demo::signing::{ENV_ALIPAY_PUBLIC_KEY, ENV_MERCHANT_PRIVATE_KEY};

const WAN: &str = env!("CARGO_BIN_EXE_wanning");
/// 替身网关地址(`.invalid` 顶域:真解析必然失败——替身注入才是唯一入口,双保险)。
const MOCK_GATEWAY: &str = "https://mock-gateway.w52.invalid/gateway.do";
const APP_ID: &str = "2021000100000000";
const AGREEMENT_NO: &str = "20170322450983769228";

// ---------------------------------------------------------------------------
// 测试夹具:临时目录 / 测试密钥对 / 替身网关 / 脚本确认器
// ---------------------------------------------------------------------------

fn temp_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "w52-channel-test-{}-{}-{}-{}",
        tag,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间早于 Unix 纪元")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

/// 现场生成的测试 RSA 密钥对(W-28/W-50 先例):只在本进程内存存活,测试结束即
/// 销毁——绝不真商户密钥,绝不落仓。材料缺省形态 = 裸 base64 DER(支付宝密钥
/// 工具的常见复制形态;私钥 PKCS#8 / 公钥 SPKI)。
struct TestKeys {
    /// 扮演「支付宝侧」的私钥:替身网关用它签响应,env 里的「支付宝公钥」
    /// 就是它的公钥半边——签名/验签必须同钥,与真实链路同构。
    alipay_side: rsa::RsaPrivateKey,
    merchant_private: String,
    alipay_public: String,
}

fn test_keys() -> TestKeys {
    use rand::rngs::OsRng;
    let private = rsa::RsaPrivateKey::new(&mut OsRng, 2048).expect("现场生成测试 RSA 密钥对");
    let private_der = private.to_pkcs8_der().expect("导出 PKCS#8 DER");
    let public_der = private
        .to_public_key()
        .to_public_key_der()
        .expect("导出 SPKI DER");
    TestKeys {
        alipay_side: private.clone(),
        merchant_private: B64.encode(private_der.as_bytes()),
        alipay_public: B64.encode(public_der.as_bytes()),
    }
}

/// 裸 base64 DER → PEM 外壳(64 列折行;覆盖 parse 的 PEM 分派半边)。
fn wrap_pem(label: &str, b64: &str) -> String {
    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).expect("base64 是 ASCII"));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

/// L1 槽位(签名自测前置密钥)。
fn l1_env(keys: &TestKeys) -> ProbeEnv {
    let mut env = ProbeEnv::default();
    env.set(SLOT_ALIPAY_MERCHANT_PRIVATE_KEY, &keys.merchant_private);
    env.set(SLOT_ALIPAY_ALIPAY_PUBLIC_KEY, &keys.alipay_public);
    env
}

/// L2 槽位 = L1 + app_id + 网关覆盖(替身)+ 护栏开关(可选)。
fn l2_env(keys: &TestKeys, allow_real_spend: bool) -> ProbeEnv {
    let mut env = l1_env(keys);
    env.set(SLOT_ALIPAY_APP_ID, APP_ID);
    env.set(SLOT_ALIPAY_ENDPOINT, MOCK_GATEWAY);
    if allow_real_spend {
        env.set(ENV_ALLOW_REAL_SPEND, "1");
    }
    env
}

/// L3 槽位 = L2 + 协议号 + W-07 全链护栏四密钥(`from_snapshot_real` 的门槛)。
fn l3_env(keys: &TestKeys) -> ProbeEnv {
    let mut env = l2_env(keys, true);
    env.set(SLOT_ALIPAY_AGREEMENT_NO, AGREEMENT_NO);
    for key in guard::REQUIRED_KEYS {
        env.set(key, "w52-test-guard-key");
    }
    env
}

/// 本地替身网关:捕获 (url, body),按脚本回放**已签名**的信封响应(测试密钥
/// 扮演支付宝侧签名)。零真网关零真实消费。
/// (`ApiTransport` 要求 `Debug`;rsa 私钥的 Debug 不打印密钥材料,照常落格式。)
#[derive(Debug)]
struct MockGateway {
    alipay_side: rsa::RsaPrivateKey,
    captured: Mutex<Vec<(String, String)>>,
    script: Mutex<Vec<Script>>,
}

/// 脚本步:这一次网关调用回什么形态的响应。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Script {
    /// precreate 成功(code=10000 + prepay_id/qr_code;qr_code 故意带 `\/` 转义,
    /// 验签按未转义原文签——复刻官方 02mse7 示例形态,顺带压验签的转义重试半边)。
    PrecreateOk,
    /// precreate 被拒(code≠10000 带子码;响应已验签 = GatewayRejected 路径)。
    /// 码值是**测试替身的罐头值**:断言只锁「原样透传」,不预设官方语义。
    PrecreateRejected,
    /// trade.pay 成功(W-50 官方响应示例字段面;金额/订单号从请求 biz 原样回显)。
    PayOk,
}

impl MockGateway {
    fn new(alipay_side: rsa::RsaPrivateKey, script: &[Script]) -> Arc<Self> {
        Arc::new(Self {
            alipay_side,
            captured: Mutex::new(Vec::new()),
            // 反转存储:pop() 从尾端出,保持按构造序回放。
            script: Mutex::new(script.iter().rev().copied().collect()),
        })
    }

    fn captured(&self) -> Vec<(String, String)> {
        self.captured.lock().expect("捕获锁").clone()
    }

    fn sign(&self, inner: &str) -> String {
        use rsa::signature::{SignatureEncoding as _, Signer as _};
        let signing_key =
            rsa::pkcs1v15::SigningKey::<rsa::sha2::Sha256>::new(self.alipay_side.clone());
        B64.encode(signing_key.sign(inner.as_bytes()).to_vec())
    }
}

impl ApiTransport for MockGateway {
    fn post_json(
        &self,
        url: &str,
        body: &str,
        _headers: &[(String, String)],
    ) -> Result<String, HttpFailure> {
        self.captured
            .lock()
            .expect("捕获锁")
            .push((url.to_string(), body.to_string()));
        let script = self
            .script
            .lock()
            .expect("脚本锁")
            .pop()
            .ok_or_else(|| HttpFailure {
                status: None,
                timeout: false,
                message: "替身网关无更多脚本步".to_string(),
            })?;
        let biz = request_biz(body);
        let out_trade_no = biz["out_trade_no"]
            .as_str()
            .expect("请求 biz 带 out_trade_no");
        let total_amount = biz["total_amount"]
            .as_str()
            .expect("请求 biz 带 total_amount");
        let (member, inner) = match script {
            Script::PrecreateOk => (
                "alipay_trade_precreate_response",
                json!({
                    "code": "10000",
                    "msg": "Success",
                    "out_trade_no": out_trade_no,
                    "prepay_id": "w52-test-prepay-id",
                    "qr_code": "https://qr.alipay.com/w52-test-probe-qr",
                })
                .to_string(),
            ),
            Script::PrecreateRejected => (
                "alipay_trade_precreate_response",
                json!({
                    "code": "40004",
                    "msg": "Business Failed",
                    "sub_code": "W52_TEST_REJECTED",
                    "sub_msg": "测试替身网关的拒绝响应(已验签)",
                })
                .to_string(),
            ),
            Script::PayOk => (
                "alipay_trade_pay_response",
                json!({
                    "code": "10000",
                    "msg": "Success",
                    "trade_no": "W52_TEST_TRADE_NO",
                    "out_trade_no": out_trade_no,
                    "buyer_logon_id": "159****5620",
                    "total_amount": total_amount,
                    "gmt_payment": "2026-09-03 12:00:00",
                })
                .to_string(),
            ),
        };
        // PrecreateOk 的 qr_code 在 wire 上带 `\/` 转义(官方示例形态),签名按
        // 未转义原文算——第一遍验签不过、转义重试才过,复刻官方响应形态。
        let (signed_inner, wire_inner) = if script == Script::PrecreateOk {
            (inner.clone(), inner.replace('/', "\\/"))
        } else {
            (inner.clone(), inner)
        };
        let sign = self.sign(&signed_inner);
        Ok(format!(r#"{{"{member}":{wire_inner},"sign":"{sign}"}}"#))
    }
}

/// 从替身捕获的请求体剥出 biz JSON(body = `biz_content=<URL 编码 JSON>`)。
fn request_biz(body: &str) -> Value {
    let encoded = body
        .strip_prefix("biz_content=")
        .expect("请求体是 biz_content 表单");
    serde_json::from_str(&percent_decode(encoded)).expect("biz_content 是合法 JSON")
}

/// 测试用 URL 解码(与实现同一 RFC 3986 规则:实现从不产生 `+`,这里不按空格解)。
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).expect("ASCII 前缀");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).expect("报文是 UTF-8")
}

/// 交互确认的脚本替身:按序回放是/否,同时录下每一条确认提示(「确认提示必须
/// 回显真实网关域名与接口名」纪律的断言面)。
struct ScriptConfirmer {
    answers: Mutex<Vec<bool>>,
    prompts: Mutex<Vec<String>>,
}

impl ScriptConfirmer {
    fn new(answers: &[bool]) -> Self {
        Self {
            // 反转存储:pop() 从尾端出,保持按构造序回答。
            answers: Mutex::new(answers.iter().rev().copied().collect()),
            prompts: Mutex::new(Vec::new()),
        }
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().expect("提示锁").clone()
    }
}

impl Confirmer for ScriptConfirmer {
    fn confirm(&mut self, prompt: &str) -> Result<bool, ChannelTestError> {
        self.prompts
            .lock()
            .expect("提示锁")
            .push(prompt.to_string());
        self.answers
            .lock()
            .expect("应答锁")
            .pop()
            .ok_or_else(|| ChannelTestError::Failed("脚本替身应答已用尽".to_string()))
    }
}

/// 组装 [`ProbeOptions`](账本/取证档恒指临时目录;传输替身 = 测试唯一入口)。
fn options(
    env: ProbeEnv,
    wal: &Path,
    evidence: &Path,
    real: bool,
    real_spend: bool,
    transport: Option<Arc<dyn ApiTransport + Send + Sync>>,
) -> ProbeOptions {
    ProbeOptions {
        channel: "alipay".to_string(),
        wal: wal.to_path_buf(),
        evidence_dir: evidence.to_path_buf(),
        real,
        real_spend,
        env,
        transport,
    }
}

fn evidence_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("取证目录已建")
        .map(|entry| {
            entry
                .expect("读目录项")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("读取文本档")
}

fn failed_message(err: &ChannelTestError) -> &str {
    match err {
        ChannelTestError::Failed(message) => message,
        ChannelTestError::Usage(message) => {
            panic!("应为运行失败(退出码 1),拿到用法错: {message}")
        }
    }
}

// ---------------------------------------------------------------------------
// 子进程测试:env 注入口 + 退出码 + stdout 契约 + 非 TTY 门
// ---------------------------------------------------------------------------

/// 子进程要剥离的槽位 env(测试结果必须与宿主机真实密钥/配置状态无关)。
const STRIPPED: &[&str] = &[
    "WANNING_HOME",
    ENV_ALLOW_REAL_SPEND,
    ENV_MERCHANT_PRIVATE_KEY,
    ENV_ALIPAY_PUBLIC_KEY,
    "WANNING_ALIPAY_APP_ID",
    "WANNING_ALIPAY_AGREEMENT_NO",
    "WANNING_ALIPAY_ENDPOINT",
    "WANNING_ALIPAY_NOTIFY_URL",
    "WANNING_GLM_KEY",
    "WANNING_JD_APP_KEY",
    "WANNING_JD_APP_SECRET",
    "WANNING_JD_ACCESS_TOKEN",
];

/// 跑 `wanning channel-test …`:剥离一切槽位 env 再按需注入;stdin 恒为 null
/// (非 TTY = 第三重明示的天然测试床,真网关探针在无交互终端时必然 fail-closed)。
fn run(args: &[&str], cwd: &Path, envs: &[(&str, &str)]) -> (i32, String, String) {
    let mut command = Command::new(WAN);
    command.args(args).current_dir(cwd).stdin(Stdio::null());
    for key in STRIPPED {
        command.env_remove(key);
    }
    for (key, value) in envs {
        command.env(key, value);
    }
    let out = command.output().expect("spawn wanning");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn usage_errors_exit_2_and_help_exits_0() {
    let dir = temp_dir("usage");
    let wal = dir.join("wal.jsonl");
    let evidence = dir.join("evidence");
    let wal_arg = wal.to_string_lossy().into_owned();
    let evidence_arg = evidence.to_string_lossy().into_owned();
    let base = [
        "channel-test",
        "--channel",
        "alipay",
        "--wal",
        wal_arg.as_str(),
        "--evidence",
        evidence_arg.as_str(),
    ];

    // --help:退出 0,阶梯/三重明示/诚实标注/密钥纪律齐全。
    let (code, stdout, _) = run(&["channel-test", "--help"], &dir, &[]);
    assert_eq!(code, 0, "{stdout}");
    for marker in [
        "L0",
        "L1",
        "L2",
        "L3",
        "绝不跳级",
        "缺一即拒",
        "零资金移动",
        "真实扣款 0.01 元",
        "京东",
        "微信",
        "美团",
        "绝不落盘",
    ] {
        assert!(stdout.contains(marker), "--help 要含「{marker}」:{stdout}");
    }

    // 用法错 = 退出码 2:缺 --channel / 未知参数 / 缺取值 / 未知渠道 / --real-spend 单给。
    let usage_cases: Vec<Vec<&str>> = vec![
        vec!["channel-test"],
        vec!["channel-test", "--nonsense"],
        vec!["channel-test", "--channel"],
        vec!["channel-test", "--channel", "paypal"],
        vec![
            "channel-test",
            "--channel",
            "alipay",
            "--real-spend",
            "--wal",
            "x.jsonl",
        ],
        base.iter().copied().chain(["--nonsense"]).collect(),
    ];
    for args in &usage_cases {
        let (code, _, stderr) = run(args, &dir, &[]);
        assert_eq!(code, 2, "用法错要退出 2:{args:?} → {stderr}");
    }
    // 缺 --channel 的报错要点名支持面。
    let (_, _, stderr) = run(&["channel-test"], &dir, &[]);
    assert!(stderr.contains("alipay"), "报错要点名支持渠道:{stderr}");
}

#[test]
fn unsupported_channels_are_refused_with_honest_reasons() {
    let dir = temp_dir("unsupported");
    assert_eq!(
        SUPPORTED_CHANNELS.to_vec(),
        vec!["alipay"],
        "当前唯一支持渠道 = alipay"
    );
    for (name, reason) in UNSUPPORTED_CHANNELS {
        let (code, _, stderr) = run(&["channel-test", "--channel", name], &dir, &[]);
        assert_eq!(code, 2, "{name} = 用法错:{stderr}");
        assert!(stderr.contains("暂不支持"), "{stderr}");
        assert!(
            stderr.contains(reason),
            "{name} 的诚实原因要进报错:{stderr}"
        );
    }
}

#[test]
fn l0_missing_keys_stop_before_any_io() {
    let dir = temp_dir("l0");
    let wal = dir.join("wal.jsonl");
    let evidence = dir.join("evidence");
    let (code, stdout, stderr) = run(
        &[
            "channel-test",
            "--channel",
            "alipay",
            "--wal",
            wal.to_string_lossy().as_ref(),
            "--evidence",
            evidence.to_string_lossy().as_ref(),
        ],
        &dir,
        &[],
    );
    assert_eq!(code, 1, "缺 L1 密钥 = 运行失败:{stdout}{stderr}");
    assert!(stderr.contains("L1 前置密钥未齐"), "{stderr}");
    assert!(stderr.contains(ENV_MERCHANT_PRIVATE_KEY), "{stderr}");
    assert!(stderr.contains(ENV_ALIPAY_PUBLIC_KEY), "{stderr}");
    assert!(stderr.contains("修复"), "缺项给 ✗ 修复指引:{stderr}");
    assert!(
        !stdout.contains("[L1] 签名自测"),
        "L0 未过不得进 L1:{stdout}"
    );
    assert!(!evidence.exists(), "L0 未过零落档:{evidence:?}");
    assert!(!wal.exists(), "L0 未过零落账:{wal:?}");
}

#[test]
fn default_without_real_stops_at_l1_with_locked_stdout() {
    let dir = temp_dir("l1-contract");
    let keys = test_keys();
    let wal = dir.join("wal.jsonl");
    let evidence = dir.join("evidence");
    let envs = [
        (ENV_MERCHANT_PRIVATE_KEY, keys.merchant_private.as_str()),
        (ENV_ALIPAY_PUBLIC_KEY, keys.alipay_public.as_str()),
    ];
    let (code, stdout, stderr) = run(
        &[
            "channel-test",
            "--channel",
            "alipay",
            "--wal",
            wal.to_string_lossy().as_ref(),
            "--evidence",
            evidence.to_string_lossy().as_ref(),
        ],
        &dir,
        &envs,
    );
    assert_eq!(code, 0, "缺省只到 L1 = 成功:{stdout}{stderr}");
    // stdout 契约:两级阶梯 + 明确的止步语 + 零出网声明 + 下一步指引。
    assert!(stdout.contains("[L0]"), "{stdout}");
    assert!(stdout.contains("[L1]"), "{stdout}");
    assert!(!stdout.contains("[L2] 网关探针"), "缺省不得进 L2:{stdout}");
    assert!(stdout.contains("签名→验签往返通过"), "{stdout}");
    assert!(stdout.contains("止步 L1"), "{stdout}");
    assert!(stdout.contains("--real"), "止步语要给下一步指引:{stdout}");
    // 零落账:缺省(无 --real)不碰探针账本。
    assert!(!wal.exists(), "L1 零网络零落账:{wal:?}");
    // 取证档:恰好一份 L1 档,且密钥材料绝不入档。
    let names = evidence_files(&evidence);
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names[0].ends_with("-L1.txt"), "{names:?}");
    let text = read(&evidence.join(&names[0]));
    assert!(text.contains("签名自测"), "{text}");
    assert!(
        !text.contains(&keys.merchant_private),
        "密钥材料绝不入档:{text}"
    );
}

#[test]
fn non_tty_stdin_fails_closed_at_l2_confirm() {
    let dir = temp_dir("non-tty");
    let keys = test_keys();
    let wal = dir.join("wal.jsonl");
    let evidence = dir.join("evidence");
    // 护栏开、app_id 齐:一路走到 L2 交互确认,非 TTY 在那里 fail-closed。
    let envs = [
        (ENV_MERCHANT_PRIVATE_KEY, keys.merchant_private.as_str()),
        (ENV_ALIPAY_PUBLIC_KEY, keys.alipay_public.as_str()),
        ("WANNING_ALIPAY_APP_ID", APP_ID),
        ("WANNING_ALIPAY_ENDPOINT", MOCK_GATEWAY),
        (ENV_ALLOW_REAL_SPEND, "1"),
    ];
    let (code, stdout, stderr) = run(
        &[
            "channel-test",
            "--channel",
            "alipay",
            "--real",
            "--wal",
            wal.to_string_lossy().as_ref(),
            "--evidence",
            evidence.to_string_lossy().as_ref(),
        ],
        &dir,
        &envs,
    );
    assert_eq!(code, 1, "非 TTY = 第三重明示拒:{stdout}{stderr}");
    assert!(stderr.contains("非交互环境"), "{stderr}");
    assert!(stderr.contains("fail-closed"), "{stderr}");
    // 确认门在网关调用与账本打开之前:零落账,取证档只有 L1。
    assert!(!wal.exists(), "确认之前零落账:{wal:?}");
    assert_eq!(evidence_files(&evidence).len(), 1, "只有 L1 档");
}

// ---------------------------------------------------------------------------
// 进程内测试:替身网关 + 脚本确认器,阶梯逐级锁
// ---------------------------------------------------------------------------

#[test]
fn l1_only_never_touches_transport_or_ledger() {
    let dir = temp_dir("l1-inproc");
    let keys = test_keys();
    let wal = dir.join("wal.jsonl");
    let evidence = dir.join("evidence");
    let gateway = MockGateway::new(keys.alipay_side.clone(), &[]);
    let mut options = options(
        l1_env(&keys),
        &wal,
        &evidence,
        false,
        false,
        Some(gateway.clone()),
    );
    let mut confirmer = ScriptConfirmer::new(&[]);

    let outcome = run_probe(&mut options, &mut confirmer).expect("L1 应过");
    assert_eq!(outcome.reached, "L1");
    assert_eq!(
        outcome.evidence.len(),
        1,
        "只有 L1 档:{:?}",
        outcome.evidence
    );
    // 零出网直接证据:传输替身一次都没被调。
    assert!(
        gateway.captured().is_empty(),
        "缺省(无 --real)绝不碰传输层:{:?}",
        gateway.captured()
    );
    assert!(!wal.exists(), "L1 零落账:{wal:?}");
    assert!(confirmer.prompts().is_empty(), "L1 不问确认");
}

#[test]
fn l1_mismatched_keypair_fails_closed_with_zero_network() {
    let dir = temp_dir("l1-mismatch");
    let keys_a = test_keys();
    let keys_b = test_keys();
    let mut env = l1_env(&keys_a);
    env.set(SLOT_ALIPAY_ALIPAY_PUBLIC_KEY, &keys_b.alipay_public);
    let wal = dir.join("wal.jsonl");
    let evidence = dir.join("evidence");
    let gateway = MockGateway::new(keys_a.alipay_side.clone(), &[]);
    let mut options = options(env, &wal, &evidence, false, false, Some(gateway.clone()));
    let mut confirmer = ScriptConfirmer::new(&[]);

    let err = run_probe(&mut options, &mut confirmer).expect_err("配对不齐必须拒");
    let message = failed_message(&err);
    assert!(message.contains("L1 不过"), "{message}");
    assert!(message.contains("同一对密钥"), "{message}");
    assert!(message.contains("零网络"), "{message}");
    // 零出网零落盘:自测失败发生在任何 IO 之前。
    assert!(gateway.captured().is_empty(), "L1 失败绝不碰网关");
    assert!(!wal.exists(), "{wal:?}");
    assert!(!evidence.exists(), "失败零落盘:{evidence:?}");
}

#[test]
fn pem_wrapped_key_material_is_accepted_too() {
    let dir = temp_dir("l1-pem");
    let keys = test_keys();
    let mut env = l1_env(&keys);
    env.set(
        SLOT_ALIPAY_MERCHANT_PRIVATE_KEY,
        &wrap_pem("PRIVATE KEY", &keys.merchant_private),
    );
    env.set(
        SLOT_ALIPAY_ALIPAY_PUBLIC_KEY,
        &wrap_pem("PUBLIC KEY", &keys.alipay_public),
    );
    let mut options = options(
        env,
        &dir.join("wal.jsonl"),
        &dir.join("evidence"),
        false,
        false,
        None,
    );
    let mut confirmer = ScriptConfirmer::new(&[]);
    let outcome = run_probe(&mut options, &mut confirmer).expect("PEM 形态应被收");
    assert_eq!(outcome.reached, "L1");
}

#[test]
fn l2_requires_guard_env_even_with_real_flag() {
    let dir = temp_dir("l2-no-guard");
    let keys = test_keys();
    let wal = dir.join("wal.jsonl");
    let evidence = dir.join("evidence");
    let gateway = MockGateway::new(keys.alipay_side.clone(), &[]);
    // --real 给了但护栏 env 没开 = 第二重明示缺 → L2 门口即拒。
    let mut options = options(
        l2_env(&keys, false),
        &wal,
        &evidence,
        true,
        false,
        Some(gateway.clone()),
    );
    let mut confirmer = ScriptConfirmer::new(&[]);

    let err = run_probe(&mut options, &mut confirmer).expect_err("护栏未开必须拒");
    let message = failed_message(&err);
    assert!(message.contains(ENV_ALLOW_REAL_SPEND), "{message}");
    assert!(message.contains("护栏"), "{message}");
    assert!(gateway.captured().is_empty(), "护栏拒 = 零网关调用");
    assert!(!wal.exists(), "零落账:{wal:?}");
    assert_eq!(evidence_files(&evidence).len(), 1, "只有 L1 档");
}

#[test]
fn l2_probe_success_with_stub_gateway_pins_wire_shape_and_ledger() {
    let dir = temp_dir("l2-ok");
    let keys = test_keys();
    let wal = dir.join("probe-wal.jsonl");
    let evidence = dir.join("evidence");
    let gateway = MockGateway::new(keys.alipay_side.clone(), &[Script::PrecreateOk]);
    let mut options = options(
        l2_env(&keys, true),
        &wal,
        &evidence,
        true,
        false,
        Some(gateway.clone()),
    );
    let mut confirmer = ScriptConfirmer::new(&[true]);

    let outcome = run_probe(&mut options, &mut confirmer).expect("L2 探针应过");
    assert_eq!(outcome.reached, "L2");
    assert_eq!(
        outcome.evidence.len(),
        2,
        "L1 + L2 两份取证档:{:?}",
        outcome.evidence
    );

    // 报文同源(W-50 官方模板管线,不另写字段面)。
    let captured = gateway.captured();
    assert_eq!(captured.len(), 1, "L2 恰好一次网关调用:{captured:?}");
    let (url, body) = &captured[0];
    assert!(url.starts_with(MOCK_GATEWAY), "探针只打配置网关:{url}");
    assert!(url.contains("method=alipay.trade.precreate"), "{url}");
    let biz = request_biz(body);
    assert_eq!(biz["product_code"], "FACE_TO_FACE_PAYMENT", "{biz}");
    assert_eq!(
        biz["total_amount"], "0.01",
        "探针金额 = 官方最小值 0.01 元:{biz}"
    );
    assert_eq!(biz["subject"], "Wanning channel-test L2 网关探针", "{biz}");
    assert!(
        biz.get("agreement_params").is_none(),
        "探针无协议号语义:{biz}"
    );

    // 确认提示回显纪律:真实网关域名 + 接口名 + 零资金移动语义。
    let prompts = confirmer.prompts();
    assert_eq!(prompts.len(), 1, "L2 只确认一次:{prompts:?}");
    assert!(prompts[0].contains(MOCK_GATEWAY), "{prompts:?}");
    assert!(prompts[0].contains("alipay.trade.precreate"), "{prompts:?}");
    assert!(prompts[0].contains("零资金移动"), "{prompts:?}");

    // 过账本不豁免:探针意图落独立探针账本(闸放行,nonce 消耗 1)。
    assert!(wal.is_file(), "探针账本已落:{wal:?}");
    {
        let state = WanningState::live_resuming(&wal).expect("探针账本可读回");
        assert_eq!(
            state.gate().replay_registry().iter().count(),
            1,
            "L2 一笔 = nonce 消耗 1"
        );
    }
    let wal_text = read(&wal);
    assert!(wal_text.contains("channel-test-probe-"), "{wal_text}");
    assert!(wal_text.contains("alipay:channel-test"), "{wal_text}");

    // 取证档脱敏:app_id/sign 打码;密钥材料绝不入档;已验签响应原文在档。
    let l2_text = read(outcome.evidence.last().expect("L2 档"));
    assert!(l2_text.contains("***"), "app_id/sign 要打码:{l2_text}");
    assert!(!l2_text.contains(APP_ID), "app_id 原值不入档:{l2_text}");
    assert!(
        !l2_text.contains(&keys.merchant_private),
        "密钥材料绝不入档"
    );
    assert!(!l2_text.contains(&keys.alipay_public), "公钥材料同样不入档");
    assert!(l2_text.contains("alipay.trade.precreate"), "{l2_text}");
    assert!(
        l2_text.contains("qr.alipay.com"),
        "已验签响应原文在档:{l2_text}"
    );
}

#[test]
fn l2_gateway_rejection_is_recorded_honestly() {
    let dir = temp_dir("l2-reject");
    let keys = test_keys();
    let wal = dir.join("probe-wal.jsonl");
    let evidence = dir.join("evidence");
    let gateway = MockGateway::new(keys.alipay_side.clone(), &[Script::PrecreateRejected]);
    let mut options = options(
        l2_env(&keys, true),
        &wal,
        &evidence,
        true,
        false,
        Some(gateway.clone()),
    );
    let mut confirmer = ScriptConfirmer::new(&[true]);

    let err = run_probe(&mut options, &mut confirmer).expect_err("网关拒绝 = L2 未过");
    let message = failed_message(&err);
    assert!(message.contains("L2 未过"), "{message}");
    assert!(message.contains("网关拒绝"), "{message}");
    assert!(message.contains("不退预算"), "诚实边界要点明:{message}");

    // 码值原样落取证档(零编造:拒绝响应已验签,码值逐字带出,零语义映射)。
    let names = evidence_files(&evidence);
    let rejected = names
        .iter()
        .find(|name| name.ends_with("-L2-rejected.txt"))
        .expect("拒绝路径要有取证档:{names:?}");
    let text = read(&evidence.join(rejected));
    assert!(text.contains("code=40004"), "{text}");
    assert!(text.contains("sub_code=W52_TEST_REJECTED"), "{text}");
    assert!(text.contains("绝不硬编码语义映射"), "{text}");

    // 过账本不豁免:闸放行在网关调用之前,渠道拒绝不退预算(账本照记)。
    let state = WanningState::live_resuming(&wal).expect("探针账本可读回");
    assert_eq!(state.gate().replay_registry().iter().count(), 1);
}

#[test]
fn l2_user_refusal_aborts_before_gateway_and_ledger() {
    let dir = temp_dir("l2-refuse");
    let keys = test_keys();
    let wal = dir.join("probe-wal.jsonl");
    let evidence = dir.join("evidence");
    let gateway = MockGateway::new(keys.alipay_side.clone(), &[]);
    let mut options = options(
        l2_env(&keys, true),
        &wal,
        &evidence,
        true,
        false,
        Some(gateway.clone()),
    );
    let mut confirmer = ScriptConfirmer::new(&[false]);

    let err = run_probe(&mut options, &mut confirmer).expect_err("用户拒绝 = 终止");
    let message = failed_message(&err);
    assert!(message.contains("取消"), "{message}");
    assert!(message.contains("零外联零落账"), "{message}");
    // 确认在网关调用与账本打开之前:两边都零触碰。
    assert!(gateway.captured().is_empty(), "拒绝 = 零网关调用");
    assert!(!wal.exists(), "拒绝 = 零落账:{wal:?}");
    assert_eq!(evidence_files(&evidence).len(), 1, "只有 L1 档");
}

#[test]
fn l3_without_real_spend_flag_stops_at_l2() {
    let dir = temp_dir("l3-stop");
    let keys = test_keys();
    let wal = dir.join("probe-wal.jsonl");
    let evidence = dir.join("evidence");
    let gateway = MockGateway::new(keys.alipay_side.clone(), &[Script::PrecreateOk]);
    let mut options = options(
        l3_env(&keys),
        &wal,
        &evidence,
        true,
        false,
        Some(gateway.clone()),
    );
    let mut confirmer = ScriptConfirmer::new(&[true]);

    let outcome = run_probe(&mut options, &mut confirmer).expect("止步 L2 = 正常返回");
    assert_eq!(outcome.reached, "L2");
    assert_eq!(captured_len(&gateway), 1, "只有探针一次调用,绝无扣款");
    assert_eq!(
        outcome.evidence.len(),
        2,
        "L1 + L2 两份档:{:?}",
        outcome.evidence
    );
    assert_eq!(confirmer.prompts().len(), 1, "L3 不该被问确认");
}

#[test]
fn l3_real_spend_requires_agreement_no() {
    let dir = temp_dir("l3-no-agreement");
    let keys = test_keys();
    let wal = dir.join("probe-wal.jsonl");
    let evidence = dir.join("evidence");
    let gateway = MockGateway::new(keys.alipay_side.clone(), &[Script::PrecreateOk]);
    // L2 槽位齐但无协议号:--real-spend 单独顶不上去(没有协议号的扣款 = 裸转账)。
    let mut options = options(
        l2_env(&keys, true),
        &wal,
        &evidence,
        true,
        true,
        Some(gateway.clone()),
    );
    let mut confirmer = ScriptConfirmer::new(&[true]);

    let err = run_probe(&mut options, &mut confirmer).expect_err("缺协议号必须拒");
    let message = failed_message(&err);
    assert!(message.contains(SLOT_ALIPAY_AGREEMENT_NO), "{message}");
    assert!(message.contains("裸转账"), "{message}");
    assert_eq!(captured_len(&gateway), 1, "L2 探针照跑,扣款一次不发");
    assert!(wal.is_file(), "L2 已过账本:{wal:?}");
}

#[test]
fn l3_full_path_has_two_gate_decisions_and_official_pay_shape() {
    let dir = temp_dir("l3-full");
    let keys = test_keys();
    let wal = dir.join("probe-wal.jsonl");
    let evidence = dir.join("evidence");
    let gateway = MockGateway::new(
        keys.alipay_side.clone(),
        &[Script::PrecreateOk, Script::PayOk],
    );
    let mut options = options(
        l3_env(&keys),
        &wal,
        &evidence,
        true,
        true,
        Some(gateway.clone()),
    );
    let mut confirmer = ScriptConfirmer::new(&[true, true]);

    let outcome = run_probe(&mut options, &mut confirmer).expect("L3 全链应过");
    assert_eq!(outcome.reached, "L3");
    assert_eq!(
        outcome.evidence.len(),
        3,
        "L1 + L2 + L3 三份档:{:?}",
        outcome.evidence
    );

    // 第二次网关调用 = alipay.trade.pay,官方模板形状(协议内扣款字段面)。
    let captured = gateway.captured();
    assert_eq!(captured.len(), 2, "探针 + 扣款各一次:{captured:?}");
    let (pay_url, pay_body) = &captured[1];
    assert!(pay_url.contains("method=alipay.trade.pay"), "{pay_url}");
    let biz = request_biz(pay_body);
    assert_eq!(biz["product_code"], "GENERAL_WITHHOLDING", "{biz}");
    assert_eq!(
        biz["agreement_params"]["agreement_no"], AGREEMENT_NO,
        "协议内扣款必须带协议号:{biz}"
    );
    assert_eq!(biz["total_amount"], "0.01", "{biz}");

    // L3 确认文案单独写明真实扣款(第四重明示)。
    let prompts = confirmer.prompts();
    assert_eq!(prompts.len(), 2, "L2 + L3 各确认一次:{prompts:?}");
    assert!(prompts[1].contains("真实扣款 0.01 元"), "{prompts:?}");
    assert!(prompts[1].contains("本人账户"), "{prompts:?}");
    assert!(prompts[1].contains("alipay.trade.pay"), "{prompts:?}");

    // 过账本不豁免:两笔意图(探针 + 扣款)= nonce 消耗 2。
    let state = WanningState::live_resuming(&wal).expect("探针账本可读回");
    assert_eq!(
        state.gate().replay_registry().iter().count(),
        2,
        "L2 + L3 = nonce 消耗 2"
    );
    drop(state);
    let wal_text = read(&wal);
    assert!(wal_text.contains("alipay:channel-test"), "{wal_text}");

    // L3 取证档带扣款接口名与「不可撤销」语义。
    let l3_text = read(outcome.evidence.last().expect("L3 档"));
    assert!(l3_text.contains("alipay.trade.pay"), "{l3_text}");
    assert!(
        !l3_text.contains(AGREEMENT_NO),
        "协议号不入取证档(值不回显):{l3_text}"
    );
}

#[test]
fn real_spend_without_real_is_refused_in_both_entries() {
    let dir = temp_dir("l3-no-real");
    let keys = test_keys();
    let wal = dir.join("probe-wal.jsonl");
    let evidence = dir.join("evidence");
    // run() 的用法门(退出码 2)在子进程测试里锁过;这里锁 run_probe 的保底门
    // ——两个入口都能到达同一道门(护栏立在通道上,不立在门口)。
    let mut options = options(l3_env(&keys), &wal, &evidence, false, true, None);
    let mut confirmer = ScriptConfirmer::new(&[]);

    let err = run_probe(&mut options, &mut confirmer).expect_err("L3 是 L2 的追加");
    let message = failed_message(&err);
    assert!(message.contains("缺少 --real"), "{message}");
    assert!(message.contains("绝不跳级"), "{message}");
    // 零 IO:门在一切落盘之前。
    assert!(!wal.exists(), "{wal:?}");
    assert!(!evidence.exists(), "{evidence:?}");
}

#[test]
fn probe_env_blank_values_count_as_unset() {
    let mut env = ProbeEnv::default();
    env.set(SLOT_ALIPAY_APP_ID, APP_ID);
    env.set(SLOT_ALIPAY_AGREEMENT_NO, "   ");
    assert!(env.is_set(SLOT_ALIPAY_APP_ID));
    assert!(
        !env.is_set(SLOT_ALIPAY_AGREEMENT_NO),
        "空白值按未设(展示面 fail-closed)"
    );
    // get 返回原值:空白被消费侧各自的 fail-closed 门拦住(护栏等值比较 / 密钥
    // 解析拒绝),槽位表不在 get 里做语义决策。
    assert_eq!(env.get(SLOT_ALIPAY_AGREEMENT_NO), Some("   "));
    // 快照同源:同一份值喂 W-07 护栏,不读第二遍进程环境。
    let snapshot = env.guard_snapshot();
    assert_eq!(snapshot.get(SLOT_ALIPAY_APP_ID), Some(APP_ID));
    assert_eq!(snapshot.get(SLOT_ALIPAY_AGREEMENT_NO), Some("   "));
}

/// 替身捕获数(测试里多处只数不看)。
fn captured_len(gateway: &MockGateway) -> usize {
    gateway.captured().len()
}
