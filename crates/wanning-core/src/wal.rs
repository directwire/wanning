//! 审计日志(WAL):append-only JSONL,每条记录一行。
//!
//! 四卖点的第 4 条(全程审计)就落在这里:哪个意图、闸怎么判、为什么、判完账本多少,
//! 全部一行 JSON,append-only,**永不 truncate**。
//!
//! 落盘行 = 内容 + 完整性链(W-21 引入;旧格式是裸记录,不互通,见下):
//!
//! ```json
//! {"seq":1,"prev":0,"rec":{"kind":"register_delegation","ts":1700000000,"delegation":{...}}}
//! {"seq":2,"prev":144...,  "rec":{"kind":"revoke","ts":1700000001,"delegation_id":"d1"}}
//! {"seq":3,"prev":99...,   "rec":{"kind":"decide","ts":1700000002,"decision":"allow",
//!          "delegation_id":"d1","intent":{...},"budget_after_cents":500}}
//! {"seq":4,"prev":77...,   "rec":{"kind":"decide","ts":1700000003,"decision":"deny",
//!          "delegation_id":"d1","intent":{...},"reason":"over_budget","budget_after_cents":500}}
//! {"seq":5,"prev":55...,   "rec":{"kind":"pending","ts":1700000004,
//!          "pending_id":"p-...","delegation_id":"d1","intent":{...},
//!          "approved_amount_cents":400,"expires_ts":1700000904}}
//! {"seq":6,"prev":33...,   "rec":{"kind":"confirm","ts":1700000010,
//!          "pending_id":"p-...","amount_cents":400,"proof":"TRADE-..."}}
//! {"seq":7,"prev":22...,   "rec":{"kind":"terminal","ts":1700000010,
//!          "pending_id":"p-...","outcome":"completed"}}
//! ```
//!
//! ⑤pending/confirm/terminal 三种行 = W-53a 人在环待支付(第一形态):一行一段
//! 事件链,语义由 [`crate::pending::PendingLedger`] 统一应用(实时与回放同一套)。
//!
//! `budget_after_cents` = 该决策落地后的**累计消费**(分),不是剩余预算;
//! 剩余预算 = 委托 cap − 此值。选累计消费而不是剩余:对未知委托也能给出明确定义(0),
//! 且回放重建账本时可直接对账。
//!
//! **完整性链(防篡改)**:每行带 `seq`(物理行号)与 `prev`(上一行的链值,首行 0),
//! 链值 = FNV-1a64(`prev` 小端 8 字节 ‖ `seq` 小端 8 字节 ‖ 该行 `rec` 的规范 JSON)。
//! 读回([`read_verified`])逐行验两件事:`seq` 必须等于物理行号(删行/重排/复制当场现形),
//! `prev` 必须等于按前文重算的链值(改任何一行而不重算后续整条链,下一行的 `prev` 就对不上)
//! ——任何一处不符即 fail-closed 报错。这只把日志从「可信因为约定 append-only」变成
//! 「可信因为改了会被抓住」。
//!
//! **已知边界**(诚实声明,不假装能测):链抓不住「只改最后一行内容」与「整体截尾」——
//! 最后一行没有后继行引用它,截尾剩下的前缀自身是一条合法的链。要堵住需要**外部锚点**
//! (所有者侧签名的链尾 / 远端锚点),列为账户开通后的 TODO(见决策记录)。
//!
//! 回放([`read_verified`] + `crate::state::WanningState::replay`):逐行重放到一个空闸上,
//! **重算结果必须与记录一致**,不一致即 fail-closed 报错;任何半行 JSON / 非法行 / 空行
//! 同样报错,**绝不静默跳过**——审计日志宁可停,不可吞。打开续写([`Wal::open`])同样
//! 先整体验一遍历史,带病审计绝不追加。
//!
//! 单写者:一份 WAL 同时至多一个**活着的写进程**([`WalLock`],[`Wal::open`] 自动持锁)。
//! 两个都已活着的进程共写一份审计,各自内存账本只知道自己花了几笔 → 预算硬上限失效、
//! 同一 nonce 跨进程放行,所以第二个写进程一律 fail-closed 拒启。锁只挡写进程,不挡读者:
//! 回放/审计读取走只读打开,服务运行期间照常可用。

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::delegation::Delegation;
use crate::error::CoreError;
use crate::gate::DenyReason;
use crate::intent::SpendIntent;
use crate::pending::PendingOutcome;

