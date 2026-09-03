# DeepSeek Harness (dsh) × Wanning 插件页

> W-44 落地。字段权威 = **本机已装的 dsh 0.1.0-rc.7 包内官方 README**
> （`@deepseek-ai/dsh-mcp-client` 字段表/工具命名/env 剥离契约，与安装版本逐字一致）
> + 真二进制 `--dump-config --patch` 组合实测。标注口径：
> **[本机直核]** = 本轮在本机文件/真二进制上验证；**[任务书转核]** = W-44 任务书
> （在档）调研直核的官方文档信息，本轮零网络未复核。
> 状态：**配置面 + patch 组合已真二进制实测通过（隔离 DSH_HOME，零网络零会话）；
> 会话级端到端待所有者放行（dsh 会话 = 模型会话 + 网络，红线 2）**。
> 本机实测基线：dsh **0.1.0-rc.7**（npm 全局 `@deepseek-ai/dsh`，`dsh --version` 实测）。

## 一键直写与体检(W-51)

- `wanning init --platform deepseek-harness --install`:把本页的 `- insert:` 块
  合并追加进 `$DSH_HOME/cordis.patch.yml`——顶层块级扫描,他人块逐字节保留
  (W-44 纪律:append 勿整文件覆盖),wanning 块替换发生在原位置,写前备份
  `<file>.wanning.bak`,`--dry-run` 零落盘预览;`DSH_HOME` 未设 = 不猜落点拒装。
- `wanning doctor --platform deepseek-harness`:装完体检六项(二进制/配置语义/
  真握手/账本目录可写/真实消费就绪度/版本一致性),从 cordis.patch.yml 读
  `id: wanning-gate` 块;每项 ❌ 带 ✗ 修复命令;真握手用隔离临时账本,零模型
  零外网零真实消费。

## dsh 是什么

DeepSeek 官方开源 agent harness（TypeScript，MIT，developer preview，
[官方明示会有破坏性变更] [任务书转核]）。架构 = Cordis「everything is a plugin」：
profile 是有序的 plugin-bundle patch 层叠，`dsh` 命令是各 profile 的启动器。
MCP 支持来自官方桥接插件 `@deepseek-ai/dsh-mcp-client`：把外部 MCP server 的工具
注册为原生工具，命名 `mcp__<serverName>__<rawName>`——**与 Claude Code / Codex
同形**（包内 README 原话）[本机直核]。

## 安装（所有者已经装了）

本机已有 dsh 0.1.0-rc.7（npm 全局），无需任何安装动作：

```powershell
dsh --version    # → 0.1.0-rc.7
```

## 接入机制：不是 mcp.json,是 Cordis overlay patch

与其他五平台（mcp.json / config.toml）**根本不同**：dsh 用 YAML patch 层声明插件。
patch 层应用顺序（dsh 包内 README）[本机直核]：

1. 各 bundle 的 patch（按 `dsh.profile.bundles` 顺序）
2. profile 的 `cordis.patch.yml`,再 home 级 `$DSH_HOME/cordis.patch.yml`
3. 最后 `--patch` 命令行 overlay

**启用二选一**：

- 临时：`dsh --profile <名> --patch wanning.cordis.yml`
- 持久：把 insert 块**合并追加**进 `<profile>/cordis.patch.yml` 或
  `$DSH_HOME/cordis.patch.yml`（**合并追加,绝不整文件覆盖**——文件里可能有别的
  patch entry；与 wanning-init「绝不覆盖」纪律同构）

## 配置现物(生成器输出,本轮实跑)

```powershell
cargo run -p wanning-init -- --platform deepseek-harness
# --out <路径>.cordis.yml 显式落盘(已存在绝不覆盖)
```

生成内容（`--out target/w44/wanning.cordis.yml` 实跑原样）：

