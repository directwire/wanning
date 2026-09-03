# OpenClaw × Wanning 插件页

> W-45 落地,W-47 补完 agent 回合。字段权威 = **本机 OpenClaw 2026.5.22
> (a374c3a) 隔离实测**（`openclaw mcp` 子命令族真跑落盘原文 + `openclaw agent
> --local` 真回合）+ 官方文档站 `docs.openclaw.ai/mcp` stdio 字段面直核。
> 标注口径：**[本机直核]** = 本轮在本机文件/真二进制上验证；
> **[文档直核]** = 本轮直接抓取的官方文档页。
> 状态：**全链路已真宿主实测通过**（隔离 `OPENCLAW_STATE_DIR`/`OPENCLAW_CONFIG_PATH`/
> `OPENCLAW_WORKSPACE_DIR`,模型侧本地 mock LLM,零外网零真实消费）——
> 配置面（W-45）+ agent 回合工具现身与判定落 WAL（W-47）。
> 本机实测基线：OpenClaw **2026.5.22**（commit a374c3a）。

## 一键直写与体检(W-51)

- `wanning init --platform openclaw --install`:产出并执行本页的
  `openclaw mcp set wanning '<payload>'`——payload 从生成器命令行原样剥出
  (打印与执行同源);**缺省只打印命令行**,加 `--yes` 才代执行(执行前解析
  宿主真实路径,`--host-bin` 显式指定优先,解析不到或退出码非 0 = fail-closed)。
- `wanning doctor --platform openclaw`:装完体检六项(二进制/配置语义/真握手/
  账本目录可写/真实消费就绪度/版本一致性),从 `$OPENCLAW_STATE_DIR/openclaw.json`
  读 `mcp.servers.wanning`;每项 ❌ 带 ✗ 修复命令;真握手用隔离临时账本,零模型
  零外网零真实消费。

## OpenClaw 是什么

开源个人 agent 框架（gateway + 多渠道消息接入 + 本地 agent 回合）。**原生支持
MCP**：`openclaw mcp` 子命令族（list / show / set / unset / serve）直接管理
MCP server 注册，落 `$OPENCLAW_STATE_DIR/openclaw.json` 的 `mcp.servers.<name>`
段 [本机直核]。

## 接入机制：宿主 CLI 写入,生成器只出一条命令

openclaw.json 由宿主自己管理——实测落盘文件里除 `mcp.servers` 外还有
`commands` / `messages` / `agents` / `meta` 等骨架段 [本机直核]。所以
wanning-init 对 openclaw **不生成配置文件**,只生成一条 `openclaw mcp set`
命令行:宿主 CLI 自己合并写入,只动 `mcp.servers.wanning` 一段,
「绝不覆盖别人配置」的纪律由宿主 CLI 天然满足。

```powershell
wanning-init --platform openclaw
# → 打印一条 `openclaw mcp set wanning '{...}'` 命令,复制执行即完成注册
```

## 配置落点(隔离实测原文)

隔离 env(`OPENCLAW_STATE_DIR` + `OPENCLAW_CONFIG_PATH` 指向仓内
`target/w45/openclaw-home`,所有者真实 `~/.openclaw` 零触碰)下执行 set 后,
openclaw.json 中出现的 wanning 段 [本机直核]:

```json
"mcp": {
  "servers": {
    "wanning": {
      "command": "D:/Desktop_Projects/Wanning/target/debug/wanning-mcp.exe",
      "args": ["--wal", "<绝对路径>", "--budget", "1000"]
    }
  }
}
```

- 字段面 `{command, args}`,与 Claude Code 的 `mcpServers` 同形,**无 `type`
  字段**(stdio 由 command 隐含)。
- 路径正斜杠:JSON 里 Windows 反斜杠要转义,正斜杠 Windows 也认(W-19 教训)。

## stdio 字段契约(docs.openclaw.ai/mcp 直核)

| 字段 | 说明 |
|---|---|
| `command` | 可执行文件 |
| `args` | 传给 command 的参数 |
| `env` | 额外环境变量;**有安全过滤** |
| `cwd` | 子进程工作目录 |

