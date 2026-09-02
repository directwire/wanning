//! wanning-init:给编码工具吐 Wanning MCP 配置的生成器(W-36)。
//!
//! 产品边界(与 wanning-demo 演示台分开):本 crate 是**对端工具的接入生成器**——
//! 零网络、零真实消费、零文件副作用(默认只打印 stdout;写文件必须显式 `--out`
//! 且绝不覆盖已存在文件,动别人工具的配置 = 危险动作,拒)。
//!
//! 平台契约来源(零编造):
//! - claude-code:仓内 `.mcp.json` 现物(W-19 真插实测),`${CLAUDE_PROJECT_DIR:-.}`
//!   路径变量;
//! - trae:仓内 `.trae/mcp.json` 现物,`${workspaceFolder}` 路径变量(W-17 直核);
//! - codex:`~/.codex/config.toml` 的 `[mcp_servers.<id>]` 片段,W-35 直核**无路径
//!   变量** → 绝对路径占位符 + TOML `#` 注释;
//! - kimi:`.kimi-code/mcp.json`(用户级 `$KIMI_CODE_HOME/mcp.json` 或项目级),
//!   W-40 本机隔离实验修订——kimi-code 0.39.1 实测无 `kimi mcp` 子命令(W-17 的
//!   `kimi mcp add` 属 legacy kimi-cli 挂法),mcpServers 形态无 `type` 字段、
//!   无 `${...}` 变量 → 绝对路径占位符,严格 JSON 无注释;
//! - workbuddy:`.workbuddy/mcp.json`(W-37 直核官方 MCP-Guide),`mcpServers` 结构
//!   同款但字段面无 `type`(官方示例只有 command/args/env),文档未提及 `${...}`
//!   变量 → 绝对路径占位符;W-17 曾查不到,W-37 换路数(robots/sitemap 绕开 JS
//!   首页)破冰,见 docs/research/workbuddy.md。
//!
//! 注释纪律:各工具注释语法不同——TOML/shell 用 `#` 行内注释;**严格 JSON 没有
//! 注释语法**,claude-code/trae 的说明只能打在 stdout 的 [`Artifact::notes`]
//! (文件内容保持纯净 JSON,防解析崩)。

use serde_json::json;

/// 支持的平台(生成器矩阵;字段权威=仓内现物与调研文档,见模块文档)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    ClaudeCode,
    Codex,
    Kimi,
    Trae,
    WorkBuddy,
}

/// 生成失败:未知平台,fail-closed 列全矩阵。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitError {
    UnknownPlatform(String),
}

impl InitError {
    /// 人可读报错;未知平台必须列出全矩阵(契约测试锁定)。
    pub fn message(&self) -> String {
        match self {
            InitError::UnknownPlatform(input) => format!(
                "未知平台 '{input}'。--platform 支持矩阵:\n  \
                 claude-code   → 项目根 .mcp.json(${{CLAUDE_PROJECT_DIR:-.}} 变量;W-19 实测)\n  \
                 codex         → config.toml [mcp_servers.wanning] 片段(无路径变量,占位符+注释;W-35)\n  \
                 kimi          → .kimi-code/mcp.json(无 type 无变量,占位符;W-40 实测)\n  \
                 trae          → .trae/mcp.json(${{workspaceFolder}} 变量;W-17)\n  \
                 workbuddy     → .workbuddy/mcp.json(无 type 无变量,占位符;W-37 直核)\n\
                 未知值 fail-closed,绝不猜。"
            ),
        }
    }
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
        other => Err(InitError::UnknownPlatform(other.to_string())),
    }
}

/// 生成一份平台配置。内容与仓内现物/调研逐字段契约锁定
/// (见 `crates/wanning-init/tests/matrix.rs`)。
pub fn generate(platform: Platform) -> Artifact {
    match platform {
        Platform::ClaudeCode => claude_code(),
        Platform::Trae => trae(),
        Platform::Codex => codex(),
        Platform::Kimi => kimi(),
        Platform::WorkBuddy => workbuddy(),
    }
}

fn single_writer_note() -> &'static str {
    "多平台同挂一份 WAL 时,第二个写进程 fail-closed 拒启(W-18 单写者锁)是特性不是缺陷"
}

