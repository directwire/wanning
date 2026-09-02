# 多平台接入一页(W-33)

> 目标:**5 分钟内**找到任一平台的现成接入答案。每条都是可直接复制的现物;
> 调研出处见 `docs/research/mcp-consumption.md`(W-17,官方来源逐条标注)。
> 前置:本仓已 `cargo build`(stdio server 是 `wanning-mcp`,`--wal` 必填 fail-closed)。

## Claude Code(已真插实测通过,W-19)

现物在仓库根 `.mcp.json`:

```json
{
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
}
```

- 挂载即生效:无头 `claude -p` 会按仓库 `.mcp.json` 自动连接(status=connected,
  无批准/无信任/无额外 flag,W-19 取证)。
- `${CLAUDE_PROJECT_DIR}` 展开为项目根;`${CLAUDE_PROJECT_DIR:-.}` 给了兜底。
- 实测证据:W-19 放行 → 重放拒 → 审计对账三步(在档取证)。
- 插件页(挂法/证据/阻塞清单):`docs/plugins/claude-code.md`(W-41)。

## Trae(配置已备,待真插实测)

现物在仓库 `.trae/mcp.json`:

```json
{
  "mcpServers": {
    "wanning": {
      "command": "cargo",
      "args": [
        "run", "--quiet", "-p", "wanning-mcp", "--",
        "--wal", "${workspaceFolder}/target/mcp-demo.wal"
      ]
    }
  }
}
```

- 变量名是 `${workspaceFolder}`(Trae 官方 docs 直核,W-17)。
- **与 Claude Code 指向同一份 WAL 是刻意设计**:两平台并发双挂时单写者锁
  保证第二个写进程 fail-closed 拒启(W-18),预算上限不可能被合力突破。
- 插件页(挂法/生成器输出/待实测清单):`docs/plugins/trae.md`(W-41)。

## Kimi Code CLI(W-40 本机实测通过)

现役 kimi-code 0.39.1 **没有 `kimi mcp` 子命令**(W-17 记录的 `kimi mcp add` 属
legacy kimi-cli 挂法,本机已迁移到 kimi-code,迁移痕迹在档)。现役挂法 = 配置文件:

```bash
# 生成配置内容(绝对路径占位符手改;官方无 ${...} 路径变量)
cargo run -p wanning-init -- --platform kimi

# 存为用户级(所有项目生效)或项目级(仅该仓):
#   用户级:~/.kimi-code/mcp.json(即 $KIMI_CODE_HOME/mcp.json)
#   项目级:<repo>/.kimi-code/mcp.json —— 未信任目录会弹 workspace trust 提示
#           (默认拒绝信任,核对列出的命令后再确认);用户级不经该提示
```

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

- stdio 由 `command` 字段隐含,**无 `type` 字段**(官方示例如此;与 Claude Code
  的 `type: "stdio"` 是刻意差异)。
- TUI 内交互管理:`/mcp-config`(增删改)、`/mcp`(连接状态)。
- **W-40 实测证据**:隔离 `KIMI_CODE_HOME` 下真 kimi 0.39.1 二进制拉起
  wanning-mcp、注入两工具(`mcp__wanning__*`),三轮判定落 WAL(allow 500 分 /
  同 nonce replay 拒 / over_budget 拒),跨会话同账;模型侧为本地 mock,真实模型
  会话复证待模型额度放行(W-42 修正:登录凭证 2026-08-30 已在档)。
  调研全文 `docs/research/kimi-code-cli.md`;插件页 `docs/plugins/kimi.md`。

## WorkBuddy

**腾讯出品的全场景 AI 办公工作台**(桌面应用;W-37 直核官方 docs,首轮 W-14/W-17
「查不到」已破——官网首页 JS 渲染无正文,但 docs 子树静态可抓,sitemap 直达
MCP-Guide 页):

- 支持 MCP:WorkBuddy 作 MCP 客户端接入外部工具,配置后自动调用对应 MCP Server
- 配置位置:用户级 `~/.workbuddy/mcp.json`(所有项目)或项目级
  `<项目目录>/.workbuddy/mcp.json`(单项目)
- JSON 结构与 `.mcp.json` 同款:顶层 `mcpServers` → 名字键 → `command`/`args`/`env`
  (官方示例无 `type` 字段;未提及 `${...}` 路径变量 → WAL 路径手改绝对路径)
- 也可走 UI:侧边栏 插件 → MCP 服务器 → 配置 MCP(可视化,免改文件)
- 生成器:`cargo run -p wanning-init -- --platform workbuddy`(W-37 已入矩阵,
  字段契约测试锁定)
- 真插实测待老板装桌面端(传输形态按官方命令启动式示例推断 stdio,待实测)
- 插件页(它是什么/两种挂法/生成器输出/待实测清单):`docs/plugins/workbuddy.md`(W-41)

来源:https://www.workbuddy.cn/docs/workbuddy/From-Beginner-to-Expert-Guide/Function-Description/MCP-Guide
调研全文(含查不到清单):docs/research/workbuddy.md

## 接入后第一件事

```bash
# 验证闸真的在:发一笔超预算意图,应被拒(reason=over_budget),且审计落行
# 审计随时可读:
cargo run -p wanning-demo -- --export-audit <target>/mcp-demo.wal --out audit.html
```

- 工具面只有「闸评估 + 审计读取」,零网络零消费;撤销不设工具(agent 能撤销
  就能复活,W-17)。
- 通知零响应零执行;batch 单条拒绝(W-20)。

## Android / 自有 app 宿主(ANAI)

不走 stdio:走 **SDK embed**。五步见 `sdk-embed.md`。
