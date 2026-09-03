# Kimi Code CLI 的 MCP 接入机制(W-40 调研 + 本机实测)

> 方法口径同 W-12/W-13/W-24/W-35:`[直核]` = 本会话直接抓取官方来源读到正文,或本机
> 真实命令实测;`[摘要]` = 官方域名的检索/转述摘要。**查不到明确写查不到,零编造。**
> 本调研 2026-09-02 完成,全程零网络消费路径、零真实下单、**零真实模型会话**(模型侧
> 一律本地 mock,真实 kimi 会话烧的是所有者 kimi 账号额度,红线 2 本来也禁)。

---

## 结论速读(≤20 行)

1. **本机装的是 kimi-code 0.39.1**(Moonshot AI,`kimi --version` [直核]),与 W-17
   调研的 kimi-cli 是**两代产品**:所有者机器上有迁移痕迹(`~/.kimi/` 旧数据根 +
   `~/.kimi-code/tui.migrated-from-kimi-cli.toml`;CLI 自带 `kimi migrate` 子命令)。
2. **现役挂法不是 `kimi mcp add`**:0.39.1 实测**无 `mcp` 子命令** [直核];官方文档
   直核挂法 = **`$KIMI_CODE_HOME/mcp.json`(用户级)或 `<repo>/.kimi-code/mcp.json`
   (项目级)**,TUI 内 `/mcp-config` 交互管理 [直核]。W-17 直核的 `kimi mcp add
   --transport stdio` 属 legacy kimi-cli 挂法,生成器 W-40 已修订。
3. **本机实测通过(0.39.1)**:隔离 `KIMI_CODE_HOME` 下,手写 mcp.json 挂
   wanning-mcp,**真 kimi 二进制拉起闸、注入两个工具、完成三次判定落 WAL**——
   放行 500 分(行 2)/ 同 nonce 重放拒(行 3)/ 超额拒(行 4),跨三会话同账、
   完整性链连续(§3)。**生成器输出的配置填占位符后同样真跑通**(§4)。
4. 与 W-35 codex 的差异:kimi-code **没有 `kimi mcp` 配置面子命令**,MCP 连接发生在
   会话启动时——本轮把模型侧换成**本地 mock LLM**(openai 协议指向 127.0.0.1),
   才把会话级 MCP 往返实测到真 kimi 二进制上;~~**真实模型会话仍待 kimi 账号登录**~~
   **真实模型会话复证待所有者放行烧额度**(RFC 8628 device-code 备用;W-42 修正
   2026-09-02:登录凭证 2026-08-30 已在档、早于本轮,见 §⑤ 阻塞清单)。
5. 接入门槛清单:用户级 mcp.json **不经 workspace trust 提示**;项目级在未信任目录
   会弹 trust(默认拒绝信任)[直核文档]。工具命名 `mcp__wanning__*`,权限可用
   `[[permission.rules]]` allow/deny 通配预先放行 [直核文档]。

---

## ① 两代 Kimi CLI:产品线与版本

- **kimi-cli(legacy)**:GitHub `MoonshotAI/kimi-cli`(W-17 直核,README 自述
  「evolving into Kimi Code CLI…本仓库将逐步收摊」)。所有者机器数据根 `~/.kimi/`
  (config.toml 2026-06-15)。
- **kimi-code(现役)**:官方文档站 <https://moonshotai.github.io/kimi-code/>(静态
  可抓,含 `llms-full.txt` 全量文档包)、GitHub `MoonshotAI/kimi-code`。本机实装
  **0.39.1**(`kimi --version` [直核]),数据根 `~/.kimi-code/`,bin 下附 fd/rg。
- **迁移证据** [直核,本机文件清单]:`tui.migrated-from-kimi-cli.toml` 存在;CLI
  子命令 `migrate` 自述「Migrate data from a legacy kimi-cli installation into
  kimi-code」。
- **子命令面(0.39.1 实测 `kimi --help`)**[直核]:`export / provider / acp / web /
  server(弃用) / login / doctor / vis / migrate / upgrade`。**没有 `mcp`**;
  `kimi mcp --help` 只回主 usage(unknown command 行为)。
- `kimi acp`:「Run kimi-code as an Agent Client Protocol (ACP) server over
  stdio」——kimi 作为 agent 被 ACP 客户端(如 Zed)驱动,是**被接入**的口,不是
  它消费 MCP 的口;与 Wanning 无关(我们不接 ACP)。

## ② 配置机制(mcp.json + config.toml)

**MCP server 声明** [直核:官方 MCP 页 /en/customization/mcp + W-40 实验现物]:

```json
{
  "mcpServers": {
    "wanning": {
      "command": "D:/path/to/wanning-mcp.exe",
      "args": ["--wal", "D:/path/to/demo.wal"]
    }
  }
}
```

