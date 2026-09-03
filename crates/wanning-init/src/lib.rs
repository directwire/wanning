//! wanning-init:给编码工具吐 Wanning MCP 配置的生成器(W-36;W-43a 产品化改版)。
//!
//! 产品边界(与 wanning-demo 演示台分开):本 crate 是**对端工具的接入生成器**——
//! 零网络、零真实消费、零文件副作用(默认只打印 stdout;写文件必须显式 `--out`
//! 且绝不覆盖已存在文件,动别人工具的配置 = 危险动作,拒)。
//!
//! **W-43a 产品化**:配置不再吐占位符——`wanning-mcp` 可执行文件与审计 WAL 路径在
//! 生成时解析成**真实绝对路径**直写进配置(新用户拿到就能用,不必手改
//! `{{WAL_PATH}}`),默认预算策略 `--budget` 显式写进 args(保守默认,用户可改);
//! 不给 `--wal` 时落产品默认账本 `~/.wanning/wal.jsonl`(Windows
//! `%USERPROFILE%\.wanning\wal.jsonl`,见 [`wanning_core::paths`])。路径一律转成
//! **正斜杠**(Windows 反斜杠在 JSON/YAML/TOML 里都要转义,正斜杠 Windows 也认)。
//!
//! 平台契约来源(零编造):
//! - claude-code:仓内 `.mcp.json` 现物(W-19 真插实测)——字段面带 `type: stdio`;
//! - trae:仓内 `.trae/mcp.json` 现物(W-17 直核)——字段面无 `type`,且官方明示
//!   command **不能含空格**(解析出的路径含空格 = 拒绝生成,绝不产出装不上的配置);
//! - codex:`~/.codex/config.toml` 的 `[mcp_servers.<id>]` 片段,W-35 直核**无路径
//!   变量**(W-43a 起写实路径,连占位符也不留)+ TOML `#` 注释;
//! - kimi:`.kimi-code/mcp.json`(用户级 `$KIMI_CODE_HOME/mcp.json` 或项目级),
//!   W-40 本机隔离实验修订——kimi-code 0.39.1 实测无 `kimi mcp` 子命令(W-17 的
//!   `kimi mcp add` 属 legacy kimi-cli 挂法),mcpServers 形态无 `type` 字段、
//!   无 `${...}` 变量,严格 JSON 无注释;
//! - workbuddy:`.workbuddy/mcp.json`(W-37 直核官方 MCP-Guide),`mcpServers` 结构
//!   同款但字段面无 `type`(官方示例只有 command/args/env),文档未提及 `${...}`
//!   变量;W-17 曾查不到,W-37 换路数(robots/sitemap 绕开 JS 首页)破冰,
//!   见 docs/research/workbuddy.md;
//! - deepseek-harness:**不是 mcp.json**——Cordis overlay YAML patch(W-44 任务书
//!   直核官方 `docs/user/guide/mcp-memory.md` 通用格式 + 本机 dsh 0.1.0-rc.7 包内
//!   `@deepseek-ai/dsh-mcp-client` README 字段表)。patch entry = `- insert:` 列表,
//!   `cwd: !!js process.cwd()` 的 js-tag 按官方示例原样;YAML 支持 `#` 注释(与
//!   TOML/shell 同侧)。真实 dsh 二进制取证:`dsh --profile headless --dump-config
//!   --patch <生成文件>` exit 0 且 wanning 行进入组合树(W-44 轮,零网络零会话);
//!   会话级端到端待所有者(dsh 会话 = 模型会话 + 网络,红线 2)。
//! - openclaw:**原生支持 MCP**(本机 2026.5.22 有 `openclaw mcp` 子命令族,W-45
//!   隔离实测 `mcp set/list/show` 全绿)——产出 `openclaw mcp set` **命令行**而非
//!   文件内容:openclaw.json 由宿主自己管理(实测落盘含 commands/messages/agents
//!   等骨架段),CLI 写入只动 `mcp.servers.wanning` 一段,天然满足「绝不覆盖」。
//!   字段面 `{command, args}` 与实测落盘逐字一致;官方 docs.openclaw.ai/mcp 直核
//!   stdio 字段 command/args/env/cwd + env 安全过滤(拦 NODE_OPTIONS 等)。
//! - hermes:**原生支持 MCP**(本机 hermes-agent v0.19.1 有 `hermes mcp` 子命令族,
//!   W-45 隔离实测)——产出 `hermes mcp add wanning --command <bin> --args <args>`
//!   命令行(discovery-first:add 即真连发现工具,挂载即验证;实测 2/2 工具现身,
//!   落 `$HERMES_HOME/config.yaml` 的 `mcp_servers.<name>` = {command, args,
//!   enabled: true})。W-45 隔离实测全链路:`hermes -z -t wanning` + 本地 mock LLM
//!   → allow 400 落 WAL;二次会话同 nonce → replay 拒(链连续)。工具现身名
//!   `mcp__wanning__*`(deferred catalog),模型侧经 `tool_call(name, arguments)`
//!   间接调用——直接调 mcp__ 名会报 does not exist(实测教训,写进 notes)。
//!
//! 注释纪律:各工具注释语法不同——TOML/shell/YAML 用 `#` 行内注释;**严格 JSON
//! 没有注释语法**,claude-code/trae/kimi/workbuddy 的说明只能打在 stdout 的
//! [`Artifact::notes`](文件内容保持纯净 JSON,防解析崩)。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::json;

