# 调研:MCP server 消费方式(各 agent 平台怎么挂 wanning-mcp)

> W-17(P1 前置砖)·2026-09-02 夜·自动分身。
> 问题:P1 第一步是「Claude Code 真插实测」。动手前必须先搞清:各平台把 stdio MCP server
> 挂在哪个文件、字段叫什么、路径变量用什么。**每个结论带来源;查不到的写「查不到,待人工」。**
> 标注惯例沿用 W-12/W-13:`[直核]`=直接抓官方域名核实;`[摘要]`=检索摘要,未直核。

## 速读结论

1. 三平台消费形态同构:`mcpServers` 对象 + `command/args/env`(Claude Code 另有可选 `type` 字段)——一份 server 配置改个路径变量就能三处通用。
2. Claude Code:项目根 `.mcp.json` [直核];spawn 进程的 **cwd 不保证是项目根**,官方注入 `CLAUDE_PROJECT_DIR`,路径写 `${CLAUDE_PROJECT_DIR:-.}` [直核];项目级 server 交互会话要用户批准一次,`claude -p`/SDK 免批 [直核+**W-19 实测**:未信任目录、`~/.claude.json` 零条目的裸 `-p` 直接 `status=connected`,无需批准无需 flag;`claude mcp list` 显示的 `⏸ Pending approval` 只是交互会话批准簿记,不拦 `-p`]。
   **协议版本(W-19 实测)**:claude 2.1.234 的 initialize 提议 `2025-11-25`;server 不支持来版时按 spec「Version Negotiation」回**自己支持的最高版**(不是报错)——W-15 旧实现回 -32602 被 claude 判死连接,已修。
3. Trae:项目根 `.trae/mcp.json`(须在设置里开「启用项目级 MCP」),字段 `command/args/env`,唯一支持的变量 `${workspaceFolder}` [直核]。
4. Kimi Code CLI(现役,本机 0.39.1):`$KIMI_CODE_HOME/mcp.json`(用户级)或 `<repo>/.kimi-code/mcp.json`(项目级),`mcpServers` 结构、**无 `type` 字段**、无 `${...}` 变量 → 绝对路径;TUI 内 `/mcp-config` 管理;**无 `kimi mcp` 子命令** [直核,W-40 本机实测——W-17 记录的 `kimi mcp add` 属 legacy kimi-cli 挂法(老板机器有迁移痕迹),详见 `docs/research/kimi-code-cli.md`]。
5. WorkBuddy:`.workbuddy/mcp.json`(用户级 `~/.workbuddy/mcp.json` 或项目级 `<项目目录>/.workbuddy/mcp.json`),`mcpServers` 结构同款,官方示例无 `type` 字段、未提及 `${...}` 变量 [直核,W-37 破冰——首轮查不到,换路数(sitemap 绕 JS 首页)后破,详见 `docs/research/workbuddy.md`]。
6. 本仓已带配置:`.mcp.json` + `.trae/mcp.json`,默认审计 `<仓库根>/target/mcp-demo.wal`(gitignore 不入库);配置契约有测试锁定(`crates/wanning-mcp/tests/mcp_json_config.rs`),`cargo run` 两会话实录在档。
7. 踩坑已修(W-17):同一 WAL 二次启动曾**不回放旧账**(nonce 洗白/撤销被复活)→ `WanningState::live_resuming`(回放对账 fail-closed → 系统时钟续写同一 WAL)。

## Claude Code(第一个目标平台)[直核]

来源:<https://code.claude.com/docs/en/mcp>(2026-09-02 抓取)

- **项目级配置文件**:项目根 `.mcp.json`,顶层 `mcpServers`;stdio 条目字段 `type`(可选,`"stdio"`)、`command`、`args`、`env`。原文:「a JSON entry that has a `url` but no `type` is a configuration error, because Claude Code reads an entry with no `type` as a stdio server」。
- **`${VAR}` / `${VAR:-default}` 展开位置**:`command`、`args`、`env`(HTTP 型再加 `url`/`headers`)。
- **cwd 语义**(关键):官方不保证子进程 cwd,而是注入环境变量——「Claude Code sets `CLAUDE_PROJECT_DIR` in the spawned server's environment to the project root, so your server can resolve project-relative paths **without depending on the working directory**」。→ 项目相对路径应写 `${CLAUDE_PROJECT_DIR:-.}/...`。
- **批准**:交互会话首次使用项目级 server 会弹批准;`claude -p`、Agent SDK、云会话无法弹窗,**直接加载**。重置选择:`claude mcp reset-project-choices`。工作区信任(v2.1.196+):克隆来的仓库不能自己批准自己的 server(提交 `enableAllProjectMcpServers`/`enabledMcpjsonServers` 在未信任目录被忽略)。
- **CLI 等价**:`claude mcp add [--scope project] [--env K=V] <名> -- <命令> [参数…]`(`--` 之后才是 server 启动命令;默认 scope=local)。
- 该页**没有** Windows 专项说明(无 `cmd /c` 包装要求);`cargo` 是真实 `.exe`,无 PATHEXT 问题。