/// 决策结论(WAL 行内的小写蛇形字符串)。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WalDecision {
    Allow,
    Deny,
}

/// 审计日志的一行。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WalRecord {
    /// 用户授权:一份委托进入闸。
    RegisterDelegation { ts: u64, delegation: Delegation },
    /// 用户收权(kill switch)。
    Revoke { ts: u64, delegation_id: String },
    /// 闸的一次判定(放行与拒绝都记)。
    Decide {
        ts: u64,
        decision: WalDecision,
        delegation_id: String,
        intent: SpendIntent,
        /// 拒绝原因;Allow 时缺省(serde skip)。
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<DenyReason>,
        /// 该决策落地后的累计消费(分),见模块注释。
        budget_after_cents: u64,
    },
    /// W-53a ③待支付:闸放行(pending_pay 档位)后开的待支付单(审批额 + TTL)。
    Pending {
        ts: u64,
        pending_id: String,
        delegation_id: String,
        /// ①意图原样入账(与触发放行的 Decide 行同一意图,回放可对账)。
        intent: SpendIntent,
        /// ②审批额(= 意图额;与 Pending 行自身的 intent.amount_cents 一致)。
        approved_amount_cents: u64,
        /// 过期时刻(开单时刻 + TTL;半开窗口)。
        expires_ts: u64,
    },
    /// W-53a ④人确认:人的显式动作(CLI 人工面),幂等,带支付凭证。
    Confirm {
        ts: u64,
        pending_id: String,
        /// 确认额(必须等于该单的审批额——金额一致钉)。
        amount_cents: u64,
        /// 支付凭证(交易号;空凭证在状态层就被拒,不落行)。
        proof: String,
    },
    /// W-53a ⑤终态:完成 / TTL 过期作废。
    Terminal {
        ts: u64,
        pending_id: String,
        outcome: PendingOutcome,
    },
}

impl WalRecord {
    /// 记录时刻(Unix 秒)。回放用它驱动注入时钟,保证判定与实时一致。
    pub fn ts(&self) -> u64 {
        match self {
            WalRecord::RegisterDelegation { ts, .. }
            | WalRecord::Revoke { ts, .. }
            | WalRecord::Decide { ts, .. }
            | WalRecord::Pending { ts, .. }
            | WalRecord::Confirm { ts, .. }
            | WalRecord::Terminal { ts, .. } => *ts,
        }
    }

    /// 记录种类(审计展示用)。
    pub fn kind(&self) -> &'static str {
        match self {
            WalRecord::RegisterDelegation { .. } => "register_delegation",
            WalRecord::Revoke { .. } => "revoke",
            WalRecord::Decide { .. } => "decide",
            WalRecord::Pending { .. } => "pending",
            WalRecord::Confirm { .. } => "confirm",
            WalRecord::Terminal { .. } => "terminal",
        }
    }
}

/// 落盘的一行:内容(`rec`)+ 完整性链(`seq`/`prev`)。
///
/// 内容与链分开存:链字段本身不参与链值计算(否则自引用),链值只覆盖 `rec` 的
/// 规范 JSON——所以改内容必断链,而 `seq`/`prev` 被改则直接对不上物理行号/前文。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalLine {
    /// 物理行号(1-based)。读回时逐行核对,删行/重排/复制当场现形。
    pub seq: u64,
    /// 上一行的链值(首行为创世值 0)。
    pub prev: u64,
    /// 记录本体。
    pub rec: WalRecord,
}

/// 完整性链值:FNV-1a64(`prev` 小端 8 字节 ‖ `seq` 小端 8 字节 ‖ `rec` 规范 JSON)。
///
/// 链值只吃 `rec` 的规范 JSON(`serde_json::to_string`),不吃整行原文——行内键序
/// 差异、`seq`/`prev` 字段位置都不影响验证,同一内容重算恒等。
/// `pub(crate)`:锚点(W-23)读侧独立重算链尾时复用同一口径,不另抄一份公式。
pub(crate) fn chain_value(prev: u64, seq: u64, rec_json: &str) -> u64 {
    let mut bytes = Vec::with_capacity(16 + rec_json.len());
    bytes.extend_from_slice(&prev.to_le_bytes());
    bytes.extend_from_slice(&seq.to_le_bytes());
    bytes.extend_from_slice(rec_json.as_bytes());
    fnv1a_64(&bytes)
}