/// 生成配置里写死的默认总预算(分)。保守默认,用户可在生成配置里改这个数。
/// 与 `wanning-mcp::DEFAULT_CAP_CENTS` 同值——刻意不引依赖同步,改任一侧时两边
/// 一起改(契约测试钉住 1000 这个数)。
pub const DEFAULT_BUDGET_CENTS: u64 = 1_000;

/// 支持的平台(生成器矩阵;字段权威=仓内现物与调研文档,见模块文档)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    ClaudeCode,
    Codex,
    Kimi,
    Trae,
    WorkBuddy,
    DeepSeekHarness,
    OpenClaw,
    Hermes,
}

/// 生成失败。全部 fail-closed:宁可拒生成,绝不产出一份装不上/对不上账的配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitError {
    /// 未知平台,报错列全矩阵。
    UnknownPlatform(String),
    /// PATH 上找不到 `wanning-mcp` 可执行文件;`searched` = 搜过的目录(报错点名)。
    McpBinaryNotFound { searched: Vec<PathBuf> },
    /// 显式 `--bin` 指向的路径不是文件。
    McpBinaryInvalid(String),
    /// WAL 路径解析失败(默认路径解析不出家目录,或当前目录不可得)。
    WalPathInvalid(String),
    /// Trae 的 command 不能含空格(官方明示),解析出的路径含空格 = 拒绝生成。
    TraeIncompatiblePath(String),
}

impl InitError {
    /// 人可读报错;未知平台必须列出全矩阵(契约测试锁定)。
    pub fn message(&self) -> String {
        match self {
            InitError::UnknownPlatform(input) => format!(
                "未知平台 '{input}'。--platform 支持矩阵:\n  \
                 claude-code   → 项目根 .mcp.json(type: stdio;W-19 实测)\n  \
                 codex         → config.toml [mcp_servers.wanning] 片段(无路径变量;W-35)\n  \
                 kimi          → .kimi-code/mcp.json(无 type 无变量;W-40 实测)\n  \
                 trae          → .trae/mcp.json(command 不能含空格;W-17)\n  \
                 workbuddy     → .workbuddy/mcp.json(无 type 无变量;W-37 直核)\n  \
                 deepseek-harness → Cordis overlay patch(- insert: 列表;W-44)\n  \
                 openclaw      → `openclaw mcp set` 命令行(mcp.servers 段;W-45 实测)\n  \
                 hermes        → `hermes mcp add` 命令行(config.yaml mcp_servers;W-45 实测)\n\
                 未知值 fail-closed,绝不猜。"
            ),
            InitError::McpBinaryNotFound { searched } => {
                let mut message = String::from(
                    "找不到 wanning-mcp 可执行文件(fail-closed,绝不猜一个命令)。先安装:\n  \
                     cargo install wanning-cli wanning-mcp\n\
                     或在 Wanning 仓内 cargo build -p wanning-mcp 后,用 --bin 指到 \
                     target/debug/wanning-mcp(或把该目录加进 PATH)。",
                );
                if !searched.is_empty() {
                    message.push_str("\n已搜索的 PATH 目录:");
                    for dir in searched {
                        message.push_str(&format!("\n  {}", dir.display()));
                    }
                }
                message
            }
            InitError::McpBinaryInvalid(message) => message.clone(),
            InitError::WalPathInvalid(message) => message.clone(),
            InitError::TraeIncompatiblePath(message) => message.clone(),
        }
    }
}

/// 生成入参:`None` 的字段按产品默认解析。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GenerateOptions {
    /// `wanning-mcp` 可执行文件;`None` = 从 PATH 解析。
    pub mcp_bin: Option<PathBuf>,
    /// 审计 WAL 路径;`None` = 产品默认 `~/.wanning/wal.jsonl`。
    pub wal: Option<PathBuf>,
}

/// 解析完成的一对真实路径(生成内容只吃这个,占位符从此不存在)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub mcp_bin: PathBuf,
    pub wal: PathBuf,
}

/// 一份生成物:`notes` 打在 stdout(说明/警示),`content` 是落盘/复制的净内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub notes: Vec<String>,
    pub content: String,
}

