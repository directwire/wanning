# Trae × Wanning 插件页

> W-41 落地(接入机制 W-17 直核官方 docs)。调研全文:
> `docs/research/mcp-consumption.md`(来源逐条标注)。
> 状态:**配置现物已备 + 生成器已支持;真插实测被 GUI 挡——本机未装 Trae**
> (2026-09-02 实测 `%LOCALAPPDATA%\Programs` 下无 Trae 目录)。所有者装好后按
> 本页「待实测清单」逐项收口,预计 15 分钟(待实测清单)。

## 安装(所有者还没装)

- 本机未装 Trae;官方渠道下载桌面端(装哪台机/哪个盘由所有者定,
  **注意铁律 4:不装 C 盘**)。
- 装好后逐项收口「待实测清单」;闸侧零改动(机制与 Claude Code 同为 MCP
  stdio,闸已真插验证过)。

## 配置现物(仓库已备,零手改)

现物在仓库 `.trae/mcp.json`(已入仓):

```json
{
  "mcpServers": {
    "wanning": {
      "command": "cargo",
      "args": [
        "run",
        "--quiet",
        "-p",
        "wanning-mcp",
        "--",
        "--wal",
        "${workspaceFolder}/target/mcp-demo.wal"
      ]
    }
  }
}
```

- `${workspaceFolder}` 是 Trae 的 workspace 路径变量(官方 docs 直核,W-17),
  语义同 Claude Code 的 `${CLAUDE_PROJECT_DIR:-.}`。
- **与 Claude Code 的 `.mcp.json` 指向同一份 WAL 是刻意设计**:两平台并发双挂时,
  单写者锁保证第二个写进程 fail-closed 拒启(W-18)——预算硬上限不可能被合力
  突破,这是特性不是缺陷。
- `cargo run` 形态免预构建;要固定二进制可先 `cargo build -p wanning-mcp`,
  把 `command`/`args` 换成绝对路径写法(与 codex/kimi 页同款)。

## 生成器(同款输出)

```bash
cargo run -p wanning-init -- --platform trae
# stdout 打印说明(notes)+ .trae/mcp.json 内容
```

2026-09-02 实跑输出(W-41 取证):

```json
{
  "mcpServers": {
    "wanning": {
      "args": [
        "run",
        "--quiet",
        "-p",
        "wanning-mcp",
        "--",
        "--wal",
        "${workspaceFolder}/target/mcp-demo.wal"
      ],
      "command": "cargo"
    }
  }
}
```

- 键序与现物不同、语义全等——字段级契约测试锁定在 `wanning-init` 仓内测试
  (W-36 起);严格 JSON 无注释语法,说明走 stdout notes(输出首行)。

## 工具面(trae 会话内可用)

| 工具 | 作用 | 权限语义 |
|---|---|---|
| `wanning_gate_evaluate` | 提交消费意图,闸判定(allow/deny + reason) | 判定与拒绝**都落审计**(WAL) |
| `wanning_audit_tail` | 读审计尾 | 只读 |

- **没有撤销工具、没有授权工具**(agent 能撤销就能复活,能授权就能自授权)——
  授权/撤销走所有者侧(白皮书 §4)。
- 四条语义(预算内放行/超额拒/重放拒/审计对账)与 Claude Code 实测(W-19)
  一致:同一把闸,trae 若真插即第五个消费端验证。

## 待实测清单(GUI 装好后逐项收口)

| 项 | 现状 |
|---|---|
| GUI 打开仓库后 `.trae/mcp.json` 自动挂载 | 待实测(机制同 Claude Code,闸零改动) |
| `${workspaceFolder}` 实际展开 | 官方 docs 直核(W-17),展开结果待 GUI |
| 与 `.mcp.json` 并发双挂 → 单写者锁拒启 | W-18 有两进程测试实证,GUI 侧复证待做 |
| 工具调用审批流(GUI 提示形态) | 待实测 |
