# Hermes Agent (Nous Research) × Wanning 插件页

> W-45 落地。字段权威 = **本机 hermes-agent v0.19.1 隔离实测**
> （`hermes mcp` 子命令族真跑 + 全链路 agent 回合,零外网零真实消费）+
> 包内 `cli-config.yaml.example` 直核。标注口径:**[本机直核]** = 本轮在本机
> 文件/真二进制上验证。
> 状态:**全链路已实测通过**——挂载(2/2 工具发现)→ one-shot agent 回合
> (`-z`)+ 本地 mock LLM → 闸判定落 WAL(allow 400 / 同 nonce replay 拒,
> 完整性链连续)。真实模型会话待所有者放行(红线 2)。
> 本机实测基线:hermes **v0.19.1**(2026-07-30)。

## Hermes 是什么

Nous Research 开源 agent harness(Python)。**原生支持 MCP**:`hermes mcp`
子命令族(add / list / test / remove)直接管理 MCP server 注册,落
`$HERMES_HOME/config.yaml` 的 `mcp_servers.<name>` 段 [本机直核]。

## 接入机制:discovery-first,挂载即验证

```powershell
wanning-init --platform hermes
# → 打印一条 `hermes mcp add wanning --command <bin> --args <args>` 命令
```

`hermes mcp add` 是 **discovery-first**:执行时真连一次 wanning-mcp 发现
工具,发现失败 = 挂载失败。挂载即验证,不存在「配置写了但连不上」的中间态
[本机直核]。

```text
$ hermes mcp add wanning --command <bin> --args --wal <wal> --budget 1000
  Connecting to 'wanning'...
  ✓ Connected! Found 2 tool(s) from 'wanning':
    wanning_gate_evaluate                    把一笔消费意图交给闸判定(…)
    wanning_audit_tail                       读取审计 WAL 的最后若干行(…)
  Enable all 2 tools? [Y/n/select]:
  ✓ Saved 'wanning' to <HERMES_HOME>/config.yaml (2/2 tools enabled)
  Start a new session to use these tools.
```

非 TTY(脚本/CI)直接跑会卡在 `Enable all 2 tools? [Y/n/select]`——用管道
喂确认:`echo y | hermes mcp add ...` [本机直核]。

## 配置落点(隔离实测原文)

隔离 `HERMES_HOME`(指向仓内 `target/w45/hermes-home`,真实 `~/.hermes` 零触碰)
下 add 后,config.yaml 中出现的 wanning 段 [本机直核]:

```yaml
mcp_servers:
  wanning:
    command: D:/Desktop_Projects/Wanning/target/debug/wanning-mcp.exe
    args:
      - --wal
      - <绝对路径>
      - --budget
      - '1000'
    enabled: true
```

- `enabled: true` 由 add 的交互确认写入;`hermes mcp list` / `mcp test wanning`
  管理,`mcp remove wanning` 移除。
- 路径正斜杠(同 W-19 教训)。

## 真实二进制取证(本轮,零外网零真实消费)

隔离 `HERMES_HOME` + 模型侧指向本地 mock LLM(127.0.0.1:18791,
OpenAI-compatible,`model.base_url` 写进隔离 config.yaml):

1. `hermes mcp test wanning` → 真连成功(两轮实测 141ms / 172ms),2 工具
   [本机直核];
2. one-shot agent 回合:`hermes -z "<消费请求>" -t wanning` + mock LLM →
   闸判定落 WAL:第 2 行 `allow 400 分`(`budget_after_cents: 400`) [本机直核];
3. 二次会话同 nonce → 第 3 行 `deny reason=replay`,链值连续
   (`prev: 4933425087409498385` 接第 2 行链值) [本机直核];
4. mock LLM 请求日志物证:16 条请求里 `mcp__wanning__wanning_gate_evaluate`
   出现 14 次、`mcp__wanning__wanning_audit_tail` 8 次——两个工具都以
   `mcp__wanning__` 前缀进了模型可见目录 [本机直核]。

## ⚠️ 两个实测教训(其他平台没有的坑)

1. **MCP 工具在 deferred catalog,经 `tool_call` 间接调用**:模型侧看到的
   工具目录里有 `mcp__wanning__*`,但直接以该名发起 tool call 会报
   `does not exist`——必须经 hermes 的 `tool_call(name, arguments)` 间接层
   [本机直核]。
2. **one-shot 要显式带 toolset**:`hermes -z "..."` 默认 cli 工具集**不含
   MCP 工具**(mock 请求日志可证:默认 20 个内置工具,零 wanning);加
   `-t wanning`(以 server 名为工具集名)后 MCP 工具才进目录 [本机直核]。

## 工具面(hermes 会话内可用)

| 工具 | 现身名(模型目录) | 作用 | 权限语义 |
|---|---|---|---|
| 闸评估 | `mcp__wanning__wanning_gate_evaluate` | 提交消费意图,闸判定 | 判定与拒绝**都落审计**(WAL) |
| 审计尾 | `mcp__wanning__wanning_audit_tail` | 读审计尾 | 只读 |

调用形态:`tool_call(name="mcp__wanning__wanning_gate_evaluate",
arguments={...})`。没有撤销工具、没有授权工具——授权/撤销走所有者侧
(白皮书 §4)。同一 WAL 多平台并发:第二个写进程 fail-closed 拒启
(W-18 单写者锁)。

## 阻塞清单

| 项 | 状态 |
|---|---|
| 真实模型会话(烧 Nous/kimi 等额度) | 待所有者放行(红线 2);本轮模型侧 = 本地 mock,判定语义已由真实闸 + 真实 WAL 证明,mock 只替代「模型说哪句话」 |
| 交互 TUI 会话(非 `-z` one-shot) | 待所有者实测(需 TTY;本轮 headless 无 TTY) |
| tool_call 间接层在交互模式的模型遵循度 | 待真实模型会话(mock 是我们自己发的 tool_call,真模型是否稳定走间接层未证) |

## 来源

- [本机直核] `hermes --version` → v0.19.1(2026-07-30);`hermes mcp --help`
  子命令族(2026-09-03 实测)
- [本机直核] 隔离 `HERMES_HOME` 下 `mcp add` / `mcp list` / `mcp test` 真跑 +
  config.yaml 落盘原文
- [本机直核] one-shot agent 回合全链路(隔离 WAL 3 行原文 + mock 请求日志,
  取证在档,W-45 节)
- [本机直核] 包内 `cli-config.yaml.example`(mcp_servers 段字段面)