/// 解析 `--platform` 取值;未知值 fail-closed 列全矩阵。
pub fn parse_platform(input: &str) -> Result<Platform, InitError> {
    match input {
        "claude-code" => Ok(Platform::ClaudeCode),
        "codex" => Ok(Platform::Codex),
        "kimi" => Ok(Platform::Kimi),
        "trae" => Ok(Platform::Trae),
        "workbuddy" => Ok(Platform::WorkBuddy),
        "deepseek-harness" => Ok(Platform::DeepSeekHarness),
        "openclaw" => Ok(Platform::OpenClaw),
        "hermes" => Ok(Platform::Hermes),
        other => Err(InitError::UnknownPlatform(other.to_string())),
    }
}

/// 解析 `wanning-mcp` 可执行文件路径(纯函数,PATH 由调用方传入便于测试):
/// 显式路径必须是已存在的文件;否则按 PATH 逐目录找 `wanning-mcp`(+平台可执行
/// 后缀);都找不到 = [`InitError::McpBinaryNotFound`] 并列出搜过的目录。
pub fn resolve_bin(
    explicit: Option<&Path>,
    path_env: Option<&OsStr>,
) -> Result<PathBuf, InitError> {
    if let Some(bin) = explicit {
        if bin.is_file() {
            return Ok(bin.to_path_buf());
        }
        return Err(InitError::McpBinaryInvalid(format!(
            "--bin 指向的路径不是文件:{bin:?}\n\
             先安装:cargo install wanning-cli wanning-mcp\n\
             或在 Wanning 仓内 cargo build -p wanning-mcp 后,把 --bin 指到 \
             target/debug/wanning-mcp"
        )));
    }
    let exe = format!("wanning-mcp{}", std::env::consts::EXE_SUFFIX);
    let mut searched = Vec::new();
    if let Some(path_env) = path_env {
        for dir in std::env::split_paths(path_env) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            searched.push(dir.clone());
            let candidate = dir.join(&exe);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(InitError::McpBinaryNotFound { searched })
}

/// 解析审计 WAL 路径:显式路径按当前目录转绝对;缺省 = 产品默认
/// `~/.wanning/wal.jsonl`(家目录解析不出 = fail-closed,绝不猜落点)。
pub fn resolve_wal(explicit: Option<&Path>) -> Result<PathBuf, InitError> {
    let wal = match explicit {
        Some(wal) => wal.to_path_buf(),
        None => wanning_core::paths::default_wal_path().ok_or_else(|| {
            InitError::WalPathInvalid(
                "解析不出默认账本路径(WANNING_HOME / USERPROFILE / HOME 都没有)。\
                 用 --wal 显式给一个审计 WAL 路径"
                    .to_string(),
            )
        })?,
    };
    if wal.is_absolute() {
        return Ok(wal);
    }
    let current = std::env::current_dir()
        .map_err(|e| InitError::WalPathInvalid(format!("解析当前目录失败: {e}")))?;
    Ok(current.join(wal))
}

/// 解析一对生成入参(PATH 取自进程环境)。
pub fn resolve(options: &GenerateOptions) -> Result<Resolved, InitError> {
    let path_env = std::env::var_os("PATH");
    Ok(Resolved {
        mcp_bin: resolve_bin(options.mcp_bin.as_deref(), path_env.as_deref())?,
        wal: resolve_wal(options.wal.as_deref())?,
    })
}

/// 生成一份平台配置(W-43a 产品形态:真实绝对路径 + 默认预算,零占位符)。
/// 内容与仓内现物/调研的字段面契约锁定(见 `crates/wanning-init/tests/matrix.rs`)。
pub fn generate(platform: Platform, options: &GenerateOptions) -> Result<Artifact, InitError> {
    generate_with(platform, &resolve(options)?)
}

/// 已解析路径直接生成(调用方已 [`resolve`] 过时免得扫两遍 PATH)。
pub fn generate_with(platform: Platform, resolved: &Resolved) -> Result<Artifact, InitError> {
    if matches!(platform, Platform::Trae)
        && resolved
            .mcp_bin
            .to_string_lossy()
            .chars()
            .any(char::is_whitespace)
    {
        return Err(InitError::TraeIncompatiblePath(format!(
            "Trae 官方文档要求 command 不能含空格(W-17 直核),解析出的 wanning-mcp 路径含空格:{}\n\
             把 wanning-mcp 装到无空格路径(cargo install 的默认 bin 目录即可),\
             或用 --bin 指定无空格路径",
            slash(&resolved.mcp_bin)
        )));
    }
    let artifact = match platform {
        Platform::ClaudeCode => claude_code(resolved),
        Platform::Trae => trae(resolved),
        Platform::Codex => codex(resolved),
        Platform::Kimi => kimi(resolved),
        Platform::WorkBuddy => workbuddy(resolved),
        Platform::DeepSeekHarness => deepseek_harness(resolved),
        Platform::OpenClaw => openclaw(resolved),
        Platform::Hermes => hermes(resolved),
    };
    Ok(artifact)
}

fn single_writer_note() -> &'static str {
    "多平台同挂一份 WAL 时,第二个写进程 fail-closed 拒启(W-18 单写者锁)是特性不是缺陷"
}

