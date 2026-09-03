//! W-51a:`wanning init --install` 直写安装——消掉「init 只打印、用户手动贴配置」断点。
//!
//! 职责边界(与生成器面的关系):条目内容**永远**来自 [`crate::generate_with`]
//! 的产物(单一事实来源:install 只负责「放进宿主的正确位置」,不另写一份
//! 字段面)——四个 mcp.json 平台解析产物里的 `mcpServers.wanning`,dsh 取产物
//! 里的 `- insert:` 块,openclaw/hermes 的宿主命令行原样来自产物。
//!
//! 纪律(扩展 W-36「绝不覆盖」):
//! - 写前必读现有文件;merge 只动 `mcpServers.wanning` / `wanning-gate` 块,
//!   他人条目语义不动(mcp.json)或逐字节不动(cordis.patch.yml 追加在尾);
//! - 写前备份 `<file>.wanning.bak`(先备份、后写入);
//! - 已有 wanning 条目且内容一致 = 已是最新:逐字节不动、不产生备份、无 diff;
//! - 升级场景打字段级 diff;`--dry-run` 打印将做的全部动作、零落盘(连目录都不建);
//! - codex 主配置是 TOML 文本面,文本合并的风险大于收益 → fail-closed 不支持
//!   (报错给 `--out` 人工指引,绝不乱写主配置);
//! - openclaw/hermes 产出宿主 CLI 命令行,仅 `--yes` 显式时才执行;执行前解析
//!   宿主真实路径(显式 `--host-bin` 优先,否则 PATH),解析不到或宿主退出码
//!   非 0 一律 fail-closed。
//!
//! 库层不读进程环境:一切宿主路径由 CLI 层显式传入([`InstallEnv`]),测试确定性;
//! `read_installed_entry` 是 doctor(W-51b)与安装面共用的读取口。

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::{budget_arg, generate_with, slash, Platform, Resolved};

/// 安装环境(全部显式传入;CLI 层负责读进程环境,库层不读 → 测试确定性)。
#[derive(Debug, Clone, Copy)]
pub struct InstallEnv<'a> {
    /// 项目根:`.mcp.json` / `.trae/mcp.json` / `.kimi-code/mcp.json` /
    /// `.workbuddy/mcp.json` 都落在这里(项目级挂法)。
    pub cwd: &'a Path,
    /// 用户主目录(doctor 用户级扫描备用;install 面只动项目级/显式路径)。
    pub home: Option<&'a Path>,
    /// `$DSH_HOME`(dsh 配置根);deepseek-harness 落 `$DSH_HOME/cordis.patch.yml`。
    pub dsh_home: Option<&'a Path>,
    /// `$OPENCLAW_STATE_DIR`(openclaw.json 所在目录)。
    pub openclaw_state_dir: Option<&'a Path>,
    /// `$HERMES_HOME`(hermes config.yaml 所在目录)。
    pub hermes_home: Option<&'a Path>,
    /// `$KIMI_CODE_HOME`(kimi 用户级配置根;install 只写项目级 `.kimi-code/`)。
    pub kimi_code_home: Option<&'a Path>,
    /// `$CODEX_HOME`(codex config.toml 所在目录;doctor 读,codex 不支持 install)。
    pub codex_home: Option<&'a Path>,
    /// PATH(宿主 CLI 解析用;`None` = 无 PATH)。
    pub path_env: Option<&'a OsStr>,
}

/// 安装入参。
#[derive(Debug, Clone, Copy)]
pub struct InstallOptions<'a> {
    pub platform: Platform,
    pub resolved: &'a Resolved,
    pub env: &'a InstallEnv<'a>,
    /// 只打印将做的全部动作,零落盘。
    pub dry_run: bool,
    /// openclaw/hermes 执行宿主 CLI 的显式确认(缺省只打印命令行)。
    pub yes: bool,
    /// 宿主 CLI 可执行文件显式路径(测试/特殊安装布局);缺省按 PATH 解析。
    pub host_bin: Option<&'a Path>,
}

/// 安装结果状态(人可读文案见 [`InstallState::label`])。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallState {
    /// 全新创建(此前没有配置文件/条目)。
    Fresh,
    /// 升级更新(原有 wanning 条目被替换,旧文件已备份)。
    Updated,
    /// 已是最新:逐字节未动,无备份无 diff。
    AlreadyCurrent,
    /// 已执行宿主 CLI(`--yes`)。
    HostExecuted,
    /// 只打印宿主命令行(未执行;加 `--yes` 执行)。
    HostPrinted,
    /// dry-run:打印了将做的动作,零落盘。
    DryRun,
}