/// FNV-1a 64(非密码学,确定性对账/完整性链用)。
pub(crate) fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// append-only 日志句柄。打开即追加,绝不截断;**打开即持单写者锁**,
/// **打开即验完整历史**(完整性链断裂 → 拒开,带病审计绝不续写)。
#[derive(Debug)]
pub struct Wal {
    file: File,
    path: PathBuf,
    lines: u64,
    /// 完整性链尾值(最后一条记录的链值;空日志为创世值 0)。
    chain: u64,
    /// 持有单写者锁(字段活着 = 锁在;Drop 时随句柄一起释放)。
    _lock: WalLock,
}

/// WAL 对应的单写者锁文件路径:`<wal 文件名>.lock`,与 WAL 同目录。
///
/// 刻意用「追加后缀」而不是 `Path::with_extension`(那会把 `.jsonl` 整个换掉),
/// 这样 `mcp-demo.wal → mcp-demo.wal.lock`、`a.jsonl → a.jsonl.lock` 一律成立。
pub fn single_writer_lock_path(wal_path: impl AsRef<Path>) -> PathBuf {
    let wal_path = wal_path.as_ref();
    let mut name = wal_path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".lock");
    wal_path.with_file_name(name)
}

/// 单写者锁:持锁期间同一份 WAL 只允许这一个写进程存在(fail-closed)。
///
/// **为什么必须有**:闸的账本、nonce 登记、撤销集合都在内存里,WAL 只在启动时
/// 回放一次。两个都已活着的进程共写一份 WAL,各自内存账本只知道自己花了几笔——
/// 实测(本仓 `tests/single_writer.rs`,修复前)两进程各放行 700 分、委托 cap
/// 1000 分,合计 1400 分:预算硬上限失效,且 WAL 出现两行同一 id 的注册,
/// 下次回放对账必炸。`.mcp.json` 与 `.trae/mcp.json` 指向同一份默认 WAL,
/// 两个平台并挂就是真实场景。
///
/// **机制**(零依赖、跨平台):`create_new`(O_EXCL)原子创建锁文件——两个进程
/// 同时抢,恰好一个成功;内容 = 持锁进程 PID + WAL 路径,供拒启方报错指认。
/// 锁随 [`Drop for WalLock`](Self) 释放(正常退出/panic 展开都会走到)。
///
/// **已知权衡**(记录于决策记录):持锁进程被 kill -9 会留下孤儿锁,
/// 下一个进程拒启,按错误信息确认无活进程后手动删除锁文件即可恢复(默认 WAL 在
/// `target/` 下,`cargo clean` 亦可)。刻意不做「自动判死」:std 没有跨平台进程
/// 存活检查,臆造判活逻辑比让所有者手删一行文件危险得多——审计闸宁可拒启,不可
/// 带病放行。
///
/// **语义边界**:锁只挡写进程,不挡读者——回放/审计读取走只读打开,服务运行
/// 期间照常可用(见 `tests/single_writer.rs::replay_works_while_writer_holds_lock`)。
#[derive(Debug)]
pub struct WalLock {
    path: PathBuf,
}

impl WalLock {
    /// 拿单写者锁;被占 → [`CoreError::WalLocked`](crate::error::CoreError::WalLocked)。
    pub fn acquire(wal_path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let wal_path = wal_path.as_ref();
        let lock_path = single_writer_lock_path(wal_path);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut file) => {
                let written = writeln!(file, "pid={}", std::process::id())
                    .and_then(|()| writeln!(file, "wal={}", wal_path.display()))
                    .and_then(|()| file.flush());
                if let Err(e) = written {
                    // 内容没写成 → 不留半个锁文件,锁没拿到就如实报 IO 错。
                    let _ = std::fs::remove_file(&lock_path);
                    return Err(CoreError::WalIo(format!(
                        "写单写者锁 {lock_path:?} 失败(fail-closed): {e}"
                    )));
                }
                Ok(Self { path: lock_path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let holder = std::fs::read_to_string(&lock_path).unwrap_or_default();
                let holder = holder.trim();
                let holder_note = if holder.is_empty() {
                    "锁文件为空(持锁方刚创建,极可能是并发启动竞争)".to_string()
                } else {
                    format!("持锁信息: {holder}")
                };
                Err(CoreError::WalLocked {
                    path: lock_path.display().to_string(),
                    message: format!(
                        "同一份审计日志已有另一个 Wanning 进程在写({holder_note});\
                         确认没有别的闸在跑后,删除该锁文件即可恢复\
                         (默认 WAL 在 target/ 下,cargo clean 亦可)"
                    ),
                })
            }
            Err(e) => Err(CoreError::WalIo(format!(
                "创建单写者锁 {lock_path:?} 失败: {e}"
            ))),
        }
    }
}