**env 安全过滤** [文档直核]:OpenClaw 启动 MCP 子进程时拦截
`NODE_OPTIONS` / `PYTHONSTARTUP` / `DYLD_*` / `LD_*` 等(防宿主环境经 env
注入子进程)。含义:将来接真实通道时,通道密钥必须显式写进配置的 `env`,
不能赌环境继承(与 dsh 的 `scrubbedParentEnv` 同构,W-44)。

## 隔离实测取证(W-45 配置面)

- `openclaw mcp set wanning '<json>'` → exit 0;`openclaw mcp list` /
  `openclaw mcp show wanning` 回读与写入一致;落盘原文见上。
- **模型侧**:隔离 env 下用 `openclaw config set models.providers.wanningmock`
  挂本地 mock LLM(`http://127.0.0.1:18791/v1`,OpenAI-compatible,零外网)
  → exit 0,`openclaw models list` 显示
  `wanningmock/wanning-mock-model (text, 195k, local, auth-yes)` [本机直核]。

## W-47 · agent 回合端到端(隔离实测通过)

`openclaw agent --local` 的「静默退出」根因查明——**不是宿主闸坏了,是三层
调用姿势叠出来的假象** [本机直核]:

1. **缺 `-m/--message` → exit 1,报错只在 stderr**(stdout 零字节)。只看
   stdout 就是「静默退出」;真实报错
   `Missing required option "-m, --message <text>".`。
2. **还须选会话**:带 `-m` 不带会话选择器 → `Error: Pass --to <E.164>,
   --session-key, --session-id, or --agent to choose a session`(同样只在
   stderr)。one-shot 用 `--agent main`。
3. **cwd 扫描税**:cwd 在大目录(仓根含 `target/`)时 CLI 启动多花
   16–17 秒(实测后台计时 06:12:46→06:13:02);冷文件系统缓存下更久,
   按常见 20–30 s 超时会话判成「挂死」。cwd 挪到轻目录即秒级。
   `--log-level` 等全局 flag 必须放子命令**前面**(`openclaw --log-level
   warn agent …`),放后面报 unrecognized option。

**能跑的命令**(全部隔离 env;`OPENCLAW_WORKSPACE_DIR` 必须显式给——不给时
workspace bootstrap 落 `~/.openclaw/workspace`,W-47 首跑实测读到的是所有者
既有文件,零写入但隔离失败):

```bash
OPENCLAW_STATE_DIR=<隔离home> OPENCLAW_CONFIG_PATH=<隔离home>/openclaw.json \
OPENCLAW_WORKSPACE_DIR=<隔离ws> \
openclaw agent --local --agent main -m "<消费请求>" --json
```

**回合实测(真实输出)**:

- 第一轮(mock 用 hermes 式 `tool_call` 间接层发工具调用):openclaw 暴露的
  工具名不认识 `tool_call`,模型自述 "I can't use the tool \"tool_call\"
  here because it isn't available",宿主重试 11 次后放弃——WAL 只有注册行。
  物证存档 `target/w47/mock-requests-attempt1.jsonl`。
- **工具现身名(修正后)**:MCP 工具直接进模型 `tools` 数组,现身名
  `<serverName>__<rawName>`——`wanning__wanning_gate_evaluate` /
  `wanning__wanning_audit_tail` [本机直核,物证在 mock 请求日志 tools 数组]。
  **无 `mcp__` 前缀**(与 Claude Code/Codex/dsh/hermes 的命名不同形!)。
- 第二轮(mock 直调真名):工具真被调用,闸判定真落账——
  `wanning_gate_evaluate` 返回「闸放行:金额 400 分,判后累计消费 400 分
  (审计 WAL 行 Some(2),state_hash fb0f7f70dda90dd5)」,WAL 行2 =
  allow 400/nonce=1/budget_after=400。
- 第三轮(同一 session 续用——openclaw 会话键 `agent:main:main` 跨 CLI 调用
  持久,sessionId 不变):同 nonce 再判 → **deny replay**,WAL 行3,
  budget_after 原样 400(拒绝不动账本、不耗 nonce)。
- `wanning audit` 读回:**完整性链逐行验证通过,回放对账两遍 hash 一致**,
  链尾 `0xc3222210ed01c555`,demo-d1 已花 400/1000 分。