impl InstallState {
    pub fn label(self) -> &'static str {
        match self {
            InstallState::Fresh => "已写入(全新创建)",
            InstallState::Updated => "已更新(原文件已备份)",
            InstallState::AlreadyCurrent => "已是最新,逐字节未动",
            InstallState::HostExecuted => "已执行宿主 CLI",
            InstallState::HostPrinted => "已打印宿主命令行(未执行;加 --yes 执行)",
            InstallState::DryRun => "dry-run(不落盘)",
        }
    }
}

/// 安装报告(CLI 打印成「安装报告」块)。
#[derive(Debug)]
pub struct InstallReport {
    pub state: InstallState,
    /// 实际落点(宿主 CLI 平台与 dry-run 为 `None`)。
    pub target: Option<PathBuf>,
    /// 写前备份路径(全新创建/未改动/dry-run/宿主 CLI 为 `None`)。
    pub backup: Option<PathBuf>,
    /// 升级场景的字段级 diff(`- ` 旧行 / `+ ` 新行)。
    pub diff: Vec<String>,
    /// 将做/已做的动作列表(dry-run 也非空)。
    pub actions: Vec<String>,
    /// openclaw/hermes 的宿主命令行(打印与执行同源)。
    pub printed: Option<String>,
}

/// 已安装条目(doctor 复用的读取面)。
#[derive(Debug)]
pub struct InstalledEntry {
    /// 配置文件路径。
    pub path: PathBuf,
    pub command: String,
    pub args: Vec<String>,
}

/// 安装失败。全部 fail-closed:宁可拒装,绝不产出一份坏的/半吊子的配置。
#[derive(Debug)]
pub enum InstallError {
    /// 平台不支持 `--install`(codex:TOML 主配置文本合并风险大,给人工指引)。
    Unsupported(String),
    /// 落点解析不出(如 DSH_HOME 未设;不猜落点)。
    TargetUnresolved(String),
    /// 现有配置形状不对(损坏 JSON/顶层不是对象/不是 insert 列表),拒绝动它。
    BadExisting(String),
    /// 文件系统错误。
    Io(String),
    /// 宿主 CLI 解析不到/无法启动。
    HostNotFound(String),
    /// 宿主 CLI 执行失败(退出码非 0)。
    HostFailed(String),
    /// 生成产物异常(install 依赖生成器,生成失败即拒装)。
    Generate(String),
}

impl InstallError {
    pub fn message(&self) -> String {
        match self {
            InstallError::Unsupported(message)
            | InstallError::TargetUnresolved(message)
            | InstallError::BadExisting(message)
            | InstallError::Io(message)
            | InstallError::HostNotFound(message)
            | InstallError::HostFailed(message)
            | InstallError::Generate(message) => message.clone(),
        }
    }
}

/// 执行安装(按平台分发)。
pub fn install(options: &InstallOptions) -> Result<InstallReport, InstallError> {
    match options.platform {
        Platform::Codex => Err(InstallError::Unsupported(
            "codex 主配置是 TOML 文本面(config.toml),文本合并的风险大于收益,本版不支持 \
             --install 直写;用 `wanning init --platform codex --out <path>` 生成片段后按 \
             docs/plugins/codex.md 人工追加"
                .to_string(),
        )),
        Platform::DeepSeekHarness => install_dsh(options),
        Platform::OpenClaw | Platform::Hermes => install_host(options),
        Platform::ClaudeCode | Platform::Kimi | Platform::Trae | Platform::WorkBuddy => {
            install_mcp_json(options)
        }
    }
}

// ── 四 mcp.json 平台(claude-code / kimi / trae / workbuddy) ────────────────

fn mcp_json_path(platform: Platform, env: &InstallEnv) -> PathBuf {
    let relative: &[&str] = match platform {
        Platform::ClaudeCode => &[".mcp.json"],
        Platform::Kimi => &[".kimi-code", "mcp.json"],
        Platform::Trae => &[".trae", "mcp.json"],
        Platform::WorkBuddy => &[".workbuddy", "mcp.json"],
        _ => unreachable!("mcp.json 平台才进这里"),
    };
    let mut path = env.cwd.to_path_buf();
    for part in relative {
        path = path.join(part);
    }
    path
}

/// install 要写入的 wanning 条目 = 生成器产物里的 `mcpServers.wanning`
/// (单一事实来源:install 不另写一份字段面)。
fn generated_entry(options: &InstallOptions) -> Result<Value, InstallError> {
    let artifact = artifact_for(options)?;
    let document: Value = serde_json::from_str(&artifact.content)
        .map_err(|error| InstallError::Generate(format!("生成产物不是合法 JSON: {error}")))?;
    Ok(document["mcpServers"]["wanning"].clone())
}