- 两级:用户级 `~/.kimi-code/mcp.json`(`$KIMI_CODE_HOME/mcp.json`)+ 项目级
  `<repo>/.kimi-code/mcp.json`,**同名条目项目级覆盖用户级** [直核]。
- 传输三种:stdio(`command` 字段)/ HTTP(`url` 无 transport)/ SSE(显式
  `"transport": "sse"`)。Wanning 用 stdio。**stdio 由 command 字段隐含,无 `type`
  字段**(官方示例与 W-40 实验现物一致;与 Claude Code 现物的 `type: "stdio"` 是
  刻意差异)。
- 可选字段 [直核]:`env` / `cwd`(stdio)/ `headers` / `bearerTokenEnvVar`(HTTP、
  SSE)/ `enabled` / `startupTimeoutMs`(默认 30000)/ `toolTimeoutMs`(默认
  60000)/ `enabledTools` / `disabledTools`。
- 全局默认:config.toml `[mcp] startup_timeout_ms` / `[mcp] tool_timeout_ms`,env
  `KIMI_MCP_STARTUP_TIMEOUT_MS` / `KIMI_MCP_TOOL_TIMEOUT_MS`;优先级 per-server >
  env > config.toml > 内置默认 [直核]。
- **路径变量:官方文档未提及 `${...}` 扩展** [直核] → 绝对路径手改(生成器占位符
  `{{WANNING_BIN}}` / `{{WAL_PATH}}`,同 codex/kimi 既有纪律)。
- TUI:`/mcp-config`(增删改,可 `/mcp-config login <名>` 走 OAuth)、`/mcp`(看
  连接状态)[直核]。

**config.toml 相关段** [直核:官方 Configuration 页]:

| 段 | 要点 |
|---|---|
| `[providers.<名>]` | `type` = `kimi/anthropic/openai/openai_responses/google-genai/vertexai`;`api_key`/`base_url`;`env` 子表兜底;**凭证不回落 shell 环境变量**(只读 config 文件) |
| `[models."<别名>"]` | `provider`/`model`/`max_context_size` 必填;`capabilities` 加 `tool_use` 等 |
| `default_model` | 必须指向 models 表里的别名 |
| `default_permission_mode` | `manual`(默认)/`yolo`/`auto` |
| `[[permission.rules]]` | `decision = "allow"/"deny"` + `pattern`,按序首条生效;MCP 工具名 `mcp__<server>__<tool>`,支持 `*`/`**` 通配 |
| `[mcp]` | `startup_timeout_ms`/`tool_timeout_ms` 全局默认(见上) |
| `telemetry` | 匿名遥测开关,`false` 显式关闭 |

**doctor**:校验 `config.toml`/`tui.toml`,缺省文件报 SKIP(built-in defaults);
**不校验 mcp.json**。`KIMI_CODE_HOME` 官方支持数据根迁移(隔离实验的合法入口)。

**非交互 `-p`** [直核]:`--prompt` 单发;不能与 `--yolo/--auto/--plan` 同用,
**非交互默认 auto 权限**;auth/模型未配置时报「No model configured. Run `kimi` and
use /login to sign in, then retry; or set default_model in config.toml」(实测原文)。

## ③ 本机离线实测(0.39.1,零登录零真实模型)

实测方法:**隔离 `KIMI_CODE_HOME`**(仓内 `target/w40/kimihome`),所有者真实
`~/.kimi-code/config.toml` 全程未动(3151 字节前后一致,无 mcp.json)。**模型侧 =
本地 mock LLM**(python,127.0.0.1:18777,openai 协议 SSE;零外网零消费)。取 Kent:

1. 空 home `kimi doctor` → `SKIP config.toml` / `SKIP tui.toml`(「File does not
   exist; built-in defaults will apply」)exit 0——隔离生效 [直核]。
2. 不配模型直接 `kimi -p` → 「No model configured…」即退,**WAL 零物证**:模型检查
   先于 MCP 连接,auth 门挡在会话外(与 codex doctor 的 auth 缺失同构)。
3. 配置:用户级 mcp.json 挂 wanning-mcp(绝对路径)+ config.toml provider mock
   (openai 协议,127.0.0.1)+ `default_model` + `default_permission_mode = "auto"`
   + `[[permission.rules]] allow mcp__wanning__*` + `telemetry = false`。
4. **第一轮教训**:mock 回普通 JSON(kimi 请求带 `stream: true` + `stream_options`)
   → kimi 解析失败反复重试 8 轮至 90s timeout;日志尾见 `mcp server unavailable …
   closed unexpectedly` 是 timeout 杀进程的**伴生现象**不是根因(W-19「配置类排障
   先抓字节」同款)。mock 改回 **SSE chunk 流**后秒级收敛。