/// 路径统一正斜杠:Windows 反斜杠在 JSON/YAML/TOML 里都要转义,正斜杠 Windows 也认。
fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn budget_arg() -> String {
    DEFAULT_BUDGET_CENTS.to_string()
}

/// first-run 三行引导(W-43a):重启 → 认工具 → 验闸。打在 stdout(不进配置文件)。
pub fn first_run_notes() -> Vec<String> {
    vec![
        "① 把生成的配置写进对应位置后,重启你的编码工具(配置只在启动时读取)。".into(),
        "② 确认 Wanning 已挂载:工具现身名 mcp__wanning__wanning_gate_evaluate(闸评估)与 mcp__wanning__wanning_audit_tail(读审计尾)。".into(),
        "③ 验证闸在工作:让 agent 试一笔超额消费(默认预算 1000 分 = ¥10),应被拒绝且 reason=over_budget;放行与拒绝都落审计账本,`wanning audit` 可对账。".into(),
    ]
}

fn json_artifact(value: serde_json::Value, mut notes: Vec<String>) -> Artifact {
    let mut content = serde_json::to_string_pretty(&value).expect("静态 JSON 序列化");
    content.push('\n');
    notes.push(first_run_note_line());
    Artifact { notes, content }
}

fn first_run_note_line() -> String {
    "装完三步:重启工具 → 认工具 mcp__wanning__wanning_gate_evaluate → 试一笔超额消费应被拒(over_budget)".to_string()
}

fn claude_code(resolved: &Resolved) -> Artifact {
    // 与仓内 .mcp.json 现物字段面全等(type: stdio,W-19 实测同款);严格 JSON →
    // 无注释,说明走 notes。W-43a 起命令/账本都是解析出的真实绝对路径。
    let value = json!({
        "mcpServers": {
            "wanning": {
                "type": "stdio",
                "command": slash(&resolved.mcp_bin),
                "args": ["--wal", slash(&resolved.wal), "--budget", budget_arg()]
            }
        }
    });
    json_artifact(
        value,
        vec![
            "Wanning 支付闸 — Claude Code MCP 配置(W-36 生成;W-43a 起写实路径)".into(),
            format!(
                "写入位置:项目根 .mcp.json。闸:{},审计账本:{}(每个项目目录可以各挂一份,互不相干)",
                slash(&resolved.mcp_bin),
                slash(&resolved.wal)
            ),
            "字段面依据仓内 .mcp.json 现物(W-19 真插实测):claude-code 需要 type: stdio,别的平台多半不需要".into(),
            single_writer_note().into(),
            "严格 JSON 不支持注释 → 文件内无注释行;实测与语义见 docs/research/mcp-consumption.md".into(),
        ],
    )
}

fn trae(resolved: &Resolved) -> Artifact {
    let value = json!({
        "mcpServers": {
            "wanning": {
                "command": slash(&resolved.mcp_bin),
                "args": ["--wal", slash(&resolved.wal), "--budget", budget_arg()]
            }
        }
    });
    json_artifact(
        value,
        vec![
            "Wanning 支付闸 — Trae MCP 配置(W-36 生成;W-43a 起写实路径)".into(),
            format!(
                "写入位置:项目根 .trae/mcp.json。闸:{},审计账本:{}",
                slash(&resolved.mcp_bin),
                slash(&resolved.wal)
            ),
            "字段面依据仓内 .trae/mcp.json 现物(W-17 直核):无 type 字段,command 不能含空格(含空格的路径已拒绝生成)".into(),
            single_writer_note().into(),
            "严格 JSON 不支持注释 → 文件内无注释行".into(),
        ],
    )
}

fn codex(resolved: &Resolved) -> Artifact {
    Artifact {
        notes: vec![
            "Wanning 支付闸 — Codex CLI MCP 配置片段(W-36 生成;W-43a 起写实路径,零占位符)".into(),
            format!(
                "追加到 ~/.codex/config.toml(全局)或 <repo>/.codex/config.toml(project-scoped,trust 机制待实测)。闸:{},审计账本:{}",
                slash(&resolved.mcp_bin),
                slash(&resolved.wal)
            ),
            single_writer_note().into(),
            "会话级使用需 OpenAI 登录(doctor ✗ auth);配置面免登录已实测(W-35)".into(),
            first_run_note_line(),
        ],
        content: format!(
            concat!(
                "# Wanning 支付闸 — Codex CLI MCP 配置片段(W-36 生成;W-43a 起写实路径;字段依据 W-35 调研 docs/research/codex-mcp.md)\n",
                "# 用法:追加到 ~/.codex/config.toml(全局)或 <repo>/.codex/config.toml(project-scoped,trust 机制待实测)\n",
                "# W-35 直核:codex 配置没有路径变量 → 本片段已是真实绝对路径,无需手改\n",
                "# 并发语义:多平台同挂一份 WAL 时,第二个写进程 fail-closed 拒启(W-18 单写者锁)是特性\n",
                "[mcp_servers.wanning]\n",
                "command = '{bin}'\n",
                "args = [\"--wal\", '{wal}', \"--budget\", \"{budget}\"]\n",
                "# 可选加固(文档字段,待 OpenAI 登录后实测):required = true —— server 起不来就 fail 启动,与闸 fail-closed 同构\n",
                "# cargo run 备选形态与 startup_timeout_sec 说明见 docs/plugins/codex.md\n",
            ),
            bin = slash(&resolved.mcp_bin),
            wal = slash(&resolved.wal),
            budget = budget_arg(),
        ),
    }
}