impl Drop for WalLock {
    fn drop(&mut self) {
        // 尽力而为:锁文件删不掉不该让业务报错(比如已被人工清理)。
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Wal {
    /// 打开(不存在则创建)用于追加。**禁 truncate**:已有内容一律保留。
    ///
    /// 先自动创建父目录(W-43a 默认路径 `~/.wanning/wal.jsonl` 的「零配置」体验;
    /// 显式路径同样受益),再拿单写者锁(fail-closed:第二个写进程拒启),再
    /// **整体验一遍已有历史**([`read_verified`]:逐行可解析 + 完整性链——历史被
    /// 改/删/排,拒开不续写),再以追加模式打开。锁定之后验历史,才不会和另一个
    /// 进程的追加赛跑。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let path = path.as_ref().to_path_buf();
        crate::paths::ensure_wal_parent(&path)?;
        let _lock = WalLock::acquire(&path)?;
        // 文件不存在 = 全新日志(0 行、创世链 0);存在则历史必须完整体面。
        let (existing_lines, chain) = if path.exists() {
            let verified = read_verified(&path)?;
            (verified.records.len() as u64, verified.tail)
        } else {
            (0, 0)
        };
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(false)
            .open(&path)
            .map_err(|e| CoreError::WalIo(format!("打开 WAL {path:?} 失败: {e}")))?;
        Ok(Self {
            file,
            path,
            lines: existing_lines,
            chain,
            _lock,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 已写入行数(含历史行,1-based 下一条即 `line_count() + 1`)。
    pub fn line_count(&self) -> u64 {
        self.lines
    }

    /// 完整性链尾值(最后一条记录的链值;空日志为创世值 0)。
    pub fn chain_tail(&self) -> u64 {
        self.chain
    }

    /// 追加一条记录,返回其行号(1-based)。
    ///
    /// 每条写完立即 `flush`——审计必须先于一切下游动作落盘(真消费触发前的证据)。
    /// 行号即 `seq`、链尾即 `prev`,与读回验证的口径一致。
    pub fn append(&mut self, record: &WalRecord) -> Result<u64, CoreError> {
        let seq = self.lines + 1;
        let rec_json = serde_json::to_string(record)
            .map_err(|e| CoreError::WalIo(format!("WAL 记录序列化失败: {e}")))?;
        let mut line = serde_json::to_string(&WalLine {
            seq,
            prev: self.chain,
            rec: record.clone(),
        })
        .map_err(|e| CoreError::WalIo(format!("WAL 记录序列化失败: {e}")))?;
        line.push('\n');
        let path = self.path.clone();
        self.file
            .write_all(line.as_bytes())
            .and_then(|()| self.file.flush())
            .map_err(|e| CoreError::WalIo(format!("写 WAL {path:?} 失败: {e}")))?;
        self.lines = seq;
        self.chain = chain_value(self.chain, seq, &rec_json);
        Ok(self.lines)
    }
}

/// 读原始行(供审计展示直接引用原文)。文件不存在 → 错误。
pub fn raw_lines(path: impl AsRef<Path>) -> Result<Vec<String>, CoreError> {
    let path = path.as_ref();
    let file =
        File::open(path).map_err(|e| CoreError::WalIo(format!("读 WAL {path:?} 失败: {e}")))?;
    BufReader::new(file)
        .lines()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| CoreError::WalIo(format!("读 WAL {path:?} 失败: {e}")))
}

/// [`read_verified`] 的产出:逐行记录(1-based 行号)+ 完整性链尾值。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedLog {
    /// 逐行记录,`Vec` 下标 i 的行号 = i + 1。
    pub records: Vec<(u64, WalRecord)>,
    /// 逐行完整性链节(与 `records` 一一对应);审计展示用——让人能看见每一行的
    /// `prev → 本行链值`,而不是只给一个无法核对的链尾。读侧各链节独立重算,
    /// 不是照抄落盘行。
    pub links: Vec<WalChainLink>,
    /// 最后一条记录的链值;空日志为创世值 0。实时侧(`Wal::chain_tail`)与
    /// 回放侧各算一份,两边相等是审计对账的证据之一。
    pub tail: u64,
}

/// 一行记录的完整性链节(读侧独立重算结果)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalChainLink {
    /// 物理行号(1-based),与 [`WalLine::seq`](WalLine::seq) 验证口径一致。
    pub seq: u64,
    /// 本行记录的前行链值(首行为创世值 0)。
    pub prev: u64,
    /// 本行链值(链尾即最后一条的 value;空日志无链节、链尾为 0)。
    pub value: u64,
}

/// 读出全部记录(**逐行验完整性链**),附行号(1-based)。
///
/// **fail-closed**,一处都不过就整体报错,绝不静默跳过——审计日志里出现
/// 「看不懂的行」或「对不上链的行」本身就是事故,吞掉它等于伪造证据:
/// - 半行 JSON / 空行 / 未知结构 → [`CoreError::WalBadLine`];
/// - `seq` 与物理行号不符(删行/重排/复制的痕迹)→ [`CoreError::WalChainBroken`];
/// - `prev` 与按前文重算的链值不符(某行内容被改而后续整条链未重算)
///   → [`CoreError::WalChainBroken`]。
///
/// 已知边界见模块注释:只改最后一行内容、整体截尾,链抓不住(无后继行引用)。
pub fn read_verified(path: impl AsRef<Path>) -> Result<VerifiedLog, CoreError> {
    let mut records = Vec::new();
    let mut links = Vec::new();
    let mut chain = 0u64;
    for (idx, line) in raw_lines(path)?.into_iter().enumerate() {
        let line_no = idx as u64 + 1;
        if line.trim().is_empty() {
            return Err(CoreError::WalBadLine {
                line: line_no,
                message: "空行(WAL 不允许空行)".to_string(),
            });
        }
        let parsed: WalLine = match serde_json::from_str(&line) {
            Ok(parsed) => parsed,
            Err(e) => return Err(parse_failure(line_no, &line, e)),
        };
        if parsed.seq != line_no {
            return Err(CoreError::WalChainBroken {
                line: line_no,
                message: format!(
                    "seq={} 与物理行号 {line_no} 不一致——删行/重排/复制的痕迹",
                    parsed.seq
                ),
            });
        }
        if parsed.prev != chain {
            return Err(CoreError::WalChainBroken {
                line: line_no,
                message: format!(
                    "prev={} 与按前文重算的链值 {chain} 不符——本行或之前的行被改过,\
                     且后续整条链未重算",
                    parsed.prev
                ),
            });
        }
        let rec_json = serde_json::to_string(&parsed.rec).map_err(|e| CoreError::WalBadLine {
            line: line_no,
            message: format!("记录重序列化失败: {e}"),
        })?;
        chain = chain_value(chain, line_no, &rec_json);
        records.push((line_no, parsed.rec));
        links.push(WalChainLink {
            seq: line_no,
            prev: parsed.prev,
            value: chain,
        });
    }
    Ok(VerifiedLog {
        records,
        links,
        tail: chain,
    })
}

/// 解析失败 → 报错。旧格式(W-21 之前的裸记录)给出指名道姓的说明:
/// 新旧格式不互通,报错要让人知道为什么读不懂,而不是一句泛泛的解析失败。
fn parse_failure(line_no: u64, line: &str, error: serde_json::Error) -> CoreError {
    let legacy_hint = if serde_json::from_str::<WalRecord>(line).is_ok() {
        ";该行是 W-21 引入完整性链之前的旧格式(裸记录,无 seq/prev 完整性链)。\
         新旧格式不互通:旧文件原样保留、绝不迁移改写;确认旧日志已留档后,\
         可将其改名/移走,让闸从一份新日志重新开始"
    } else {
        ""
    };
    CoreError::WalBadLine {
        line: line_no,
        message: format!("JSON 解析失败: {error}{legacy_hint}"),
    }
}

/// 读出全部记录,附行号(1-based)。[`read_verified`] 的薄封装(只取记录,不取链尾)。
pub fn read_records(path: impl AsRef<Path>) -> Result<Vec<(u64, WalRecord)>, CoreError> {
    Ok(read_verified(path)?.records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join("wanning-wal-tests");
        std::fs::create_dir_all(&dir).expect("建临时目录");
        // pid + 原子序号 + 纳秒:裸 pid 跨轮运行会撞残留账本(W-21 教训,W-43b 轮补齐)。
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        dir.join(format!(
            "{tag}-{}-{}-{nanos}.jsonl",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ))
    }

    fn sample_record(ts: u64) -> WalRecord {
        WalRecord::Decide {
            ts,
            decision: WalDecision::Allow,
            delegation_id: "d1".into(),
            intent: SpendIntent::new("d1", 1, 500, "jd:shop-1", "grocery", "测试"),
            reason: None,
            budget_after_cents: 500,
        }
    }

    #[test]
    fn append_is_one_json_per_line_and_counts_lines() {
        let path = tmp_path("append");
        let mut wal = Wal::open(&path).expect("打开");
        assert_eq!(wal.line_count(), 0);
        assert_eq!(wal.chain_tail(), 0, "空日志链尾 = 创世值 0");
        assert_eq!(wal.append(&sample_record(1)).expect("写"), 1);
        assert_eq!(wal.append(&sample_record(2)).expect("写"), 2);
        drop(wal);

        let lines = raw_lines(&path).expect("读");
        assert_eq!(lines.len(), 2);
        assert!(!lines[0].ends_with('\n'), "行内不含换行");
        let line: WalLine = serde_json::from_str(&lines[0]).expect("逐行可解析");
        assert_eq!(line.seq, 1, "首行 seq = 物理行号");
        assert_eq!(line.prev, 0, "首行 prev = 创世值 0");
        assert_eq!(line.rec.ts(), 1);
        assert_eq!(line.rec.kind(), "decide");
        let second: WalLine = serde_json::from_str(&lines[1]).expect("逐行可解析");
        assert_eq!(second.seq, 2);
        assert_ne!(second.prev, 0, "第二行 prev 必须是第一行的链值");
    }

    #[test]
    fn chain_tail_matches_independent_recompute_and_survives_reopen() {
        // 写侧的链尾与读侧独立重算的链尾必须一致;重开(验完整历史)后续写,链必须接上。
        let path = tmp_path("chain-tail");
        let mut wal = Wal::open(&path).expect("打开");
        for ts in 1..=3 {
            wal.append(&sample_record(ts)).expect("写");
        }
        let live_tail = wal.chain_tail();
        drop(wal);

        let verified = read_verified(&path).expect("读回验链");
        assert_eq!(verified.records.len(), 3);
        assert_eq!(verified.tail, live_tail, "读侧重算链尾 == 写侧链尾");
        assert_ne!(live_tail, 0, "三条记录后链尾非 0");

        // 重开:验历史通过,链从旧尾接续;再写一条,seq/prev 接得上,读回仍验得过。
        let mut wal = Wal::open(&path).expect("重开(历史完整)");
        assert_eq!(wal.line_count(), 3);
        assert_eq!(wal.chain_tail(), live_tail, "重开后链尾从历史接续");
        wal.append(&sample_record(4)).expect("续写");
        let verified = read_verified(&path).expect("续写后读回验链");
        assert_eq!(verified.records.len(), 4);
        assert_eq!(verified.tail, wal.chain_tail());
    }

    #[test]
    fn read_verified_reports_per_line_chain_links() {
        // 逐行链(审计回放页要人能看见每一行的 prev→本行链值):与记录一一对应,
        // 首行 prev = 创世值 0,本行 prev = 前行链值,尾行链值 = 链尾。
        let path = tmp_path("links");
        let mut wal = Wal::open(&path).expect("打开");
        for ts in 1..=4 {
            wal.append(&sample_record(ts)).expect("写");
        }
        drop(wal);

        let verified = read_verified(&path).expect("读回验链");
        assert_eq!(
            verified.links.len(),
            verified.records.len(),
            "逐行链与记录一一对应"
        );
        for (idx, link) in verified.links.iter().enumerate() {
            assert_eq!(link.seq, idx as u64 + 1, "link.seq = 物理行号");
            if idx == 0 {
                assert_eq!(link.prev, 0, "首行 prev = 创世值 0");
            } else {
                assert_eq!(
                    link.prev,
                    verified.links[idx - 1].value,
                    "本行 prev = 前行链值"
                );
            }
        }
        assert_eq!(
            verified.links.last().map(|link| link.value),
            Some(verified.tail),
            "尾行链值 = 链尾"
        );
    }

    #[test]
    fn empty_wal_has_no_chain_links() {
        let path = tmp_path("empty-links");
        std::fs::write(&path, "").expect("写空文件");
        let verified = read_verified(&path).expect("空文件是合法状态");
        assert!(verified.links.is_empty(), "空日志无链节");
    }

    #[test]
    fn empty_wal_verifies_to_genesis_chain() {
        let path = tmp_path("empty-chain");
        std::fs::write(&path, "").expect("写空文件");
        let verified = read_verified(&path).expect("空文件是合法状态");
        assert!(verified.records.is_empty());
        assert_eq!(verified.tail, 0, "空日志链尾 = 创世值 0");
    }

    #[test]
    fn open_is_append_only_never_truncates() {
        let path = tmp_path("append-only");
        {
            let mut wal = Wal::open(&path).expect("打开");
            wal.append(&sample_record(1)).expect("写");
        }
        {
            let mut wal = Wal::open(&path).expect("重开不得截断");
            assert_eq!(wal.line_count(), 1, "重开必须看到历史行");
            wal.append(&sample_record(2)).expect("追加");
        }
        assert_eq!(raw_lines(&path).expect("读").len(), 2, "历史行必须保留");
    }

    #[test]
    fn decide_record_roundtrip_shape() {
        // 拒绝记录带 reason,放行记录不带(缺省),形状与模块注释一致。
        let deny = WalRecord::Decide {
            ts: 7,
            decision: WalDecision::Deny,
            delegation_id: "d1".into(),
            intent: SpendIntent::new("d1", 2, 9000, "jd:shop-1", "x", ""),
            reason: Some(DenyReason::OverBudget),
            budget_after_cents: 500,
        };
        let json = serde_json::to_string(&deny).unwrap();
        assert!(json.contains("\"kind\":\"decide\""));
        assert!(json.contains("\"decision\":\"deny\""));
        assert!(json.contains("\"reason\":\"over_budget\""));
        let back: WalRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, deny);

        let allow_json = serde_json::to_string(&sample_record(1)).unwrap();
        assert!(!allow_json.contains("reason"), "Allow 不应带 reason 字段");
    }

