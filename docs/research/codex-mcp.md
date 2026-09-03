# Codex CLI 的 MCP 接入机制(W-35 调研)

> 方法口径同 W-12/W-13/W-24:`[直核]` = 本会话直接抓取官方来源读到正文,或本机真实
> 命令实测;`[摘要]` = 官方域名的检索/转述摘要。**查不到明确写查不到,零编造。**
> 本调研 2026-09-02 完成,全部零网络消费路径、零真实下单;codex 会话未登录,**一次
> 模型会话都没跑**(红线 2:会话跑起来烧的是 OpenAI 额度)。

---

## 结论速读(≤20 行)

1. **Codex CLI 是 OpenAI 的编码 agent CLI**,MCP 支持完整:stdio + streamable HTTP
   两种传输,配置全部落在 `~/.codex/config.toml` 的 `[mcp_servers.<id>]` 表。
2. **本机实装实测通过(0.133.0)**:`codex mcp add/list/get/remove/login/logout` 六个
   子命令**全部无需 OpenAI 登录**即可操作配置面;用隔离 `CODEX_HOME` 实测 `mcp add`
   真写出 TOML、真起 wanning-mcp、真走通 MCP 握手与工具调用落账(见 §4)。
3. **会话级工具注入被 auth 挡住**:`codex exec`/TUI 的 MCP 连接发生在会话内,
   `codex doctor` 实证「no Codex credentials were found」→ **待 OpenAI 账号**(且
   会话烧 OpenAI 额度,红线 2 本来也禁)。
4. **Wanning 侧结论:codex 是「钥匙即插」状态**——配置面免登录已实测,配置生成的
   TOML 已实证可直接启动 wanning-mcp 并落账;唯一缺口 = 所有者的 OpenAI 登录。
5. 对 W-36 生成器的直接影响:codex **没有 `.mcp.json` 式的仓根文件**(project-scoped
   `.codex/config.toml` 存在但 trust 机制待实测,`codex mcp add` 实测只写全局
   home),生成器应输出 **config.toml 片段 / `codex mcp add` 命令行**。

---

## ① Codex CLI 是什么、版本与 MCP 支持起点

- **安装形态**:`npm install -g @openai/codex`(官方 README [直核]);本机实装
  `@openai/codex@0.133.0`(npm 全局,`codex --version` → `codex-cli 0.133.0` [直核])。
  官方 README 另列 macOS/Linux 安装脚本、Windows PowerShell 安装脚本、`brew install
  --cask codex`,安装器默认从 `https://releases.openai.com/codex` 下载、回退 GitHub
  Releases。[直核]