fn parse_mcp_document(text: &str) -> Result<Value, InstallError> {
    let document: Value = serde_json::from_str(text).map_err(|error| {
        InstallError::BadExisting(format!(
            "现有配置不是合法 JSON({error}),拒绝动它;修好或人工处理后再装"
        ))
    })?;
    if !document.is_object() {
        return Err(InstallError::BadExisting(format!(
            "现有配置顶层是{},不是 JSON 对象,拒绝动它",
            type_name(&document)
        )));
    }
    match document.get("mcpServers") {
        None | Some(Value::Object(_)) => Ok(document),
        Some(other) => Err(InstallError::BadExisting(format!(
            "现有配置的 mcpServers 是{},不是对象,拒绝动它",
            type_name(other)
        ))),
    }
}

struct McpPlan {
    unchanged: bool,
    diff: Vec<String>,
}

fn plan_mcp_merge(document: Option<&Value>, entry: &Value) -> McpPlan {
    let existing = document
        .and_then(|doc| doc.get("mcpServers"))
        .and_then(|servers| servers.get("wanning"));
    match existing {
        Some(current) if current == entry => McpPlan {
            unchanged: true,
            diff: Vec::new(),
        },
        Some(current) => McpPlan {
            unchanged: false,
            diff: entry_diff(current, entry),
        },
        None => McpPlan {
            unchanged: false,
            diff: Vec::new(),
        },
    }
}

/// 字段级 diff:两对象按键并集逐字段比对,变了的字段打 `- ` 旧行 / `+ ` 新行;
/// 任一侧不是对象(不该发生,防御性)则整值对比,保证 diff 永远点名改动。
fn entry_diff(old: &Value, new: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    match (old.as_object(), new.as_object()) {
        (Some(old_map), Some(new_map)) => {
            let keys: BTreeSet<&String> = old_map.keys().chain(new_map.keys()).collect();
            for key in keys {
                let old_value = old_map.get(key);
                let new_value = new_map.get(key);
                if old_value == new_value {
                    continue;
                }
                match (old_value, new_value) {
                    (Some(value), None) => lines.push(format!("- {key}: {}", render(value))),
                    (None, Some(value)) => lines.push(format!("+ {key}: {}", render(value))),
                    (Some(old_value), Some(new_value)) => {
                        lines.push(format!("- {key}: {}", render(old_value)));
                        lines.push(format!("+ {key}: {}", render(new_value)));
                    }
                    (None, None) => unreachable!("键来自两 map 的并集"),
                }
            }
        }
        _ => {
            lines.push(format!("- {}", render(old)));
            lines.push(format!("+ {}", render(new)));
        }
    }
    lines
}

fn render(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<不可序列化>".to_string())
}

fn install_mcp_json(options: &InstallOptions) -> Result<InstallReport, InstallError> {
    let path = mcp_json_path(options.platform, options.env);
    let entry = generated_entry(options)?;
    let existed = path.exists();
    let raw = if existed { read_optional(&path)? } else { None };
    let document = match &raw {
        Some(text) => Some(parse_mcp_document(text)?),
        None => None,
    };
    let plan = plan_mcp_merge(document.as_ref(), &entry);

    if options.dry_run {
        let mut actions = vec![format!("将写入 {}", path.display())];
        if plan.unchanged {
            actions.push("已是最新,--dry-run 也不会有任何改动".to_string());
        } else {
            if existed {
                actions.push(format!(
                    "将先备份原文件到 {}",
                    backup_path_for(&path).display()
                ));
            }
            if let Some(old) = document
                .as_ref()
                .and_then(|doc| doc.get("mcpServers"))
                .and_then(|servers| servers.get("wanning"))
            {
                for line in entry_diff(old, &entry) {
                    actions.push(format!("  {line}"));
                }
            }
        }
        return Ok(InstallReport {
            state: InstallState::DryRun,
            target: None,
            backup: None,
            diff: plan.diff,
            actions,
            printed: None,
        });
    }

    if plan.unchanged {
        return Ok(InstallReport {
            state: InstallState::AlreadyCurrent,
            target: Some(path.clone()),
            backup: None,
            diff: Vec::new(),
            actions: vec![format!("{} 已是最新,未改动", path.display())],
            printed: None,
        });
    }

    // merge:他人条目原样保留(mcpServers 里只换 wanning 一段),没有 mcpServers
    // 就建(serde_json 对 Null 的下标赋值会自动升级成对象)。
    let mut merged = document.unwrap_or_else(|| serde_json::json!({}));
    merged["mcpServers"]["wanning"] = entry;
    let mut content = serde_json::to_string_pretty(&merged)
        .map_err(|error| InstallError::Io(format!("序列化配置失败: {error}")))?;
    content.push('\n');

    // 先备份(原文件字节),后写入。
    let backup = if existed {
        let backup_path = backup_path_for(&path);
        fs::copy(&path, &backup_path)
            .map_err(|error| InstallError::Io(format!("备份 {} 失败: {error}", path.display())))?;
        Some(backup_path)
    } else {
        None
    };
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|error| {
                InstallError::Io(format!("创建 {} 失败: {error}", parent.display()))
            })?;
        }
    }
    fs::write(&path, content.as_bytes())
        .map_err(|error| InstallError::Io(format!("写入 {} 失败: {error}", path.display())))?;

    let mut actions = vec![format!(
        "{} → {}",
        path.display(),
        if existed {
            "更新 wanning 条目"
        } else {
            "全新创建"
        }
    )];
    if let Some(backup) = &backup {
        actions.push(format!("原文件已备份到 {}", backup.display()));
    }
    for line in &plan.diff {
        actions.push(format!("  {line}"));
    }
    Ok(InstallReport {
        state: if existed {
            InstallState::Updated
        } else {
            InstallState::Fresh
        },
        target: Some(path),
        backup,
        diff: plan.diff,
        actions,
        printed: None,
    })
}