    #[test]
    fn read_records_fails_closed_on_half_line() {
        let path = tmp_path("corrupt");
        std::fs::write(&path, "{\"kind\":\"revoke\",\"ts\":1,\"deleg\n").expect("写坏行");
        let err = read_records(&path).unwrap_err();
        assert!(
            matches!(err, CoreError::WalBadLine { line: 1, .. }),
            "半行 JSON 必须 fail-closed 报错: {err:?}"
        );
    }

    #[test]
    fn read_records_fails_closed_on_blank_line() {
        let path = tmp_path("blank");
        std::fs::write(&path, "\n").expect("写空行");
        let err = read_records(&path).unwrap_err();
        assert!(
            matches!(err, CoreError::WalBadLine { line: 1, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn read_records_fails_closed_on_unknown_shape() {
        let path = tmp_path("unknown");
        std::fs::write(&path, "{\"kind\":\"mystery\",\"ts\":1}\n").expect("写");
        let err = read_records(&path).unwrap_err();
        assert!(
            matches!(err, CoreError::WalBadLine { line: 1, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn read_records_reports_failing_line_number() {
        let path = tmp_path("line3");
        let mut wal = Wal::open(&path).expect("打开");
        wal.append(&sample_record(1)).expect("写");
        wal.append(&sample_record(2)).expect("写");
        drop(wal);
        let mut content = raw_lines(&path).expect("读").join("\n");
        content.push_str("\n{\"kind\":\"decide\",\"ts\":3\n");
        std::fs::write(&path, content).expect("追加坏行");

        match read_records(&path) {
            Err(CoreError::WalBadLine { line, .. }) => assert_eq!(line, 3, "报错必须指到坏行"),
            other => panic!("应报 WalBadLine,实际 {other:?}"),
        }
    }
}