- **MCP 支持起点(官方 repo 历史)**[直核,来源 GitHub commits 搜索 API]:
  - 2025-05-02 `83961e0` **feat: introduce mcp-types crate (#787)** —— repo 历史中
    最早的 MCP commit(共 1043 个含 mcp 的 commit);
  - 2025-05-06 `147a940` **feat: support mcp_servers in config.toml (#829)** ——
    config.toml 的 `mcp_servers` 支持落点;
  - 同日 `88e7ca5`(#836)TUI 展示 MCP 工具调用、`7d8b38b`(#841)exec 同样支持。
- **查不到**:MCP 支持起点对应的**发布版本号**(哪个 tag 首次含 MCP)——官方
  CHANGELOG.md 只有一行「见 releases 页」,releases API 本轮分页抓取被 403 限流,
  逐版考证未做,待人工。commit 日期为实测可靠事实。

## ② 配置机制(config.toml)

**结构** [直核:本地 0.133.0 实测写出的字节 + config-reference 文档]:

```toml
# ~/.codex/config.toml(全局)或 <project>/.codex/config.toml(project-scoped,
# 官方文档注明仅 trusted projects;trust 如何建立待实测)
[mcp_servers.wanning]
command = 'D:\...\wanning-mcp.exe'   # stdio 必填
args = ["--wal", 'D:\...\demo.wal']
# env = { KEY = "value" }             # 环境变量(文档字段,本轮未实测)
# cwd = 'D:\...'                      # 工作目录(文档字段)
```

**字段清单**(config-reference [直核];本地写出验证了 command/args 两个):

| 字段 | 作用 | 默认 |
|---|---|---|
| `command` / `args` / `env` / `cwd` | stdio 启动四件 | `command` 必填 |
| `url` / `bearer_token_env_var` / `auth` | streamable HTTP;`auth` = `oauth`(默认)\|`chatgpt` | `oauth` |
| `startup_timeout_sec`(别名 `_ms`)| server 启动超时 | **10s** |
| `tool_timeout_sec` | 每工具调用超时 | **60s** |
| `enabled` | 停用不移除配置 | true |
| **`required`** | **true = server 无法初始化时 fail 启动/resume** | false |
| `enabled_tools` / `disabled_tools` | 工具白名单/黑名单(黑名单后生效) | — |
| `default_tools_approval_mode` / `tools.<t>.approval_mode` | `auto\|prompt\|writes\|approve` | — |

**与 Wanning 语义的契合点**:`required = true` 与闸的 fail-closed 同构——server 起
不来就拒绝启动会话,不静默降级为「无闸裸奔」(文档字段,待 OpenAI 登录后实测)。
`disabled_tools` 可把审计读面关掉,但**工具面本就只有两个**(闸评估+审计读),无
需过滤;**撤销/锚点等所有者侧动作在 MCP 面不存在**(W-15/W-17 既定设计),不靠
codex 的过滤兜底。

**CLI 命令面** [直核:0.133.0 `codex mcp --help` / `codex mcp add --help`]:
`codex mcp add <NAME> (--url <URL> | -- <COMMAND>...)` + `--env KEY=VALUE` +
`--bearer-token-env-var`;`list` / `get` / `remove` / `login` / `logout`。
`codex mcp list` 表头 `Name/Command/Args/Env/Cwd/Status/Auth`——**Status 是配置级
(enabled),不是连通性探测**(实测列表/doctor 均不发起握手)。
`--oauth-client-id`、`--oauth-client-registration` 等 OAuth 旗标出现在官方文档
(当前版),**本地 0.133.0 的 add help 未列出**——文档描述的是更新版本行为,版本
差如实标注。

## ③ 本机离线实测(0.133.0,零登录零消费)

实测方法:**隔离 `CODEX_HOME`**(指到仓内 `target\w35-codex-home`),所有者真实
`~/.codex/config.toml` 全程未动(前后 2526 字节一致)。`CODEX_HOME` 隔离被 codex
官方支持:`codex doctor` 直接打印生效的 `CODEX_HOME` 路径;config-reference 亦引用
`$CODEX_HOME` 约定。[直核]

1. 空 home 下 `codex mcp list` → 「No MCP servers configured yet」,证明隔离生效。
2. `codex mcp add wanning -- <wanning-mcp.exe> --wal <wal>` → 「Added global MCP
   server 'wanning'」;隔离 home 里真写出 `config.toml`(§② 的 TOML 即原文)。
3. `codex mcp get wanning` → 回执 enabled/transport: stdio/command/args/env/remove。
4. `codex doctor` → `✗ auth: no Codex credentials were found`(会话级不可达的实据);
   `✓ mcp: 1 server (1 stdio) · 0 disabled`(配置级计数);顺带实证 **npm 发行版带
   原生 win32-x64 二进制**(与 repo 文档 install.md「Windows 11 via WSL2」不一致
   ——repo 文档只写源码构建路径,滞后于 npm 发行形态,如实记)。
5. **握手取证**(`target/w35-codex-home/handshake_probe.py`,tomllib 解析 codex 写出
   的 config,原样 command+args spawn):initialize(2025-06-18)→ serverInfo
   `wanning-mcp 0.1.0`;`notifications/initialized` 通知零响应;`tools/list` 恰 2 个
   工具;`wanning_gate_evaluate` 500 分 → **allow,budget_after=500,wal_line=2**(行1
   注册委托);同 nonce 重试 → **deny replay**;`wanning_audit_tail` 读回含
   budget_after;退出码 0;WAL 落盘 3 行,`seq/prev/rec` 完整性链齐全。→ **codex
   写出的配置命令,不加任何修改即可启动闸并真实落账**。
6. doctor 报 0.152.1 可升级,**不升**:npm 全局升级会写入 C 盘
   (`C:\Users\<用户名>\AppData\Roaming\npm`),违反铁律 4。

## ④ 阻塞清单(待所有者/待登录)

| 项 | 卡在哪 | 解锁后做什么 |
|---|---|---|
| 会话级 MCP 工具注入 | OpenAI 登录(doctor ✗ auth);且会话烧 OpenAI 额度(红线 2,须所有者亲自) | 实测 codex 会话内真调 wanning_gate_evaluate;验证 `required: true`、`startup_timeout_sec` 行为 |
| project-scoped `.codex/config.toml` | trust 机制文档提了但没给操作,查不到细节 | 实测仓内 `.codex/config.toml` 是否被采纳(trusted projects) |
| MCP 起点对应版本号 | releases 逐版考证未做(403 限流) | 人工翻 releases 页补一句 |

## 来源

- [直核] 本机实测:`codex --version` / `codex mcp --help` / `codex mcp add --help` /
  `codex mcp list` / `codex mcp get` / `codex doctor`,0.133.0,2026-09-02
  (取证输出在档)
- [直核] Codex 官方 MCP 文档(CLI 面):https://learn.chatgpt.com/docs/extend/mcp?surface=cli
  (由 https://developers.openai.com/codex/mcp 308 重定向)
- [直核] Codex 配置参考(mcp_servers 全字段):https://learn.chatgpt.com/docs/config-file/config-reference
  (由 https://developers.openai.com/codex/config-reference 308 重定向)
- [直核] MCP 支持起点(commit 搜索,83961e0/147a940 等):
  https://api.github.com/search/commits?q=repo:openai/codex+mcp&sort=committer-date&order=asc
- [直核] 安装命令(官方 README):https://raw.githubusercontent.com/openai/codex/main/README.md
- [摘要] repo docs/install.md(仅源码构建 + WSL2 标注,与 npm 发行形态不一致):
  https://raw.githubusercontent.com/openai/codex/main/docs/install.md
- [摘要] repo docs/config.md 已改为指引页(真身迁 learn.chatgpt.com):
  https://raw.githubusercontent.com/openai/codex/main/docs/config.md