// ── deepseek-harness(cordis.patch.yml 文本块级 merge) ──────────────────────

fn dsh_patch_path(env: &InstallEnv) -> Result<PathBuf, InstallError> {
    let Some(dsh_home) = env.dsh_home else {
        return Err(InstallError::TargetUnresolved(
            "deepseek-harness 的落点是 $DSH_HOME/cordis.patch.yml,但 DSH_HOME 未设置;\
             不猜落点,设好 DSH_HOME 后重试"
                .to_string(),
        ));
    };
    Ok(dsh_home.join("cordis.patch.yml"))
}

/// 生成产物里的 `- insert:` 块(头部注释行丢弃,块行原样)。
fn dsh_block_lines(options: &InstallOptions) -> Result<Vec<String>, InstallError> {
    let artifact = artifact_for(options)?;
    let block: Vec<String> = artifact
        .content
        .lines()
        .skip_while(|line| !line.starts_with("- "))
        .map(str::to_string)
        .collect();
    if block.is_empty() {
        return Err(InstallError::Generate(
            "生成产物里没有 `- insert:` 块".to_string(),
        ));
    }
    Ok(block)
}

/// 顶层块扫描:cordis.patch.yml 顶层是 insert 列表(`- ` 行起头,缩进行/空行/
/// `#` 注释行延续到下一个列 0 行);列 0 出现非 `- ` 非注释非空行 = 顶层不是
/// 列表,fail-closed。返回每个块的 [起,止) 行区间。
fn scan_patch_blocks(text: &str) -> Result<Vec<(usize, usize)>, InstallError> {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if line.starts_with("- ") {
            let start = index;
            index += 1;
            while index < lines.len() {
                let follow = lines[index];
                if follow.starts_with("- ") {
                    break;
                }
                if follow.trim().is_empty()
                    || follow.starts_with('#')
                    || follow.starts_with(' ')
                    || follow.starts_with('\t')
                {
                    index += 1;
                    continue;
                }
                return Err(InstallError::BadExisting(format!(
                    "cordis.patch.yml 顶层不是 insert 列表(第 {} 行 `{}`),拒绝动它",
                    index + 1,
                    follow
                )));
            }
            blocks.push((start, index));
        } else if line.trim().is_empty() || line.starts_with('#') {
            index += 1;
        } else {
            return Err(InstallError::BadExisting(format!(
                "cordis.patch.yml 顶层不是 insert 列表(第 {} 行 `{}`),拒绝动它",
                index + 1,
                line
            )));
        }
    }
    Ok(blocks)
}