fn kimi(resolved: &Resolved) -> Artifact {
    // 字段权威 = W-40 本机隔离实验(docs/research/kimi-code-cli.md):
    // 本机 kimi-code 0.39.1 实测无 `kimi mcp` 子命令(W-17 直核的 kimi mcp add 属
    // legacy kimi-cli 挂法,所有者机器 ~/.kimi → ~/.kimi-code 迁移痕迹佐证);官方挂法
    // = $KIMI_CODE_HOME/mcp.json(用户级)或 <repo>/.kimi-code/mcp.json(项目级),
    // mcpServers → command/args/env;官方文档与 W-40 实验现物均无 type 字段
    // (stdio 由 command 字段隐含),未提及 ${...} 变量(W-43a 起直接写实路径)。
    // W-40 实验同款形态被真 kimi 二进制接受并完成 MCP 往返(allow/replay/
    // over_budget 三判定落 WAL)。严格 JSON → 无注释,说明走 notes。
    let value = json!({
        "mcpServers": {
            "wanning": {
                "command": slash(&resolved.mcp_bin),
                "args": ["--wal", slash(&resolved.wal), "--budget", budget_arg()]
            }
        }
    });
    json_artifact(
        value,
        vec![
            "Wanning 支付闸 — Kimi Code CLI MCP 配置(W-36 生成;W-40 按本机实测修订;W-43a 起写实路径)".into(),
            "写入位置:用户级 ~/.kimi-code/mcp.json(或 $KIMI_CODE_HOME/mcp.json,所有项目生效)或 <repo>/.kimi-code/mcp.json(单项目)".into(),
            format!(
                "kimi-code 无 ${{...}} 路径变量(W-40 官方文档直核)→ 本配置已是真实绝对路径:闸 {},审计账本 {}",
                slash(&resolved.mcp_bin),
                slash(&resolved.wal)
            ),
            "项目级 .kimi-code/mcp.json 在未信任目录会弹 workspace trust 提示(默认拒绝信任)——核对其中列出的命令后再确认;用户级挂法不经 trust 提示".into(),
            "TUI 内交互管理:/mcp-config(增删改)、/mcp(看连接状态)".into(),
            single_writer_note().into(),
            "W-40 已实测:真 kimi 0.39.1 二进制拉起 wanning-mcp,工具注入 + 放行/重放拒/超额拒三判定落 WAL(模型侧为本地 mock,真实模型会话待所有者放行烧额度)".into(),
        ],
    )
}

fn workbuddy(resolved: &Resolved) -> Artifact {
    // 字段权威 = workbuddy.cn 官方 MCP-Guide(W-37 直核,docs/research/workbuddy.md):
    // mcpServers → 名字键 → command/args/env;官方示例无 type 字段;未提及 ${...}
    // 变量(W-43a 起直接写实路径)。严格 JSON → 无注释,说明走 notes。
    let value = json!({
        "mcpServers": {
            "wanning": {
                "command": slash(&resolved.mcp_bin),
                "args": ["--wal", slash(&resolved.wal), "--budget", budget_arg()]
            }
        }
    });
    json_artifact(
        value,
        vec![
            "Wanning 支付闸 — WorkBuddy MCP 配置(W-36 生成,字段依据 W-37 直核官方 MCP-Guide;W-43a 起写实路径)".into(),
            "写入位置:用户级 ~/.workbuddy/mcp.json(所有项目生效)或 <项目目录>/.workbuddy/mcp.json(单项目)".into(),
            format!(
                "WorkBuddy 文档未提及路径变量 → 本配置已是真实绝对路径:闸 {},审计账本 {}",
                slash(&resolved.mcp_bin),
                slash(&resolved.wal)
            ),
            "官方示例字段面无 type(与 claude-code 现物带 type:stdio 是刻意差异);也可走 UI:侧边栏 插件 → MCP 服务器 → 配置 MCP".into(),
            "传输形态按官方命令启动式示例推断 stdio,真插实测待所有者桌面端(待实测项)".into(),
            single_writer_note().into(),
        ],
    )
}