5. **三轮判定**(同一 WAL 跨三个 `kimi -p` 会话)[直核,WAL 原文 §6]:
   - 会话 1:`wanning_gate_evaluate` 500 分 → **allow**(行 2,budget_after 500);
   - 会话 2:同 nonce 1 重放 → **deny replay**(行 3,预算保持 500——被拒不耗号);
   - 会话 3:nonce 2、10000 分 → **deny over_budget**(行 4)。
   行 1 注册 demo-d1(cap 1000 分)由 wanning-mcp 启动自动写入;第二/三次启动
   「已注册则跳过」(W-17 语义),完整性链 seq/prev 连续。
6. **工具注入物证**:mock 收到的请求 `tools` 数组里真实出现
   `mcp__wanning__wanning_gate_evaluate` 与 `mcp__wanning__wanning_audit_tail`
   (与官方 `mcp__<server>__<tool>` 命名一致),与全部内置工具并列。
7. 审计页导出(`--export-audit`):4 行账,完整性链逐行验证,链尾
   `0x19659a9b8edeabf8`,回放两遍 hash `0xb69e94bae00879e6`,放行 1 笔/拒绝 2 笔。

## ④ 生成器联动(W-40 修订)

- W-17 口径的 kimi 生成物(`kimi mcp add --transport stdio …` 命令行)在 0.39.1
  上**跑不通**(无该子命令)→ 生成器 kimi 分支改为生成 **`.kimi-code/mcp.json`
  内容**(mcpServers/command/args,无 type,绝对路径占位符),契约测试换血
  (`kimi_output_matches_w40_experiment`,先红后绿,红相落 `target/w40-red.txt`)。
- **生成物实测**:`cargo run -p wanning-init -- --platform kimi` 输出填占位符 →
  写入隔离 mcp.json → 真 kimi 二进制往返 allow(WAL 2 行:注册 + allow 500 分)。

## ⑤ 阻塞清单(待所有者)

| 项 | 卡在哪 | 解锁后做什么 |
|---|---|---|
| 真实模型会话的 MCP 往返 | ~~kimi 账号登录~~ **登录凭证已在档**(W-42 直核:`~/.kimi-code/credentials/kimi-code.json` 2026-08-30 06:03 与 user-history 1,055,057 字节同刻在档,均早于本轮;当时「待登录」出自隔离空 home 的「No model configured … use /login」报错,只对隔离 home 成立)——卡点收窄为**所有者放行烧 kimi 额度**(红线 2);凭证是否仍有效由真实会话证明,失效再走 device-code(mainland-cn/global 二选一) | 跑真模型会话复证本轮 mock 实测的三判定与工具注入 |
| 项目级 mcp.json 的 trust 提示 | 交互 UI(headless 不可见;文档直核默认拒绝信任) | 在真实项目里信任一次,验证项目级挂法 |
| `kimi upgrade` | 未测(升级可能写 C 盘 `~/.kimi-code/updates`,铁律 4 须所有者拍板) | 版本升级后重测本轮契约 |

## 查不到清单

- kimi-code **MCP 支持起点版本号**(哪个 0.x 首次含 MCP)——release notes 逐版
  考证未做,待人工;两代迁移的版本分界同样未考证。
- 真实 kimi 模型会话下的 MCP 工具调用实测(§⑤ 第一条;W-42 修正:登录凭证
  2026-08-30 已在档,卡点=所有者放行烧额度,非登录)。
- 项目级 trust 提示的实际 UI 形态与白名单机制(文档只给行为描述)。
- `provider catalog`(models.dev 导入)是否含免费/本地网关——未展开(与本任务无关,
  且涉真实模型调用)。

## 来源

- [直核] 本机实测:`kimi --version` / `kimi --help` / `kimi mcp --help` / `kimi
  doctor`(真实 HOME 只读 + 隔离 HOME)/ `kimi -p` 三轮实验 / `kimi provider add
  --help` / `kimi login --help` / `kimi acp --help`,0.39.1,2026-09-02(取证输出在档)
- [直核] 官方 MCP 文档(挂法/字段/权限/trust):https://moonshotai.github.io/kimi-code/en/customization/mcp.html
- [直核] 官方 Configuration 文档(config.toml 全 schema):https://moonshotai.github.io/kimi-code/llms-full.txt
  (config-files 节,2026-09-02 抓取)
- [直核] 官方文档站导航:https://moonshotai.github.io/kimi-code/(GitHub Pages 静态;
  `llms.txt` / `llms-full.txt` 为官方 LLM 优化文档包)
- [摘要] legacy 产品线:https://github.com/MoonshotAI/kimi-cli(W-17 直核 README,
  本轮未重抓)
