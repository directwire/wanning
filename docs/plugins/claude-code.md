# Claude Code × Wanning 插件页

> W-41 落地(证据全取自 W-19 真插实测,2026-09-02)。取证全文:
> W-19 取证在档(探路轮 + 根因修正两节)。
> 状态:**真插实测通过——五平台矩阵里唯一全绿的一格**(其余四格见
> `codex.md`/`kimi.md`/`trae.md`/`workbuddy.md` 各自的阻塞清单)。
> 本机实测基线:Claude Code **2.1.234**(Windows 11,仓库根裸 `claude -p`)。

## 安装(老板已经装了)

本机已有 Claude Code 2.1.234,无需任何安装动作。验证:

```powershell
claude --version
```

## 配置现物(零手改,仓库原样即插)

现物在仓库根 `.mcp.json`(已入仓,克隆即用):

```json
{
  "mcpServers": {
    "wanning": {
      "type": "stdio",
      "command": "cargo",
      "args": [
        "run",
        "--quiet",
        "-p",
        "wanning-mcp",
        "--",
        "--wal",
        "${CLAUDE_PROJECT_DIR:-.}/target/mcp-demo.wal"
      ]
    }
  }
}
```

- `${CLAUDE_PROJECT_DIR:-.}` 展开为项目根,带 `.` 兜底;WAL 落
  `target/mcp-demo.wal`(gitignore,不入库)。
- `cargo run` 形态免预构建;想固定二进制可先 `cargo build -p wanning-mcp`,
  把 `command`/`args` 换成绝对路径写法(与 codex/kimi 页同款)。
- **挂载即生效**:无头 `claude -p` 按仓库 `.mcp.json` 自动连接——init 事件回
  `"mcp_servers":[{"name":"wanning","status":"connected"}]`,无批准、无信任、
  `~/.claude.json` 零条目(W-19 实证)。
- `claude mcp list` 可能显示 `⏸ Pending approval`——实测它只是**交互会话的批准
  簿记,不拦 `-p`**(list 显示 Pending approval 的同一时刻,无头会话已
  `status=connected`)。W-19 探路轮曾把它误判为拦路门,被字节级抓包证伪。

## 已实测证据(W-19)

三步证据链(headless 嵌套 Claude Code 会话,续写 demo WAL 旧账):

```text
① wanning_gate_evaluate nonce=45, 80分
   {"budget_after_cents":980,"decision":"allow","state_hash":"17a3c3e71b1ac539","wal_line":13}
② 同 nonce 重放探针
   {"decision":"deny","reason":"replay","state_hash":"17a3c3e71b1ac539","wal_line":14}
③ wanning_audit_tail lines=5 —— 尾部与磁盘 WAL 逐行一致
```

- 重启语义在真客户端下复证:server 重启**没洗白** nonce(② 拒)、**没复活**
  授权、预算从旧账 900 续算(900+80=980,不是新 cap 1000)——`live_resuming`
  (W-17)被第三方客户端复证。独立复核(探路会话,HEAD 上)再取一行:放行
  10 分 980→990(WAL 行 15)。
- **协议版本协商**:claude 2.1.234 提议 `2025-11-25`,server 按 spec
  「Version Negotiation」规范条文回自己支持的最高版(`2025-06-18`)——协商不是
  报错;W-15 曾在此回 `-32602` 导致 claude 不重试直接判 failed,W-19 修复
  (先红后绿)。教训:**配置类排障先字节级抓包,再谈门与权限**;自写客户端发的
  版本恰与 server 一致时,会反过来掩盖真因。

## 工具面(claude 会话内可用)

| 工具 | 作用 | 权限语义 |
|---|---|---|
| `wanning_gate_evaluate` | 提交消费意图,闸判定(allow/deny + reason) | 判定与拒绝**都落审计**(WAL) |
| `wanning_audit_tail` | 读审计尾 | 只读 |

- **没有撤销工具、没有授权工具**(agent 能撤销就能复活,能授权就能自授权)——
  授权/撤销走老板侧(白皮书 §4)。
- **同一 WAL 多平台并发**:`.mcp.json` 与 `.trae/mcp.json`、kimi/codex/workbuddy
  的配置指向同一份 WAL 时,第二个写进程 fail-closed 拒启(W-18 单写者锁)——
  这是特性:两本账不会悄悄分叉。
- 四条语义(预算内放行/超额拒/重放拒/审计对账)即四卖点,同一把闸(证据见
  README 四卖点真实输出)。

## 阻塞清单

| 项 | 状态 |
|---|---|
| 端到端复测 | 机制已实证;**复测烧 Claude 模型额度**(W-19 两轮合计 ≈$2.38,记账在档),重跑由老板拍板 |
| claude 升级后的行为差异 | 基线 2.1.234;新版本 `.mcp.json` 审批/挂载行为如有变化,待老板环境重测 |
| workspace trust 簿记 | 非阻塞:`mcp list` 的 Pending approval 只是交互簿记,不拦 `-p`(W-19 证伪记录在案) |