```yaml
# Wanning 支付闸 — DeepSeek Harness (dsh) Cordis overlay patch(W-44 生成)
# 启用二选一:
#   临时:dsh --profile <名> --patch <本文件>
#   持久:把下面 insert 块合并追加进 <profile>/cordis.patch.yml 或
#         $DSH_HOME/cordis.patch.yml(合并追加,绝不整文件覆盖)
- insert:
    - id: wanning-gate                 # 唯一 id
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: wanning            # 工具将现身为 mcp__wanning__wanning_gate_evaluate
        transport: stdio
        command: cargo                 # 或发布后的二进制路径
        args: ["run", "--quiet", "-p", "wanning-mcp", "--", "--wal", "{{WAL_PATH}}"]
        env: {}
        cwd: !!js process.cwd()
```

- `{{WAL_PATH}}` 手改成审计 WAL 绝对路径（dsh 文档未提及 `${...}` 路径变量）。
- `cwd: !!js process.cwd()` 是官方 js-tag 用法,按官方示例原样生成 [本机直核：
  包内 mcp-client README 官方示例同款 js-tag]。
- `command: cargo` 形态假定 dsh 从 Wanning 仓内启动（`cargo run -p` 需要工作区）;
  dsh 在其他项目启动时把 command 换成 `wanning-mcp` 可执行文件绝对路径 +
  `args: ["--wal", "<绝对路径>"]`——该变体同样过了本轮 dump-config 门禁。

## 真实二进制取证（本轮,零网络零会话）

隔离 `DSH_HOME`（仓内 `target/w44/dshhome`,所有者真实 `~/.dsh` 零触碰）：

```text
dsh --profile headless --dump-config --patch target/w44/wanning.cordis.yml
→ exit 0;组合树末尾出现独立 patch 层:
  # == D:\...\target\w44\wanning.cordis.yml
  - id: wanning-gate
    name: '@deepseek-ai/dsh-mcp-client'
    config: {serverName: wanning, transport: stdio, command: cargo, args: [...], env: {}, cwd: !!js process.cwd()}
```

`- insert:` 被真解析器**消费**（组合树里是拆出的插件行,没有 `insert` 键残留）——
不是被当普通行放过。红相对照：坏 YAML patch → exit 1,
`failed to parse overlay ... YAMLException: missed comma between flow collection entries`。

**诚实边界（本门禁测到哪一层）**：`--dump-config` 只证明 YAML 语法 + patch entry
形态被 loader 接受；**不证明 `@deepseek-ai/dsh-mcp-client` 包能在 boot 时解析**
（组合阶段不解析插件包,对照实验：`--patch` 一个不存在的插件名照常 exit 0）。
boot/会话级行为见阻塞清单。取证输出：`target/w44/gate-patched.out`（过程记录在
档,W-44 节）。

## 字段契约（包内 @deepseek-ai/dsh-mcp-client README 直核）

| 字段 | 必填 | 说明 |
|---|---|---|
| `transport` | 是 | `"stdio"` 或 `"streamable-http"` |
| `serverName` | 是 | 工具名命名空间;`[A-Za-z0-9_-]{1,32}`,跨活跃实例唯一（`wanning` 7 字符合规） |
| `command` | stdio 是 | 可执行文件 |
| `args` | stdio 否 | 传给 command 的参数 |
| `env` | stdio 否 | **在 scrub 后的 ambient env 之上合并**（见下） |
| `cwd` | stdio 否 | 子进程工作目录 |

可选加固：`config.failOnStartupError: true`——**默认 `false` 意味着闸起不来时
插件仍激活但零工具**（闸位形同虚设）；置 true 则 dsh 拒绝激活该插件,与 Wanning
fail-closed 同构。重连行为（`reconnect.*`）官方默认指数退避 10 次,详细见包内 README。

## ⚠️ 环境变量剥离（官方行为）