fn claude_code() -> Artifact {
    // 与仓内 .mcp.json 现物语义全等(契约测试);严格 JSON → 无注释,说明走 notes。
    let value = json!({
        "mcpServers": {
            "wanning": {
                "type": "stdio",
                "command": "cargo",
                "args": [
                    "run", "--quiet", "-p", "wanning-mcp", "--",
                    "--wal", "${CLAUDE_PROJECT_DIR:-.}/target/mcp-demo.wal"
                ]
            }
        }
    });
    let mut content = serde_json::to_string_pretty(&value).expect("静态 JSON 序列化");
    content.push('\n');
    Artifact {
        notes: vec![
            "Wanning 支付闸 — Claude Code MCP 配置(W-36 生成)".into(),
            "写入位置:项目根 .mcp.json(与仓内现物逐字段契约测试锁定)".into(),
            "WAL 落 ${CLAUDE_PROJECT_DIR:-.}/target/mcp-demo.wal —— 每个 Claude Code 项目目录一本账".into(),
            single_writer_note().into(),
            "严格 JSON 不支持注释 → 文件内无注释行;实测与语义见 docs/research/mcp-consumption.md".into(),
        ],
        content,
    }
}

fn trae() -> Artifact {
    let value = json!({
        "mcpServers": {
            "wanning": {
                "command": "cargo",
                "args": [
                    "run", "--quiet", "-p", "wanning-mcp", "--",
                    "--wal", "${workspaceFolder}/target/mcp-demo.wal"
                ]
            }
        }
    });
    let mut content = serde_json::to_string_pretty(&value).expect("静态 JSON 序列化");
    content.push('\n');
    Artifact {
        notes: vec![
            "Wanning 支付闸 — Trae MCP 配置(W-36 生成)".into(),
            "写入位置:项目根 .trae/mcp.json(与仓内现物逐字段契约测试锁定)".into(),
            "WAL 落 ${workspaceFolder}/target/mcp-demo.wal —— 每个 Trae 工作区一本账".into(),
            single_writer_note().into(),
            "严格 JSON 不支持注释 → 文件内无注释行;路径变量依据 W-17 直核".into(),
        ],
        content,
    }
}

fn codex() -> Artifact {
    Artifact {
        notes: vec![
            "Wanning 支付闸 — Codex CLI MCP 配置片段(W-36 生成)".into(),
            "codex 配置没有路径变量(W-35 直核)→ 占位符 {{WANNING_BIN}} / {{WAL_PATH}} 必须手改成绝对路径".into(),
            single_writer_note().into(),
            "会话级使用需 OpenAI 登录(doctor ✗ auth);配置面免登录已实测(W-35)".into(),
        ],
        content: concat!(
            "# Wanning 支付闸 — Codex CLI MCP 配置片段(W-36 生成;字段依据 W-35 调研 docs/research/codex-mcp.md)\n",
            "# 用法:追加到 ~/.codex/config.toml(全局)或 <repo>/.codex/config.toml(project-scoped,trust 机制待实测)\n",
            "# W-35 直核:codex 配置没有路径变量 → 下面两个占位符必须手改成绝对路径:\n",
            "#   {{WANNING_BIN}} = wanning-mcp 可执行文件绝对路径(先在 Wanning 仓 cargo build -p wanning-mcp)\n",
            "#   {{WAL_PATH}}    = 审计 WAL 绝对路径(如 D:\\path\\to\\Wanning\\target\\mcp-demo.wal)\n",
            "# 并发语义:多平台同挂一份 WAL 时,第二个写进程 fail-closed 拒启(W-18 单写者锁)是特性\n",
            "[mcp_servers.wanning]\n",
            "command = '{{WANNING_BIN}}'\n",
            "args = [\"--wal\", '{{WAL_PATH}}']\n",
            "# 可选加固(文档字段,待 OpenAI 登录后实测):required = true —— server 起不来就 fail 启动,与闸 fail-closed 同构\n",
            "# cargo run 备选形态与 startup_timeout_sec 说明见 docs/plugins/codex.md\n",
        )
        .to_string(),
    }
}

