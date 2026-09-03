# 多平台接入一页(W-33;W-44 加 DeepSeek Harness)

> 目标:**5 分钟内**找到任一平台的现成接入答案。每条都是可直接复制的现物;
> 调研出处见 `docs/research/mcp-consumption.md`(W-17,官方来源逐条标注;
> DeepSeek Harness 见 `docs/plugins/deepseek-harness.md`,W-44)。
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
  会话复证待所有者放行烧额度(W-42 修正:登录凭证 2026-08-30 已在档,所有者亲自)。
  取证在档(W-40 节);调研全文 `docs/research/kimi-code-cli.md`;插件页 `docs/plugins/kimi.md`。

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
- 真插实测待所有者装桌面端(传输形态按官方命令启动式示例推断 stdio,待实测)
- 插件页(它是什么/两种挂法/生成器输出/待实测清单):`docs/plugins/workbuddy.md`(W-41)

来源:https://www.workbuddy.cn/docs/workbuddy/From-Beginner-to-Expert-Guide/Function-Description/MCP-Guide
调研全文(含查不到清单):docs/research/workbuddy.md

## DeepSeek Harness(配置面 + patch 组合已实测,W-44)

**DeepSeek 官方开源 agent harness**(developer preview,本机 0.1.0-rc.7 已装)。
与其他平台根本不同:**不是 mcp.json,是 Cordis overlay YAML patch**——
`- insert:` 列表声明插件,`--patch` 临时挂或合并追加进
`<profile>/cordis.patch.yml` / `$DSH_HOME/cordis.patch.yml`(绝不整文件覆盖):

- 生成器:`cargo run -p wanning-init -- --platform deepseek-harness`
  (`--out <路径>.cordis.yml` 显式落盘,已存在绝不覆盖)
- 启用:`dsh --profile <名> --patch wanning.cordis.yml`(临时)或把 insert 块
  合并追加进 patch 文件(持久)
- 工具现身名:`mcp__wanning__wanning_gate_evaluate` /
  `mcp__wanning__wanning_audit_tail`(`serverName: wanning` →
  `mcp__<serverName>__<rawName>`,与 Claude Code/Codex 同形)
- **⚠️ env 剥离(官方行为)**:stdio 桥启动子进程前丢弃 ambient
  credential-shaped 与全部 `DSH_*` 环境变量,显式 `env` 在 scrub 之后才合并——
  将来接真实通道时密钥必须写进 row 的 `config.env`,不能赌继承
- **本机已实测**(W-44,隔离 `DSH_HOME`,零网络零会话):真 dsh 二进制
  `--dump-config --patch <生成文件>` exit 0,wanning 行作为独立 patch 层进入
  组合树;坏 YAML 对照 exit 1。诚实边界:dump 级只验语法 + patch 形态,
  插件包解析/工具注入待会话级
- 会话级端到端待所有者放行(dsh 会话 = 模型会话 + 网络,红线 2)
- 插件页(机制/字段契约/取证/阻塞清单):`docs/plugins/deepseek-harness.md`(W-44)

## OpenClaw(全链路已实测,W-45 配置面 + W-47 agent 回合)

**开源个人 agent 框架**(gateway + 多渠道消息接入;本机 2026.5.22 a374c3a 已装)。
原生支持 MCP:`openclaw mcp` 子命令族管理注册,落
`$OPENCLAW_STATE_DIR/openclaw.json` 的 `mcp.servers.<name>` 段:

- 生成器:`wanning-init --platform openclaw` → 打印一条
  `openclaw mcp set wanning '{...}'` 命令,复制执行即完成注册
  (openclaw.json 由宿主管理,CLI 只动 wanning 一段,绝不整文件覆盖)
- 字段面:`{command, args}`(与 Claude Code `mcpServers` 同形,无 `type`);
  官方 stdio 字段还有 `env`/`cwd`,env 有安全过滤(拦 `NODE_OPTIONS`/
  `PYTHONSTARTUP`/`DYLD_*`/`LD_*` 等)→ 将来接真实通道时密钥必须写进 `env`
- **本机已实测**(W-45,隔离 `OPENCLAW_STATE_DIR`/`OPENCLAW_CONFIG_PATH`,
  零网络零会话):`mcp set/list/show` 全绿,落盘原文与生成内容一致;
  `models.providers` 挂本地 mock LLM(`127.0.0.1` OpenAI-compatible)exit 0,
  `openclaw models list` 显示 `wanningmock/wanning-mock-model`
- **agent 回合已实测**(W-47,隔离 `OPENCLAW_WORKSPACE_DIR` 也必须给,
  模型侧本地 mock,零外网):`openclaw agent --local --agent main -m "…" --json`
  真回合——「静默退出」根因查明(缺 `-m` 报错只在 stderr + 缺会话选择器 +
  cwd 大目录扫描税 16–17s);工具现身名 `wanning__wanning_gate_evaluate`
  (无 `mcp__` 前缀),allow 400 落 WAL 行2、同 nonce 第三轮 replay 拒行3,
  完整性链连续(`wanning audit` 逐行验证通过)
- **诚实边界**:真实模型会话复证待所有者放行(红线 2);env 安全过滤逐键行为
  待真实会话。另:所有者真实 `~/.openclaw/openclaw.json` 当前
  是坏的(persona-migration 残留),分身不修,所有者跑 set 前需先处理
- 插件页:`docs/plugins/openclaw.md`(W-45 + W-47)

## Hermes(全链路已实测,W-45)

**Nous Research 开源 agent harness**(本机 v0.19.1 已装)。原生支持 MCP:
`hermes mcp` 子命令族管理注册,落 `$HERMES_HOME/config.yaml` 的
`mcp_servers.<name>` 段:

- 生成器:`wanning-init --platform hermes` → 打印一条
  `hermes mcp add wanning --command <bin> --args --wal <wal> --budget 1000`
  命令;`mcp add` 是 **discovery-first**(真连一次发现工具,挂载即验证);
  非 TTY 用 `echo y | hermes mcp add ...` 喂确认
- **全链路已实测**(W-45,隔离 `HERMES_HOME` + 本地 mock LLM,零外网零真实消费):
  `mcp test wanning` 真连 141ms;one-shot `hermes -z "..." -t wanning`
  → 闸判定落 WAL:allow 400 分(第 2 行)→ 二次会话同 nonce →
  `deny reason=replay`(第 3 行),完整性链连续
- **两个实测教训**(其他平台没有):①MCP 工具在 deferred catalog,模型侧经
  `tool_call(name, arguments)` 间接调用,直接调 `mcp__wanning__*` 名报
  `does not exist`;②one-shot 默认 cli 工具集**不含 MCP 工具**,须显式
  `-t wanning`(mock 请求日志物证:默认 20 个内置工具零 wanning,带 `-t` 后
  两个工具进目录)
- 真实模型会话待所有者放行(红线 2);交互 TUI 会话待所有者(需 TTY)
- 插件页:`docs/plugins/hermes.md`(W-45)

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

不走 stdio:走 **SDK embed**(W-32 设计稿在档),SDK 五步见 `sdk-embed.md`。