fn deepseek_harness(resolved: &Resolved) -> Artifact {
    // 字段权威 = W-44 任务书直核(官方 docs/user/guide/mcp-memory.md 的通用
    // Cordis overlay patch 格式)+ 本机 dsh 0.1.0-rc.7 包内
    // @deepseek-ai/dsh-mcp-client README(字段表 serverName/transport/command/
    // args/env/cwd;工具命名 mcp__<serverName>__<rawName>;环境变量经
    // scrubbedParentEnv 丢弃 credential-shaped 与 DSH_* 后显式 env 才合并)。
    // patch entry 形态 = 顶层 YAML 数组的 `- insert:` 列表(dsh 官方 patch 语义,
    // boss 侧 headless profile 自带说明「insert lists」同证)。js-tag 按官方示例
    // 原样生成,不自创字段。真实 dsh 二进制取证(零网络零会话):隔离 DSH_HOME 下
    // `dsh --profile headless --dump-config --patch <本文件>` exit 0,wanning 行作为
    // 独立 patch 层进入组合树;坏 YAML 对照组 exit 1(取证在档
    // W-44 节)。
    let content = format!(
        concat!(
            "# Wanning 支付闸 — DeepSeek Harness (dsh) Cordis overlay patch(W-44 生成;W-43a 起写实路径)\n",
            "# 启用二选一:\n",
            "#   临时:dsh --profile <名> --patch <本文件>\n",
            "#   持久:把下面 insert 块合并追加进 <profile>/cordis.patch.yml 或\n",
            "#         $DSH_HOME/cordis.patch.yml(合并追加,绝不整文件覆盖)\n",
            "- insert:\n",
            "    - id: wanning-gate                 # 唯一 id\n",
            "      name: '@deepseek-ai/dsh-mcp-client'\n",
            "      config:\n",
            "        serverName: wanning            # 工具将现身为 mcp__wanning__wanning_gate_evaluate\n",
            "        transport: stdio\n",
            "        command: {bin}\n",
            "        args: [\"--wal\", \"{wal}\", \"--budget\", \"{budget}\"]\n",
            "        env: {{}}\n",
            "        cwd: !!js process.cwd()\n",
        ),
        bin = slash(&resolved.mcp_bin),
        wal = slash(&resolved.wal),
        budget = budget_arg(),
    );
    Artifact {
        notes: vec![
            "Wanning 支付闸 — DeepSeek Harness (dsh) Cordis overlay patch(W-36 生成,W-44 按官方格式入矩阵;W-43a 起写实路径)".into(),
            format!(
                "dsh 用 Cordis overlay YAML patch 声明 MCP server(不是 mcp.json);本文件是 patch entry,落盘惯用名 *.cordis.yml(--out 显式给路径,已存在绝不覆盖)。闸 {},审计账本 {}",
                slash(&resolved.mcp_bin),
                slash(&resolved.wal)
            ),
            "启用二选一:临时 dsh --profile <名> --patch <本文件>;持久 = 把 insert 块合并追加进 <profile>/cordis.patch.yml 或 $DSH_HOME/cordis.patch.yml(合并追加,绝不整文件覆盖)".into(),
            "工具现身名:mcp__wanning__wanning_gate_evaluate / mcp__wanning__wanning_audit_tail(serverName: wanning → mcp__<serverName>__<tool>,官方命名契约,与 Claude Code/Codex 同形)".into(),
            "dsh stdio 桥启动子进程前丢弃 ambient credential-shaped 与全部 DSH_* 环境变量(scrubbedParentEnv),其余照常继承 → 将来接真实通道时密钥必须写进本 row 的 config.env,不能赌继承".into(),
            single_writer_note().into(),
            "可选加固:config.failOnStartupError: true(默认 false = 闸起不来插件仍激活但零工具,闸位形同虚设;置 true 则 dsh 拒绝激活,与闸 fail-closed 同构)".into(),
            "dsh 0.1.0-rc.7 = developer preview,官方明示会有破坏性变更——升级后本配置可能要跟着改".into(),
            "本机 dsh 0.1.0-rc.7 已实测:--dump-config --patch 接受本格式(W-44,隔离 DSH_HOME,零网络零会话);会话级端到端待所有者放行(dsh 会话 = 模型会话 + 网络,红线 2)".into(),
            first_run_note_line(),
        ],
        content,
    }
}

