# WorkBuddy 调研(W-37 二次攻坚)

> W-17 首轮结论「官网 JS 渲染无正文,MCP 支持性查不到待人工」。本轮换路数:
> **GitHub topic/仓库检索 + 官网 robots.txt/sitemap.xml 直抓**(绕开 JS 渲染的首页,
> 从 sitemap 索引直接定位 docs 正文页)。两路都命中,下面每条带来源。
> 检索日期 2026-09-02。标注口径:`[直核]` = 打开官方页面逐字核对;`[摘要]` = 第三方
> 来源摘要,仅佐证不作为字段依据;`[推断]` = 明确标出的推理,不当事实用。

## 一、WorkBuddy 是什么(任务书第一问)

- **[直核] 腾讯出品的全场景 AI 办公工作台**:官方 docs 原句
  「WorkBuddy 是腾讯出品的全场景 AI 办公工作台」,功能=对话式下达任务、自主拆解
  规划执行、多模态任务(文档/表格/PPT/数据分析)、本地文件批量处理——交付
  「可验收的结果」而非聊天回复。
  来源:https://www.workbuddy.cn/docs/workbuddy/Overview
- **[直核] 形态 = 桌面应用**(有 Windows / Mac 两份安装指南),产品线还有
  WorkBuddy 小程序、移动端、企业版;同 docs 站并列 CodeBuddy IDE/插件/CLI 产品线。
  来源:同上 Overview 页 + sitemap(workbuddyapp 子树 15 页为移动端文档)。
- **[摘要] GitHub 佐证腾讯所属**:`Tencent/workbuddy-bench`(Tencent 官方组织,
  「benchmark for evaluating coding agents on real-world work」,引用署名含
  「Tencent Youtu Lab and Keen Security Lab and WorkBuddy and Yunding Security Lab」,
  Tencent license)。来源:https://github.com/Tencent/workbuddy-bench
- **[摘要] 第三方生态佐证桌面形态**:workbuddy-remote(桌面版 CDP 遥控桥,写明
  「桌面 WorkBuddy 仍然负责原生会话、任务、内存和账号状态」;官方 web 版在
  codebuddy.cn/agents)。来源:https://github.com/vergess3/workbuddy-remote
  (第三方仓库,仅佐证形态,不作字段依据)

## 二、MCP 支持性(W-17 查不到的那一条,本轮 [直核] 破)

来源:**https://www.workbuddy.cn/docs/workbuddy/From-Beginner-to-Expert-Guide/Function-Description/MCP-Guide**
(官方 docs,本轮唯一字段权威;下面字段全部逐字直核)

- **[直核] WorkBuddy 是 MCP 客户端/宿主**:MCP「用于将外部工具和服务接入
  WorkBuddy」,配置后「WorkBuddy 会自动调用对应的 MCP Server」;文档有
  「MCP Server 状态」检查(🟢 连接成功 / 🔴 配置异常)。
- **[直核] 配置双路径**:
  - 用户级(所有项目生效):`~/.workbuddy/mcp.json`
  - 项目级(单个项目):`<项目目录>/.workbuddy/mcp.json`
- **[直核] JSON 结构 = 顶层 `mcpServers` 对象 → 服务器名键 → `command` / `args` /
  `env` 三字段**(官方示例 `"command": "uvx"`、`"args": ["wecom-bot-mcp-server"]`、
  `"env": {"WECOM_WEBHOOK_URL": "…"}`)——**与 Claude Code `.mcp.json` 的
  `mcpServers` 结构同款**,与 kimi `--mcp-config-file` 同款(见
  docs/research/mcp-consumption.md);字段面没有 `type`(与 claude-code 现物有
  `type: stdio` 是刻意差异,生成器按 WorkBuddy 官方示例不带 type)。
- **[直核] UI 配置路径**:「进入侧边栏 插件 → 点击右上角 MCP 服务器 → 配置 MCP」,
  「无需编码、无需手动改配置文件,可视化操作即可完成接入」——两条路并存。
- **[推断] 传输形态 = stdio**:官方示例是 `command`+`args` 启动式(uvx),与本仓
  wanning-mcp 的 stdio 启动同构;本页未出现 HTTP/SSE 字样,故按 stdio 接入。
  (推断依据:命令启动式 MCP server 的通行语义;待实测确认。)
- **[直核] 路径变量:文档未提及任何 `${...}` 变量扩展**(只有 `~` 和项目目录两种
  位置描述)→ **不用变量,占位符手改绝对路径**,与 kimi/codex 同判据。
- **[直核] 本页未描述工具调用审批流**(侧边栏另有「默认权限与安全沙箱」页,本轮
  未读)→ 权限面待实测,不在调研里臆造。

## 三、与 Wanning 闸的接入语义(设计结论,非 WorkBuddy 官方说法)

- WorkBuddy 挂 `wanning-mcp` = agent 消费意图先进闸(同一 MCP 工具面
  `wanning_gate_evaluate` + `wanning_audit_tail`,无支付工具无撤销工具)。
- **单写者锁(W-18)是并发语义的守门人**:WorkBuddy 用户级/项目级双配置路径意味着
  「同一台机器多项目同挂一份 WAL」是现实场景——第二个写进程 fail-closed 拒启是
  特性不是缺陷;每项目独立 WAL 则天然隔离。
- 生成器矩阵同步:`wanning-init --platform workbuddy` 生成 `.workbuddy/mcp.json`
  内容(`mcpServers` 同款结构、无 `type`、无 `${}` 变量、WAL 路径占位符),
  W-36 契约测试锁定字段(官方示例无 type 是刻意差异)。

## 四、查不到清单(诚实边界)

- 传输方式 stdio/HTTP/SSE 的**官方明示字样**:查不到(本页只有命令启动式示例,
  stdio 是 [推断],待实测)。
- `${...}` 路径变量支持:查不到官方说法(文档未提及 → 按「不用」处理)。
- 工具调用审批流/安全沙箱细则:查不到(「默认权限与安全沙箱」页未读)。
- WorkBuddy CLI 形态(与 CodeBuddy CLI 是否同物):查不到;本仓按桌面应用接入。
- **Wanning × WorkBuddy 真插实测**:未做(需要所有者装桌面端并真插,本轮零编造
  只交付生成器与配置;实测走 待实测清单)。

## 五、攻坚路径复盘(给下一轮调研省命)

- W-17 首轮死在「首页 JS 渲染、正文空」→ 本轮绕法:`/robots.txt`(2026-06-03,
  RFC 9309)→ `/sitemap.xml`(索引)→ `/docs/sitemap.xml`(二级索引)→
  `sitemap-workbuddy-workbuddy.xml`(94 页正文清单,当场看到 `MCP-Guide`)。
  **docs 子树是静态可抓的**——首轮只抓首页等于只看了门脸。
- GitHub 仓库检索(api.github.com/search/repositories?q=workbuddy)命中 3871 个
  仓库,`Tencent/workbuddy-bench` 直接钉死公司归属——第三方生态(遥控桥/换肤/
  账号切换器)反向拼出产品形态。
- 检索工具教训沿用决策记录:WebSearch 返回伪搜索结果一律弃用,本轮用
  GitHub API + 官方域名直抓,零编造。

## 六、所有者一句话可解锁的剩余问题

1. WorkBuddy 桌面端装在哪台机器(实测前置;本机装桌面端涉及安装位置,不装 C 盘
   铁律下由所有者决定路径)。
2. 挂用户级(`~/.workbuddy/mcp.json`,全项目生效)还是项目级(单项目)——影响
   WAL 路径选择(每项目一本账 or 共用一本,单写者锁语义见第三节)。