fn kimi() -> Artifact {
    // 字段权威 = W-40 本机隔离实验(docs/research/kimi-code-cli.md):
    // 本机 kimi-code 0.39.1 实测无 `kimi mcp` 子命令(W-17 直核的 kimi mcp add 属
    // legacy kimi-cli 挂法,老板机器 ~/.kimi → ~/.kimi-code 迁移痕迹佐证);官方挂法
    // = $KIMI_CODE_HOME/mcp.json(用户级)或 <repo>/.kimi-code/mcp.json(项目级),
    // mcpServers → command/args/env;官方文档与 W-40 实验现物均无 type 字段
    // (stdio 由 command 字段隐含),未提及 ${...} 变量 → 绝对路径占位符。
    // W-40 实验同款形态被真 kimi 二进制接受并完成 MCP 往返(allow/replay/
    // over_budget 三判定落 WAL)。严格 JSON → 无注释,说明走 notes。
    let value = json!({
        "mcpServers": {
            "wanning": {
                "command": "{{WANNING_BIN}}",
                "args": ["--wal", "{{WAL_PATH}}"]
            }
        }
    });
    let mut content = serde_json::to_string_pretty(&value).expect("静态 JSON 序列化");
    content.push('\n');
    Artifact {
        notes: vec![
            "Wanning 支付闸 — Kimi Code CLI MCP 配置(W-36 生成,W-40 按本机实测修订)".into(),
            "写入位置:用户级 ~/.kimi-code/mcp.json(或 $KIMI_CODE_HOME/mcp.json,所有项目生效)或 <repo>/.kimi-code/mcp.json(单项目)".into(),
            "kimi-code 无 ${...} 路径变量(W-40 官方文档直核)→ 两个占位符必须手改成绝对路径:".into(),
            "  {{WANNING_BIN}} = wanning-mcp 可执行文件绝对路径(先在 Wanning 仓 cargo build -p wanning-mcp)".into(),
            "  {{WAL_PATH}}    = 审计 WAL 绝对路径".into(),
            "项目级 .kimi-code/mcp.json 在未信任目录会弹 workspace trust 提示(默认拒绝信任)——核对其中列出的命令后再确认;用户级挂法不经 trust 提示".into(),
            "TUI 内交互管理:/mcp-config(增删改)、/mcp(看连接状态)".into(),
            single_writer_note().into(),
            "W-40 已实测:真 kimi 0.39.1 二进制拉起 wanning-mcp,工具注入 + 放行/重放拒/超额拒三判定落 WAL(模型侧为本地 mock,真实模型会话待 kimi 账号登录)".into(),
        ],
        content,
    }
}

fn workbuddy() -> Artifact {
    // 字段权威 = workbuddy.cn 官方 MCP-Guide(W-37 直核,docs/research/workbuddy.md):
    // mcpServers → 名字键 → command/args/env;官方示例无 type 字段;未提及 ${...}
    // 变量 → WAL 路径占位符手改绝对路径。严格 JSON → 无注释,说明走 notes。
    let value = json!({
        "mcpServers": {
            "wanning": {
                "command": "cargo",
                "args": [
                    "run", "--quiet", "-p", "wanning-mcp", "--",
                    "--wal", "{{WAL_PATH}}"
                ]
            }
        }
    });
    let mut content = serde_json::to_string_pretty(&value).expect("静态 JSON 序列化");
    content.push('\n');
    Artifact {
        notes: vec![
            "Wanning 支付闸 — WorkBuddy MCP 配置(W-36 生成,字段依据 W-37 直核官方 MCP-Guide)".into(),
            "写入位置:用户级 ~/.workbuddy/mcp.json(所有项目生效)或 <项目目录>/.workbuddy/mcp.json(单项目)"
                .into(),
            "WorkBuddy 文档未提及路径变量 → {{WAL_PATH}} 必须手改成审计 WAL 绝对路径".into(),
            "官方示例字段面无 type(与 claude-code 现物带 type:stdio 是刻意差异);也可走 UI:侧边栏 插件 → MCP 服务器 → 配置 MCP".into(),
            "传输形态按官方命令启动式示例推断 stdio,真插实测待老板桌面端(待实测项,boss-checklist)".into(),
            single_writer_note().into(),
        ],
        content,
    }
}