fn openclaw(resolved: &Resolved) -> Artifact {
    // 字段权威 = 本机 OpenClaw 2026.5.22 (a374c3a) 隔离实测(W-45,取证落
    // W-45 节(取证在档)):`openclaw mcp set wanning '<json>'` 落
    // $OPENCLAW_STATE_DIR/openclaw.json 的 mcp.servers.<name> = {command, args}
    // (与 Claude Code mcpServers 同形,无 type 字段);官方 docs.openclaw.ai/mcp
    // 直核:stdio 字段 command/args/env/cwd + env 安全过滤(env 中拦 NODE_OPTIONS/
    // PYTHONSTARTUP/DYLD_*/LD_* 等键)。产出 = CLI 命令行:openclaw.json 由宿主
    // 自己管理(实测落盘含 commands/messages/agents/meta 骨架段),`mcp set` 只动
    // mcp.servers.wanning 一段,「绝不覆盖」天然成立。诚实边界:W-45 实测到配置面
    // + models.providers 挂本地 mock 模型为止,工具现身与判定落 WAL 属 agent 回合,
    // 需 gateway/模型会话(烧额度,红线 2,所有者放行)。
    let payload = serde_json::to_string(&json!({
        "command": slash(&resolved.mcp_bin),
        "args": ["--wal", slash(&resolved.wal), "--budget", budget_arg()]
    }))
    .expect("静态 JSON 序列化");
    Artifact {
        notes: vec![
            "Wanning 支付闸 — OpenClaw MCP 配置(W-45 生成;字段依据本机 2026.5.22 隔离实测 + docs.openclaw.ai/mcp 直核)".into(),
            "执行下面这条命令即完成写入(openclaw.json 由宿主管理,openclaw mcp set 只动 mcp.servers.wanning 一段,绝不整文件覆盖)".into(),
            format!(
                "配置落点:openclaw.json 的 mcp.servers.wanning = {{command, args}}。闸 {},审计账本 {}",
                slash(&resolved.mcp_bin),
                slash(&resolved.wal)
            ),
            "OpenClaw 2026.5.22 原生支持 MCP(mcp list/show/set/unset 子命令族);W-45 隔离 env(OPENCLAW_STATE_DIR/OPENCLAW_CONFIG_PATH)实测 set/list/show 全绿".into(),
            "stdio 字段面(官方文档直核):command/args/env/cwd;env 有安全过滤,拦 NODE_OPTIONS/PYTHONSTARTUP/DYLD_*/LD_* 等键 → 将来接真实通道时密钥必须写进 env,不能赌继承".into(),
            "诚实边界:W-45 实测到配置面 + models.providers 挂本地 mock 模型为止;工具现身与判定落 WAL 属 agent 回合,需 gateway/模型会话(烧额度,红线 2,所有者放行)".into(),
            single_writer_note().into(),
            first_run_note_line(),
        ],
        content: format!("openclaw mcp set wanning '{payload}'\n"),
    }
}

fn hermes(resolved: &Resolved) -> Artifact {
    // 字段权威 = 本机 hermes-agent v0.19.1 (2026.7.30) 隔离实测(W-45):
    // `hermes mcp add wanning --command <bin> --args <args…>` discovery-first
    // 即真连发现工具(实测 ✓ Connected, Found 2 tool(s), 2/2 enabled),落
    // $HERMES_HOME/config.yaml 的 mcp_servers.<name> = {command, args, enabled:
    // true};`hermes mcp test wanning` 真连 141ms;`hermes -z -t wanning` + 本地
    // mock LLM → allow 400 落 WAL,二次会话同 nonce → replay 拒(链连续)。
    // 工具现身名 mcp__wanning__wanning_gate_evaluate / …_audit_tail(deferred
    // catalog),模型侧经 tool_call(name, arguments) 间接调用——直接调 mcp__ 名
    // 报 does not exist(W-45 实测教训,写进 notes)。产出 = CLI 命令行(挂载即
    // 验证;--args 必须是最后一个选项)。
    let content = format!(
        "hermes mcp add wanning --command {bin} --args --wal {wal} --budget {budget}\n",
        bin = slash(&resolved.mcp_bin),
        wal = slash(&resolved.wal),
        budget = budget_arg(),
    );
    Artifact {
        notes: vec![
            "Wanning 支付闸 — Hermes Agent MCP 配置(W-45 生成;字段依据本机 hermes v0.19.1 隔离实测 + 包内 cli-config.yaml.example 直核)".into(),
            format!(
                "执行下面这条命令即完成挂载(mcp add 是 discovery-first:真连一次发现工具,挂载即验证)。配置落点:$HERMES_HOME/config.yaml 的 mcp_servers 段。闸 {},审计账本 {}",
                slash(&resolved.mcp_bin),
                slash(&resolved.wal)
            ),
            "终端里跑会问 Enable all 2 tools? [Y/n] 回车即全开;脚本/CI 无 TTY 场景用 echo y | 管道喂确认(W-45 实测)".into(),
            "落盘形态(实测原文):mcp_servers.wanning = {command: <bin>, args: [--wal, <wal>, --budget, '1000'], enabled: true};管理用 hermes mcp list / mcp test wanning / mcp remove wanning".into(),
            "工具现身名:mcp__wanning__wanning_gate_evaluate / mcp__wanning__wanning_audit_tail(与 Claude Code/Codex/dsh 同形);hermes 把 MCP 工具放进 deferred catalog,模型侧经 tool_call(name, arguments) 间接调用——直接调 mcp__ 名会报 does not exist(W-45 实测教训)".into(),
            "one-shot 会话要显式带 toolset:hermes -z \"…\" -t wanning(默认 cli 工具集不含 MCP 工具,W-45 实测)".into(),
            "W-45 已实测(真 hermes 二进制 + 本地 mock LLM,零外网零真实消费):allow 400 分落 WAL;二次会话同 nonce → replay 拒,完整性链连续;真实模型会话待所有者放行烧额度(红线 2)".into(),
            single_writer_note().into(),
            first_run_note_line(),
        ],
        content,
    }
}