## Trae [直核]

来源:<https://docs.trae.cn/ide/add-mcp-servers>、<https://docs.trae.cn/ide/model-context-protocol>(2026-09-02 抓取;`docs.trae.ai` 国际站同结构)

- **项目级配置**:项目根 `.trae/mcp.json`,须在 设置 > MCP 打开「启用项目级 MCP」(官方同时警告只信任工作区文件)。
- **stdio 字段表**:`command`(必填,PATH 上可解析或全路径,**不能含空格**)、`args`(可选,全字符串)、`env`(可选,全字符串)。无 `type` 字段。
- **变量**:官方文档原文「`${workspaceFolder}` 是唯一支持的变量,启动时替换为项目根」。→ 本仓 Trae 配置用它锚定审计路径;**在 args 中实际展开行为待 P1 实测**(文档只说支持该变量,未逐字段说明)。
- 市场里标「Local」的 server 需本机装 NPX/UVX;手写本地 `command` 不受此限。

## Kimi Code CLI(W-17 直核 legacy → W-40 本机实测现役)[直核]

来源:官方文档 <https://moonshotai.github.io/kimi-code/en/customization/mcp.html> 与
Configuration 页(`llms-full.txt`,2026-09-02 抓取)+ 本机 0.39.1 隔离实验;legacy
kimi-cli README(<https://github.com/MoonshotAI/kimi-cli>,W-17 抓取)。

- **两代产品**:W-17 直核的 `kimi mcp add --transport stdio` / `--mcp-config-file`
  属 legacy kimi-cli(README 自述将收摊);老板机器已迁移到 **kimi-code 0.39.1**
  (`~/.kimi` → `~/.kimi-code`,迁移痕迹实测在档),现役版**无 `kimi mcp` 子命令**。
- **现役挂法**:用户级 `$KIMI_CODE_HOME/mcp.json` 或项目级 `<repo>/.kimi-code/mcp.json`
  (同名项目级覆盖用户级),`mcpServers` → `command`/`args`/`env`/`cwd` 等;
  stdio 由 `command` 字段隐含(**无 `type` 字段**);HTTP 型用 `url`;SSE 显式
  `transport: "sse"`。传输/超时/字段表见 `docs/research/kimi-code-cli.md` §②。
- **权限**:工具名 `mcp__<server>__<tool>`;`[[permission.rules]]`(config.toml)
  可预放行(如 `pattern = "mcp__wanning__*"`);`-p` 非交互默认 auto 权限。项目级
  server 在未信任目录弹 workspace trust 提示(**默认拒绝信任**),用户级不经该提示。
- **W-40 本机实测**:隔离 `KIMI_CODE_HOME` + 手写 mcp.json 挂 wanning-mcp,真 kimi
  二进制拉起闸、注入两工具,三轮判定落 WAL(allow 500 分 / 同 nonce replay 拒 /
  over_budget 拒),跨会话同账、完整性链连续;模型侧为本地 mock(真实模型会话
  复证待老板放行烧额度;W-42 修正 2026-09-02:登录凭证 2026-08-30 已在档,
  「待登录」出自隔离空 home 报错)。取证见 `docs/tasks/P0-demo-closedloop.md`
  W-40 节。
- 生成器已按现役挂法修订(`wanning-init --platform kimi` 输出 mcp.json 内容,
  W-40 契约测试换血)。

## WorkBuddy [W-17 查不到 → W-37 破冰直核]

- W-17 首轮:检索(搜狗)显示其为腾讯 CodeBuddy 团队的 AI agent 办公产品,官网域名 `workbuddy.cn` [摘要];直接抓首页 JS 渲染、正文为空 [直核失败] → 当轮结论「查不到待人工」。
- **W-37 攻坚轮(2026-09-02)换路数破冰**:robots.txt → sitemap.xml 索引 → docs 子树 sitemap(94 页正文清单)→ `MCP-Guide` 页直核。结论:腾讯出品的 AI 办公工作台(桌面应用),**支持 MCP 客户端**,配置 `.workbuddy/mcp.json`(用户级 `~/.workbuddy/mcp.json` 或项目级 `<项目目录>/.workbuddy/mcp.json`,`mcpServers` 同款结构,无 `type`,无 `${...}` 变量),UI 路径「侧边栏 插件 → MCP 服务器 → 配置 MCP」。字段权威与查不到清单见 `docs/research/workbuddy.md`;生成器矩阵 W-37 已同步(`cargo run -p wanning-init -- --platform workbuddy`)。

## 消费草图(本仓已落地的部分)

- `crates/wanning-mcp`:bin 参数 `--wal <路径>`(必填,无审计不服务)/`--cap-cents`(默认 1000=¥10)/`--hours`(默认 24)。工具面仅 `wanning_gate_evaluate` + `wanning_audit_tail`;**支付工具与撤销永不进 MCP 面**(决策记录 2026-09-02)。
- `<仓库根>/.mcp.json`(Claude Code):`command=cargo`,`args=["run","--quiet","-p","wanning-mcp","--","--wal","${CLAUDE_PROJECT_DIR:-.}/target/mcp-demo.wal"]`。
- `<仓库根>/.trae/mcp.json`(Trae):同款,变量换 `${workspaceFolder}`。
- Kimi(现役挂法,用户级或项目级 `.kimi-code/mcp.json`):`wanning-init --platform kimi`
  生成 mcp.json 内容(绝对路径占位符手改;W-40 实测可用),不再用 `kimi mcp add`
  (现役版无该子命令)。
- **契约锁定**:`tests/mcp_json_config.rs` 解析这两份配置,断言字段/参数/路径变量,并把配置里的服务端参数原样喂给真 bin 跑完整握手——配置烂了测试当场红。
- 已知地雷:① `cargo run` 首次触发编译,可先 `cargo build` 后把 `command` 换成二进制绝对路径;② `cargo` 必须在平台 spawn 环境 PATH 里;③ 审计在 `target/` 下,`cargo clean` 会清掉(重新演示无妨,销毁审计须自知);④ 撤销/过期后想重演,换新 WAL 路径——重启**不会**复活授权(见实测记录)。

## P1 实测清单(下一步,老板或下一会话)

1. ~~Claude Code:本仓目录起 `claude` → 批准项目级 server → `/mcp` 应见 `wanning` → 对话触发 `wanning_gate_evaluate` → `wanning_audit_tail` 对照 WAL 行。~~ **已完成(W-19,2026-09-02)**:headless `claude -p` 裸跑即连(无需批准/信任),三步证据链(放行→重放拒→审计对账)全过,证据见 `docs/tasks/P0-demo-closedloop.md` W-19 两节。交互会话路径(`/mcp` 面板查看)随用随验,不再是前置阻塞。
2. Trae:开「启用项目级 MCP」→ 确认 `${workspaceFolder}` 在 args 里真的被展开 → 同上对账。
3. ~~Kimi Code CLI:装新版(旧 kimi-cli 将收摊)→ `kimi mcp add` 绝对路径 → 同上。~~ **挂法与闸往返已实测(W-40,2026-09-02)**:现役 kimi-code 0.39.1 无 `kimi mcp` 子命令,挂法改为 `.kimi-code/mcp.json`(生成器已修订);隔离 `KIMI_CODE_HOME` 下真 kimi 二进制完成 MCP 往返(allow/replay/over_budget 三判定落 WAL),模型侧为本地 mock。剩真实模型会话复证,待老板放行烧额度(W-42 修正:登录凭证 2026-08-30 已在档,`kimi login` 大概率可省;待实测清单)。
4. WorkBuddy:**W-37 已破冰**(配置面直核落档,生成器已出活);剩真插实测待老板装桌面端(待实测清单)。

## W-19 排障踩坑(Windows + git-bash,给下一会话省命)

- **协议协商不是报错**:server 不支持客户端提议的版本时,spec 要求回「自己支持的最高版」;回 -32602 等错误会被 Claude Code 直接判 `status=failed` 且不重试。抓包方法:python tee 垫片(逐行 `<<`/`>>` 落日志,stdout 只透传 server 字节)挂在 `--mcp-config` 中间,一次运行拿全字节证据。
- **`claude -p` 的 stream-json init 事件**带 `"mcp_servers":[{"name":…,"status":"connected|failed"}]`——判断挂没挂上直接看它,别靠模型转述;`--output-format stream-json --verbose` + `grep mcp_servers`。
- **git-bash heredoc 会把 `\\` 折叠成 `\`**:写含 Windows 反斜杠路径的 JSON 配置文件会得到非法 JSON(claude 报 `MCP config is not a valid JSON`);JSON 里一律用正斜杠(Windows spawn 认)或用 Write 工具写文件。
- **MSYS 会把 `/tmp/...` 参数转成 Windows 路径**(实测能被 claude 读到),但显式 `$(cygpath -w …)` 更可读、更稳。
- **`claude mcp list` 的健康检查只对「已批准」server 生效**,且不吃 `--mcp-config`;`claude mcp add` 本地注册在未信任目录会挂起等 trust 对话框(W-19 实测 180s 零输出)。
- **配置类排障先抓字节再看门**:W-19 探路会话先入为主归因 workspace trust,烧掉 ≈$1.75 才由字节级抓包定位到真因(协议版本);自写客户端「恰好能连」反而会掩盖真因(它发的版本恰好与 server 一致)。