fn install_dsh(options: &InstallOptions) -> Result<InstallReport, InstallError> {
    let path = dsh_patch_path(options.env)?;
    let block = dsh_block_lines(options)?;
    let existing = read_optional(&path)?;

    let Some(existing_text) = existing else {
        let content = block.join("\n") + "\n";
        if options.dry_run {
            let mut actions = vec![format!("将写入 {}", path.display())];
            for line in &block {
                actions.push(format!("  + {line}"));
            }
            return Ok(dry_run_report(actions));
        }
        fs::create_dir_all(options.env.dsh_home.expect("上面已判定存在"))
            .map_err(|error| InstallError::Io(format!("创建 {} 失败: {error}", path.display())))?;
        fs::write(&path, content.as_bytes())
            .map_err(|error| InstallError::Io(format!("写入 {} 失败: {error}", path.display())))?;
        return Ok(InstallReport {
            state: InstallState::Fresh,
            target: Some(path.clone()),
            backup: None,
            diff: Vec::new(),
            actions: vec![format!("{} → 全新创建(append 块写入)", path.display())],
            printed: None,
        });
    };

    let lines: Vec<&str> = existing_text.lines().collect();
    let blocks = scan_patch_blocks(&existing_text)?;
    let span = blocks.iter().copied().find(|&(start, end)| {
        lines[start..end]
            .iter()
            .any(|line| line.contains("id: wanning-gate"))
    });

    let (new_text, diff, state) = match span {
        None => {
            // 追加在文件尾:他人块逐字节保留(W-44 纪律,append 勿整文件覆盖)。
            let mut text = existing_text.clone();
            if !text.ends_with('\n') && !text.is_empty() {
                text.push('\n');
            }
            let mut diff = Vec::new();
            for line in &block {
                diff.push(format!("+ {line}"));
            }
            (text + &block.join("\n") + "\n", diff, InstallState::Updated)
        }
        Some((start, end))
            if lines[start..end]
                .iter()
                .copied()
                .eq(block.iter().map(String::as_str)) =>
        {
            (
                existing_text.clone(),
                Vec::new(),
                InstallState::AlreadyCurrent,
            )
        }
        Some((start, end)) => {
            // 替换发生在原 wanning 块位置,他人块不动。
            let mut diff = Vec::new();
            for line in &lines[start..end] {
                diff.push(format!("- {line}"));
            }
            for line in &block {
                diff.push(format!("+ {line}"));
            }
            let mut rebuilt: Vec<&str> = Vec::new();
            rebuilt.extend_from_slice(&lines[..start]);
            rebuilt.extend(block.iter().map(String::as_str));
            rebuilt.extend_from_slice(&lines[end..]);
            let mut text = rebuilt.join("\n");
            text.push('\n');
            (text, diff, InstallState::Updated)
        }
    };

    if state == InstallState::AlreadyCurrent {
        return Ok(InstallReport {
            state,
            target: Some(path.clone()),
            backup: None,
            diff: Vec::new(),
            actions: vec![format!("{} 已是最新,未改动", path.display())],
            printed: None,
        });
    }

    if options.dry_run {
        let mut actions = vec![format!("将写入 {}", path.display())];
        for line in &diff {
            actions.push(format!("  {line}"));
        }
        return Ok(dry_run_report(actions));
    }

    let backup_path = backup_path_for(&path);
    fs::copy(&path, &backup_path)
        .map_err(|error| InstallError::Io(format!("备份 {} 失败: {error}", path.display())))?;
    fs::write(&path, new_text.as_bytes())
        .map_err(|error| InstallError::Io(format!("写入 {} 失败: {error}", path.display())))?;
    let mut actions = vec![format!("{} → {}", path.display(), "合并 wanning 块")];
    actions.push(format!("原文件已备份到 {}", backup_path.display()));
    for line in &diff {
        actions.push(format!("  {line}"));
    }
    Ok(InstallReport {
        state: InstallState::Updated,
        target: Some(path),
        backup: Some(backup_path),
        diff,
        actions,
        printed: None,
    })
}

fn dry_run_report(actions: Vec<String>) -> InstallReport {
    InstallReport {
        state: InstallState::DryRun,
        target: None,
        backup: None,
        diff: Vec::new(),
        actions,
        printed: None,
    }
}

// ── openclaw / hermes(宿主 CLI;--yes 才执行) ───────────────────────────────

fn host_name(platform: Platform) -> &'static str {
    match platform {
        Platform::OpenClaw => "openclaw",
        Platform::Hermes => "hermes",
        _ => unreachable!("宿主 CLI 平台才进这里"),
    }
}