// ── CLI 面(W-43a:统一入口 `wanning init` 与旧 bin `wanning-init` 共用) ────

const USAGE: &str = "wanning-init:给编码工具吐 Wanning MCP 配置(零网络、零真实消费)

用法: wanning-init --platform <名> [--bin <wanning-mcp 路径>] [--wal <审计账本路径>] [--out <文件>]

  --platform <名>  目标平台(必填):claude-code / codex / kimi / trae / workbuddy /
                   deepseek-harness / openclaw / hermes
  --bin <路径>     wanning-mcp 可执行文件;缺省从 PATH 解析(找不到 = 拒,给安装指引)
  --wal <路径>     审计 WAL 路径;缺省 = 产品默认 ~/.wanning/wal.jsonl(Windows %USERPROFILE%\\.wanning)
  --out <文件>     落盘路径;缺省只打印 stdout。已存在的文件**绝不覆盖**(动别人工具的配置 = 危险动作)
  -h / --help      打印本说明后退出
";

/// CLI 错误分层:用法错(退出码 2)与运行失败(退出码 1)。
enum CliError {
    Usage(String),
    Failed(String),
}

/// 统一 CLI 主体:旧 bin `wanning-init` 与统一入口 `wanning init` 走同一段逻辑
/// (`program` 只进报错前缀,保证两个入口的报错口径一致)。
/// 退出码:0 成功;2 用法错(参数缺失/未知);1 运行失败(解析不到路径/拒绝覆盖)。
pub fn run_cli(program: &str, args: &[String]) -> ExitCode {
    match cli_run(program, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(CliError::Usage(message)) => {
            eprintln!("{program}: {message}");
            ExitCode::from(2)
        }
        Err(CliError::Failed(message)) => {
            eprintln!("{program}: {message}");
            ExitCode::FAILURE
        }
    }
}

fn cli_run(program: &str, args: &[String]) -> Result<(), CliError> {
    let mut platform: Option<String> = None;
    let mut mcp_bin: Option<PathBuf> = None;
    let mut wal: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--platform" => platform = Some(next_value(args, &mut index, "--platform")?),
            "--bin" => mcp_bin = Some(next_path(args, &mut index, "--bin")?),
            "--wal" => wal = Some(next_path(args, &mut index, "--wal")?),
            "--out" => out = Some(next_path(args, &mut index, "--out")?),
            other => {
                return Err(CliError::Usage(format!(
                    "未知参数: {other}(用 --help 看用法)"
                )))
            }
        }
        index += 1;
    }
    let Some(platform) = platform else {
        return Err(CliError::Usage(format!(
            "缺少 --platform <名>(支持矩阵:claude-code / codex / kimi / trae / workbuddy / \
             deepseek-harness / openclaw / hermes;--help 看用法;{program} 是 Wanning 的配置生成器)"
        )));
    };
    let platform = parse_platform(&platform).map_err(|e| CliError::Usage(e.message()))?;

    let resolved =
        resolve(&GenerateOptions { mcp_bin, wal }).map_err(|e| CliError::Failed(e.message()))?;
    let artifact = generate_with(platform, &resolved).map_err(|e| CliError::Failed(e.message()))?;
    println!("# Wanning 支付闸 — 配置生成完成");
    for note in &artifact.notes {
        println!("# {note}");
    }
    for note in first_run_notes() {
        println!("# {note}");
    }
    println!();

    match out {
        Some(out) => {
            let content = artifact.content;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&out)
                .and_then(|mut file| std::io::Write::write_all(&mut file, content.as_bytes()))
                .map_err(|e| {
                    CliError::Failed(if e.kind() == std::io::ErrorKind::AlreadyExists {
                        format!(
                            "拒绝覆盖:{} 已存在。动别人工具的配置 = 危险动作;请先人工确认,\
                             换个文件名,或把已有内容备份后删掉再生成",
                            out.display()
                        )
                    } else {
                        format!("写 {} 失败: {e}", out.display())
                    })
                })?;
            println!("已写入:{}(绝不覆盖已存在文件)", out.display());
        }
        None => print!("{content}", content = artifact.content),
    }
    Ok(())
}

fn next_value(args: &[String], index: &mut usize, flag: &str) -> Result<String, CliError> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| CliError::Usage(format!("{flag} 缺少取值(用 --help 看用法)")))
}

fn next_path(args: &[String], index: &mut usize, flag: &str) -> Result<PathBuf, CliError> {
    Ok(PathBuf::from(next_value(args, index, flag)?))
}
