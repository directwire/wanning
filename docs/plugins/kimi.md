# Kimi Code CLI × Wanning 插件页

> W-40 落地。调研全文(来源/实测细节):`docs/research/kimi-code-cli.md`。
> 状态:**MCP 挂法 + 工具注入 + 闸往返已在本机实测通过(隔离 KIMI_CODE_HOME,
> 模型侧本地 mock);真实模型会话复证待所有者放行烧额度**~~待 kimi 账号登录~~
> (W-42 修正 2026-09-02:登录凭证 `~/.kimi-code/credentials/kimi-code.json` 与
> user-history(1,055,057 字节)均 **2026-08-30 06:03** 在档、`default_model`
> 08-30 02:26 已配,均早于 W-40——「待登录」出自隔离空 home 的报错,只对隔离
> home 成立;烧 kimi 额度的动作仍一律所有者亲自,凭证是否仍有效由真实会话证明)。
> 本机实测基线:kimi-code **0.39.1**(`~/.kimi-code/bin/kimi.exe`)。

## 一键直写与体检(W-51)

- `wanning init --platform kimi --install`:把本页配置直写进项目根
  `.kimi-code/mcp.json`——merge 只动 `mcpServers.wanning` 条目,他人条目不动,
  写前备份 `<file>.wanning.bak`,升级打字段级 diff,`--dry-run` 零落盘预览。
- `wanning doctor --platform kimi`:装完体检六项(二进制/配置语义/真握手/账本
  目录可写/真实消费就绪度/版本一致性),每项 ❌ 带 ✗ 修复命令;真握手用隔离临时
  账本,零模型零外网零真实消费。

## 安装(所有者已经装了)

本机已有 kimi-code 0.39.1,无需任何安装动作。验证:

```powershell
kimi --version    # → 0.39.1
```

注意:`kimi upgrade` 未测(可能写 C 盘用户目录,违反铁律 4 的风险须所有者拍板后自测)。

## 配置现物(生成器 + 手改占位符)

**推荐路径**(W-40 实测同款):

```powershell
cargo run -p wanning-init -- --platform kimi
# stdout 打印:说明(notes)+ .kimi-code/mcp.json 内容
```

把 JSON 部分存为 **`~/.kimi-code/mcp.json`**(用户级,所有项目生效;或
`$KIMI_CODE_HOME/mcp.json`)或 **`<repo>/.kimi-code/mcp.json`**(项目级,仅该仓),
再把两个占位符手改成绝对路径(官方文档无 `${...}` 路径变量):

```json
{
  "mcpServers": {
    "wanning": {
      "command": "D:/path/to/Wanning/target/debug/wanning-mcp.exe",
      "args": ["--wal", "D:/path/to/Wanning/target/kimi-demo.wal"]
    }
  }
}
```

- 先 `cargo build -p wanning-mcp` 出二进制;JSON 里用正斜杠(Windows spawn 认)。
- **项目级注意**:未信任目录里 kimi 会弹 workspace trust 提示并**默认拒绝信任**
  ——核对提示里列出的命令/参数后再确认;**用户级挂法不经 trust 提示**(想免交互
  一步到位就用用户级)。
- TUI 内交互管理:`/mcp-config`(增删改)、`/mcp`(看连接状态)。
- 会话需能跑模型(见阻塞清单);auto 权限模式(`-p` 默认)下 MCP 工具调用自动
  放行;manual 模式可用 `[[permission.rules]]`(config.toml)预先放行:

  ```toml
  [[permission.rules]]
  decision = "allow"
  pattern = "mcp__wanning__*"
  ```

**已实测证据(W-40)**:上述形态被真 kimi 0.39.1 二进制接受——wanning-mcp 被拉起
(WAL 行 1 注册 demo-d1),两个工具 `mcp__wanning__wanning_gate_evaluate` /
`wanning_audit_tail` 注入模型工具面,三轮判定落 WAL:allow 500 分(行 2)→ 同
nonce 重放拒(行 3)→ 超额拒(行 4),跨三会话同账、完整性链连续;生成器输出填
占位符后同样真跑通。取证输出在档。

## 工具面(kimi 会话内可用)

| 工具 | 作用 | 权限语义 |
|---|---|---|
| `wanning_gate_evaluate` | 提交消费意图,闸判定(allow/deny + reason) | 判定与拒绝**都落审计**(WAL) |
| `wanning_audit_tail` | 读审计尾 | 只读 |
| `wanning_pending_status` | 只读查询支付形态与待支付单状态(W-53) | 只读,零写入零网络 |

**人在环为默认旅程(W-53)**:默认档位 `pending_pay` 下,`wanning_gate_evaluate`
放行即开待支付单(单号 `p-…`,带审批额与 15 分钟 TTL,确认前零资金流);AI 侧止步于
提交意图与 `wanning_pending_status` 只读查询——**确认不在工具面上**(AI 不能确认
AI 自己的支付,工具面连 confirm 字样都不出现,契约测试钉死)。人付完款在终端跑
`wanning confirm <单号> --amount <同额元> --proof <交易号>` 把支付凭证入账
(金额一致 / 幂等 / TTL 三钉 fail-closed,被拒的确认一行都不落账)。

- **没有撤销工具、没有授权工具**(agent 能撤销就能复活,能授权就能自授权)——
  授权/撤销走所有者侧(白皮书 §4)。
- **同一 WAL 多平台并发**:kimi 的 mcp.json 与 `.mcp.json`(Claude Code)、
  `.trae/mcp.json`、codex config.toml 指向同一份 WAL 时,第二个写进程 fail-closed
  拒启(W-18 单写者锁)——这是特性:两本账不会悄悄分叉。
- 四条语义(预算内放行/超额拒/重放拒/审计对账)与 Claude Code 实测(W-19)一致:
  同一把闸,kimi 是第四个验证过的消费端(Claude Code 真插、codex 配置面、kimi
  挂法+往返、W-15 stdio 契约)。

## 阻塞清单

| 项 | 状态 |
|---|---|
| 真实模型会话内端到端 | ~~待 kimi 账号登录~~ **登录凭证已在档**(W-42 直核:`credentials/kimi-code.json` 2026-08-30 06:03,早于 W-40;`kimi login` 大概率可省)——剩所有者放行烧 kimi 额度的复证;凭证失效会话时报错再走 device-code |
| 项目级 mcp.json 的 trust 交互 | 文档直核(默认拒绝信任),UI 实际形态待真实环境 |
| `kimi upgrade` | 未测(铁律 4 风险,所有者拍板);升级后建议重测本轮三判定 |