**W-47 实测教训(别踩第二遍)**:

- **工具调用形状按宿主定制**:openclaw 直调真名,hermes 要过 `tool_call`
  间接层(deferred catalog)——同一份 mock LLM 换宿主就要换调用形状。
  物证方法 = 看 mock 请求日志里的 `tools` 数组(W-40 同款)。
- **mock 状态机看「最后一条消息」而非 `any(role==tool)`**:openclaw 续用
  session 时历史里留有旧工具消息,`any()` 口径会让第二轮再也不调工具,
  replay 轮造不出来。
- workspace bootstrap 会往 `OPENCLAW_WORKSPACE_DIR` 写默认文件(AGENTS.md
  等)——隔离目录必须预建为空目录,别指到任何真实数据旁。

## 工具面(实测回填)

| 工具 | 现身名(实测) | 作用 | 权限语义 |
|---|---|---|---|
| 闸评估 | `wanning__wanning_gate_evaluate` | 提交消费意图,闸判定 | 判定与拒绝**都落审计**(WAL) |
| 审计尾 | `wanning__wanning_audit_tail` | 读审计尾 | 只读 |
| 待支付查询 | `wanning__wanning_pending_status` | 只读查询支付形态与待支付单状态(W-53) | 只读,零写入零网络 |

**人在环为默认旅程(W-53)**:默认档位 `pending_pay` 下,闸评估放行即开待支付单
(单号 `p-…`,带审批额与 15 分钟 TTL,确认前零资金流);AI 侧止步于提交意图与
待支付查询(只读)——**确认不在工具面上**(AI 不能确认 AI 自己的支付,工具面连
confirm 字样都不出现,契约测试钉死)。人付完款在终端跑
`wanning confirm <单号> --amount <同额元> --proof <交易号>` 把支付凭证入账
(金额一致 / 幂等 / TTL 三钉 fail-closed,被拒的确认一行都不落账)。

没有撤销工具、没有授权工具(agent 能撤销就能复活,能授权就能自授权)——
授权/撤销走所有者侧(白皮书 §4)。同一 WAL 多平台并发:第二个写进程
fail-closed 拒启(W-18 单写者锁)。

## 阻塞清单

| 项 | 状态 |
|---|---|
| ~~agent 回合端到端(工具现身 + 判定落 WAL)~~ | **已实测通过(W-47,隔离 mock)**;真实模型会话复证属所有者放行项(剩真实模型会话半边,红线 2) |
| ~~隔离 env 下 agent 静默退出的根因~~ | **已查明(W-47)**:缺 `-m`(报错只在 stderr)+ 缺会话选择器 + cwd 扫描税 16–17s;详见上文 W-47 节 |
| 宿主真实家目录(所有者 `~/.openclaw/openclaw.json`) | W-45 只读直核时发现该文件**当前是坏的**(persona-migration 残留插件路径,宿主报 Invalid JSON)——属所有者配置,分身不修;所有者跑 `openclaw mcp set` 前需先处理 |
| env 安全过滤的逐键实测 | 字段面已 [文档直核];逐键行为待真实模型会话(观察子进程 env) |
| 真实模型会话(烧额度) | 待所有者放行(红线 2);mock 已证明的语义 = 工具现身/判定落 WAL/replay 拒/链连续 |

## 来源

- [本机直核] `openclaw --version` → 2026.5.22 (a374c3a);`openclaw mcp --help`
  子命令族(2026-09-03 实测)
- [本机直核] 隔离 env 下 `openclaw mcp set/list/show` 真跑 + openclaw.json
  落盘原文(取证在档,W-45 节)
- [本机直核] 隔离 env 下 `openclaw config set models.providers.*` + 
  `openclaw models list`(mock provider 注册,exit 0)
- [本机直核] W-47 隔离 `openclaw agent --local` 真回合 ×3(工具现身名/
  allow/replay 落 WAL/链连续;取证 `target/w47/`,W-47 节)
- [文档直核] `docs.openclaw.ai/mcp`(stdio 字段面 command/args/env/cwd +
  env 安全过滤;2026-09-03 抓取)