fn install_host(options: &InstallOptions) -> Result<InstallReport, InstallError> {
    let artifact = artifact_for(options)?;
    let printed = artifact.content.clone();
    let name = host_name(options.platform);

    if options.dry_run {
        let mut actions = vec![format!("将执行宿主 CLI:{}", printed.trim_end())];
        actions.push("dry-run 不执行,零副作用".to_string());
        return Ok(InstallReport {
            state: InstallState::DryRun,
            target: None,
            backup: None,
            diff: Vec::new(),
            actions,
            printed: Some(printed),
        });
    }

    if !options.yes {
        return Ok(InstallReport {
            state: InstallState::HostPrinted,
            target: None,
            backup: None,
            diff: Vec::new(),
            actions: vec![format!(
                "复制执行下面的命令即完成挂载(或加 --yes 让 wanning 代执行)"
            )],
            printed: Some(printed),
        });
    }

    let args = host_args(options, &printed)?;
    let program = resolve_host(name, options.host_bin, options.env.path_env)?;
    let (code, stdout, stderr) = exec_host(&program, &args, options.platform == Platform::Hermes)?;
    if code != 0 {
        return Err(InstallError::HostFailed(format!(
            "宿主 CLI {} 退出码 {code},安装失败(fail-closed,不回滚宿主配置);\
             stdout: {} stderr: {}",
            program.display(),
            stdout.trim(),
            stderr.trim()
        )));
    }
    Ok(InstallReport {
        state: InstallState::HostExecuted,
        target: None,
        backup: None,
        diff: Vec::new(),
        actions: vec![format!("已执行:{} {}", program.display(), args.join(" "))],
        printed: Some(printed),
    })
}

/// 宿主 CLI argv:openclaw 的 payload 从打印命令行里原样剥出(打印与执行同源);
/// hermes 的 argv 与生成器命令行同一组值构成。
fn host_args(options: &InstallOptions, printed: &str) -> Result<Vec<String>, InstallError> {
    match options.platform {
        Platform::OpenClaw => {
            let line = printed.trim_end();
            let payload = line
                .strip_prefix("openclaw mcp set wanning '")
                .and_then(|rest| rest.strip_suffix('\''))
                .ok_or_else(|| {
                    InstallError::Generate(
                        "openclaw 命令行形态不符合预期(单引号包裹 payload)".to_string(),
                    )
                })?;
            Ok(vec![
                "mcp".to_string(),
                "set".to_string(),
                "wanning".to_string(),
                payload.to_string(),
            ])
        }
        Platform::Hermes => Ok(vec![
            "mcp".to_string(),
            "add".to_string(),
            "wanning".to_string(),
            "--command".to_string(),
            slash(&options.resolved.mcp_bin),
            "--args".to_string(),
            "--wal".to_string(),
            slash(&options.resolved.wal),
            "--budget".to_string(),
            budget_arg(),
        ]),
        _ => unreachable!("宿主 CLI 平台才进这里"),
    }
}