`scrubbedParentEnv()` / `SENSITIVE_ENV_PATTERN` 是唯一共享 scrub 定义
（dsh-subprocess 包内 README 原话）[本机直核]：**ambient credential-shaped 与全部
`DSH_*` 环境变量名被丢弃,显式 `env` 在 scrub 之后才合并**。含义：

- 通道密钥类 env（将来接真实通道时）**必须写进本 row 的 `config.env`**,不能赌
  环境继承——官方文档同样建议如此 [任务书转核]。
- Wanning 当前零密钥即可工作（闸只管授权判定,不碰支付通道）,此行为对现状无影响；
  记在档是为将来接真实通道时不踩坑。

## 工具面(dsh 会话内可用)

| 工具 | 现身名 | 作用 | 权限语义 |
|---|---|---|---|
| 闸评估 | `mcp__wanning__wanning_gate_evaluate` | 提交消费意图,闸判定(allow/deny + reason) | 判定与拒绝**都落审计**(WAL) |
| 审计尾 | `mcp__wanning__wanning_audit_tail` | 读审计尾 | 只读 |
| 待支付查询 | `mcp__wanning__wanning_pending_status` | 只读查询支付形态与待支付单状态(W-53) | 只读,零写入零网络 |

**人在环为默认旅程(W-53)**:默认档位 `pending_pay` 下,闸评估放行即开待支付单
(单号 `p-…`,带审批额与 15 分钟 TTL,确认前零资金流);AI 侧止步于提交意图与
待支付查询(只读)——**确认不在工具面上**(AI 不能确认 AI 自己的支付,工具面连
confirm 字样都不出现,契约测试钉死)。人付完款在终端跑
`wanning confirm <单号> --amount <同额元> --proof <交易号>` 把支付凭证入账
(金额一致 / 幂等 / TTL 三钉 fail-closed,被拒的确认一行都不落账)。

- **没有撤销工具、没有授权工具**(agent 能撤销就能复活,能授权就能自授权)——
  授权/撤销走所有者侧(白皮书 §4)。
- 同一 WAL 多平台并发：与其他平台指向同一份 WAL 时,第二个写进程 fail-closed
  拒启(W-18 单写者锁)——两本账不会悄悄分叉。

## 阻塞清单

| 项 | 状态 |
|---|---|
| 会话级端到端(`dsh web --patch` 真跑闸) | 待所有者放行——dsh 会话 = 模型会话 + 网络,红线 2(烧额度动作一律所有者亲自);本机 dsh 已装,唯一缺的是「放行烧额度」这一句话 |
| `--patch` 的实际启动行为(权限预设/审批流交互) | 待会话级实测 |
| developer preview 破坏性变更 | 官方明示;升级 dsh 后建议先重跑本轮 `--dump-config --patch` 门禁再上会话 |
| `env` 剥离的字段级验证 | 行为已从包内 README 直核;逐字段实测待会话级(观察子进程 env) |

## 来源

- [本机直核] `dsh --version` / `dsh --help`（0.1.0-rc.7,2026-09-02 实测）
- [本机直核] `@deepseek-ai/dsh` npm 全局包内：
  `node_modules/@deepseek-ai/dsh-mcp-client/README.md`（字段表/工具命名/行为）、
  `node_modules/@deepseek-ai/dsh-subprocess/README.md`（scrubbedParentEnv）、
  顶层 `README.md`（profile/patch 层级）
- [本机直核] 真二进制 `--dump-config --patch` 组合实验（隔离 DSH_HOME,红绿对照）
- [本机直核] `~/.dsh/profiles/headless/cordis.patch.yml` 头注释（dsh 自生成说明：
  「top-level YAML array of loader patch entries … and insert lists; `!!js`
  expressions allowed」;所有者文件只读未动）
- [任务书转核] 官方文档站 `https://deepseek-harness.github.io/deepseek-harness/`
  （含 `docs/user/guide/mcp-memory.md` 通用 patch 格式）——来源 = W-44 任务书
  （在档）调研直核,本轮零网络未复核