/// 宿主 CLI 解析:显式 `--host-bin` 必须是真实文件;否则按 PATH 逐目录找
/// (Windows 补 .exe/.cmd/.bat 后缀)。找不到 = fail-closed。
fn resolve_host(
    name: &str,
    host_bin: Option<&Path>,
    path_env: Option<&OsStr>,
) -> Result<PathBuf, InstallError> {
    if let Some(explicit) = host_bin {
        if explicit.is_file() {
            return Ok(explicit.to_path_buf());
        }
        return Err(InstallError::HostNotFound(format!(
            "宿主 CLI {name} 在指定路径 {} 不存在(--host-bin 必须指向真实可执行文件)",
            explicit.display()
        )));
    }
    let Some(path_env) = path_env else {
        return Err(InstallError::HostNotFound(format!(
            "环境里没有 PATH,解析不到宿主 CLI {name};用 --host-bin 显式指定"
        )));
    };
    for dir in std::env::split_paths(path_env) {
        let mut candidates = vec![dir.join(name)];
        if cfg!(windows) {
            for ext in [".exe", ".cmd", ".bat"] {
                candidates.push(dir.join(format!("{name}{ext}")));
            }
        }
        for candidate in candidates {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(InstallError::HostNotFound(format!(
        "宿主 CLI {name} 不在 PATH 里;先安装它,或用 --host-bin 显式指定"
    )))
}

/// 执行宿主 CLI。hermes 非 TTY 下 `mcp add` 会问确认(W-45 实测),喂 `y\n` 后
/// **关闭写端**;openclaw 不喂 stdin。必须手动 spawn + take(stdin):`output()`
/// 会立即关掉管道写端,确认就读不到了。
fn exec_host(
    program: &Path,
    args: &[String],
    feed_yes: bool,
) -> Result<(i32, String, String), InstallError> {
    let mut command = Command::new(program);
    command.args(args);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command.stdin(if feed_yes {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = command
        .spawn()
        .map_err(|error| InstallError::HostNotFound(format!("宿主 CLI 无法启动({error})")))?;
    if feed_yes {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(b"y\n");
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|error| InstallError::HostFailed(format!("等待宿主 CLI 失败: {error}")))?;
    let code = output.status.code().unwrap_or(-1);
    Ok((
        code,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

// ── 读取面(doctor 复用) ────────────────────────────────────────────────────

/// 读一个平台已装的 wanning 条目;未装 = `Ok(None)`;配置形状坏 = 报错
/// (doctor 据此给修复指引,绝不静默当未装)。
pub fn read_installed_entry(
    platform: Platform,
    env: &InstallEnv,
) -> Result<Option<InstalledEntry>, InstallError> {
    match platform {
        Platform::ClaudeCode | Platform::Kimi | Platform::Trae | Platform::WorkBuddy => {
            let path = mcp_json_path(platform, env);
            let Some(text) = read_optional(&path)? else {
                return Ok(None);
            };
            let document = parse_mcp_document(&text)?;
            match document
                .get("mcpServers")
                .and_then(|servers| servers.get("wanning"))
            {
                Some(value) => entry_from_value(path, value),
                None => Ok(None),
            }
        }
        Platform::Codex => {
            let Some(home) = env.codex_home else {
                return Ok(None);
            };
            let path = home.join("config.toml");
            let Some(text) = read_optional(&path)? else {
                return Ok(None);
            };
            read_codex_fragment(path, &text)
        }
        Platform::OpenClaw => {
            let Some(dir) = env.openclaw_state_dir else {
                return Ok(None);
            };
            let path = dir.join("openclaw.json");
            let Some(text) = read_optional(&path)? else {
                return Ok(None);
            };
            let document: Value = serde_json::from_str(&text).map_err(|error| {
                InstallError::BadExisting(format!(
                    "{} 不是合法 JSON({error}),拒绝解读",
                    path.display()
                ))
            })?;
            match document.pointer("/mcp/servers/wanning") {
                Some(value) => entry_from_value(path, value),
                None => Ok(None),
            }
        }
        Platform::Hermes => {
            let Some(dir) = env.hermes_home else {
                return Ok(None);
            };
            let path = dir.join("config.yaml");
            let Some(text) = read_optional(&path)? else {
                return Ok(None);
            };
            read_hermes_config(path, &text)
        }
        Platform::DeepSeekHarness => {
            let path = dsh_patch_path(env)?;
            let Some(text) = read_optional(&path)? else {
                return Ok(None);
            };
            read_dsh_block(path, &text)
        }
    }
}

fn entry_from_value(path: PathBuf, value: &Value) -> Result<Option<InstalledEntry>, InstallError> {
    let Some(object) = value.as_object() else {
        return Err(InstallError::BadExisting(format!(
            "{} 里的 wanning 条目不是对象,拒绝解读",
            path.display()
        )));
    };
    let Some(command) = object.get("command").and_then(Value::as_str) else {
        return Err(InstallError::BadExisting(format!(
            "{} 里的 wanning 条目缺 command 字符串,拒绝解读",
            path.display()
        )));
    };
    let Some(args) = object.get("args").and_then(Value::as_array) else {
        return Err(InstallError::BadExisting(format!(
            "{} 里的 wanning 条目缺 args 数组,拒绝解读",
            path.display()
        )));
    };
    let mut parsed = Vec::new();
    for arg in args {
        let Some(text) = arg.as_str() else {
            return Err(InstallError::BadExisting(format!(
                "{} 里的 wanning 条目 args 含非字符串项,拒绝解读",
                path.display()
            )));
        };
        parsed.push(text.to_string());
    }
    Ok(Some(InstalledEntry {
        path,
        command: command.to_string(),
        args: parsed,
    }))
}

/// codex 的 config.toml 是 TOML 文本面:容忍式读 `[mcp_servers.wanning]` 段
/// (不引 toml 依赖;段边界 = 下一个 `[` 行)。
fn read_codex_fragment(path: PathBuf, text: &str) -> Result<Option<InstalledEntry>, InstallError> {
    let lines: Vec<&str> = text.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        if line.trim() != "[mcp_servers.wanning]" {
            continue;
        }
        let mut command: Option<String> = None;
        let mut args: Vec<String> = Vec::new();
        for follow in &lines[index + 1..] {
            if follow.trim_start().starts_with('[') {
                break;
            }
            let content = follow.trim();
            if content.is_empty() || content.starts_with('#') {
                continue;
            }
            if let Some(value) = toml_value_after_key(content, "command") {
                command = Some(unquote(value));
            } else if let Some(value) = toml_value_after_key(content, "args") {
                args = parse_flow_strings(value);
            }
        }
        return match command {
            Some(command) => Ok(Some(InstalledEntry {
                path,
                command,
                args,
            })),
            None => Err(InstallError::BadExisting(format!(
                "{} 的 [mcp_servers.wanning] 段缺 command,拒绝解读",
                path.display()
            ))),
        };
    }
    Ok(None)
}

/// `key = value` 取值(前缀匹配防误伤:键名必须紧随 `=`,`commands` 不会命中
/// `command`)。
fn toml_value_after_key<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.strip_prefix(key)?
        .trim_start()
        .strip_prefix('=')?
        .trim_start()
        .into()
}

/// hermes 的 config.yaml:`wanning:` 键的块(缩进更深的后续行,到空行或缩进
/// 回落为止)。
fn read_hermes_config(path: PathBuf, text: &str) -> Result<Option<InstalledEntry>, InstallError> {
    let lines: Vec<&str> = text.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        if line.trim() != "wanning:" {
            continue;
        }
        let key_indent = indent_of(line);
        let mut end = index + 1;
        while end < lines.len() {
            let follow = lines[end];
            if follow.trim().is_empty() || indent_of(follow) <= key_indent {
                break;
            }
            end += 1;
        }
        return read_yamlish_entry(path, &lines[index + 1..end]);
    }
    Ok(None)
}

/// dsh 的 cordis.patch.yml:找含 `id: wanning-gate` 的顶层块,块内读
/// command/args。
fn read_dsh_block(path: PathBuf, text: &str) -> Result<Option<InstalledEntry>, InstallError> {
    let blocks = scan_patch_blocks(text)?;
    let lines: Vec<&str> = text.lines().collect();
    let span = blocks.iter().copied().find(|&(start, end)| {
        lines[start..end]
            .iter()
            .any(|line| line.contains("id: wanning-gate"))
    });
    match span {
        Some((start, end)) => read_yamlish_entry(path, &lines[start..end]),
        None => Ok(None),
    }
}

/// YAML 形态的容忍式条目读取(hermes config.yaml 块 / dsh patch 块共用):
/// `command:` 标量 + `args:`(行内 flow 形态或块列表形态)。
fn read_yamlish_entry(
    path: PathBuf,
    lines: &[&str],
) -> Result<Option<InstalledEntry>, InstallError> {
    let mut command: Option<String> = None;
    let mut args: Vec<String> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let content = lines[index].trim();
        if let Some(value) = yaml_scalar_after_key(content, "command") {
            command = Some(unquote(value));
        } else if content == "args:" {
            // 块列表:后面缩进更深的 `- ` 项(hermes 落盘形态)。
            let base_indent = indent_of(lines[index]);
            let mut block = Vec::new();
            let mut scan = index + 1;
            while scan < lines.len() {
                let follow = lines[scan];
                if follow.trim().is_empty() || indent_of(follow) <= base_indent {
                    break;
                }
                match follow.trim().strip_prefix("- ") {
                    Some(item) => block.push(unquote(item)),
                    None => break,
                }
                scan += 1;
            }
            args = block;
            index = scan;
            continue;
        } else if let Some(value) = yaml_scalar_after_key(content, "args") {
            args = parse_flow_strings(value);
        }
        index += 1;
    }
    match command {
        Some(command) => Ok(Some(InstalledEntry {
            path,
            command,
            args,
        })),
        None => Ok(None),
    }
}

/// `key: value` 取值(hermes/dsh 的 YAML 标量;前缀匹配防误伤)。
fn yaml_scalar_after_key<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.strip_prefix(key)?
        .strip_prefix(':')?
        .trim_start()
        .into()
}

fn parse_flow_strings(text: &str) -> Vec<String> {
    let Some(inner) = text
        .trim()
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    else {
        return Vec::new();
    };
    inner
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(unquote)
        .collect()
}

/// 去掉成对的单/双引号(YAML/TOML 标量;不处理转义,闸配置的路径面没有引号字符)。
fn unquote(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        let first = bytes[0];
        let last = bytes[trimmed.len() - 1];
        if (first == b'\'' || first == b'"') && first == last {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "布尔值",
        Value::Number(_) => "数字",
        Value::String(_) => "字符串",
        Value::Array(_) => "数组",
        Value::Object(_) => "对象",
    }
}

/// 读文件;不存在 = `Ok(None)`(未装),其它错误上抛。
fn read_optional(path: &Path) -> Result<Option<String>, InstallError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(InstallError::Io(format!(
            "读 {} 失败: {error}",
            path.display()
        ))),
    }
}

/// 备份路径 = `<file>.wanning.bak`(与目标同目录)。
fn backup_path_for(path: &Path) -> PathBuf {
    let mut name = path.file_name().expect("安装落点必有文件名").to_os_string();
    name.push(".wanning.bak");
    path.with_file_name(name)
}

fn artifact_for(options: &InstallOptions) -> Result<crate::Artifact, InstallError> {
    generate_with(options.platform, options.resolved)
        .map_err(|error| InstallError::Generate(error.message()))
}
