# WorkBuddy × Wanning 插件页

> W-41 落地(支持性 W-37 直核官方 docs,破 W-17「查不到」)。调研全文
> (含查不到清单与来源汇总):`docs/research/workbuddy.md`。
> 状态:**官方 MCP 支持已直核 + 生成器已入矩阵;真插实测待所有者装桌面端**。
> 传输形态是推断(官方未明示 stdio/HTTP 字样)——待实测清单第一项。

## 它是什么(先破冰再谈接入)

- **腾讯出品的全场景 AI 办公工作台**(桌面应用,Win/Mac;W-37 直核官方
  Overview 页原句;产品线含小程序/移动端/企业版,与 CodeBuddy 同 docs 站)。
- W-17 首轮「查不到」已破——官网首页 JS 渲染无正文,但 docs 子树静态可抓:
  `robots.txt → sitemap.xml → docs 子树 sitemap(94 页)→ MCP-Guide 页`。
  教训:**「查不到」要区分「查无此事」与「路数不对」**。
- 来源(W-37 直核):<https://www.workbuddy.cn/docs/workbuddy/From-Beginner-to-Expert-Guide/Function-Description/MCP-Guide>

## 安装(所有者还没装)

- 本机未装(2026-09-02 核);腾讯官方渠道下载桌面端(装哪台机/哪个盘由所有者定,
  **注意铁律 4:不装 C 盘**)。
- 装好后按「待实测清单」逐项收口;闸侧零改动(接入机制同 Claude Code 系,
  闸已真插验证过)。

## 配置现物(两种挂法二选一)

- **用户级**:`~/.workbuddy/mcp.json`(所有项目生效)
- **项目级**:`<项目目录>/.workbuddy/mcp.json`(仅该仓)
- UI 免改文件:侧边栏 插件 → MCP 服务器 → 配置 MCP(W-37 直核)

JSON 结构与 Claude Code 的 `.mcp.json` 同款(顶层 `mcpServers` → 名字键 →
`command`/`args`/`env`),两处官方差异(W-37 直核):

- 官方示例**无 `type` 字段**(stdio 由 `command` 命令启动式隐含,与 Claude Code
  的 `type: "stdio"` 是刻意差异);
- **未提及 `${...}` 路径变量** → WAL 路径手改绝对路径。

用户级 vs 项目级影响账本粒度(每项目一本 or 共用一本)——无论哪种,单写者锁
(W-18)兜底:第二个写进程 fail-closed 拒启。

## 生成器(占位符手改)

```bash
cargo run -p wanning-init -- --platform workbuddy
# stdout 打印说明(notes)+ .workbuddy/mcp.json 内容
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
        "{{WAL_PATH}}"
      ],
      "command": "cargo"
    }
  }
}
```

- `{{WAL_PATH}}` 占位符手改成绝对路径(官方无路径变量);头部注释即提示
  「传输形态是推断项」。`cargo run` 形态需工作目录在仓库根;要固定二进制可先
  `cargo build -p wanning-mcp` 后把 `command`/`args` 换成绝对路径写法。

## 工具面(workbuddy 会话内可用)

| 工具 | 作用 | 权限语义 |
|---|---|---|
| `wanning_gate_evaluate` | 提交消费意图,闸判定(allow/deny + reason) | 判定与拒绝**都落审计**(WAL) |
| `wanning_audit_tail` | 读审计尾 | 只读 |

- **没有撤销工具、没有授权工具**(agent 能撤销就能复活,能授权就能自授权)——
  授权/撤销走所有者侧(白皮书 §4)。
- 四条语义(预算内放行/超额拒/重放拒/审计对账)与 Claude Code 实测(W-19)
  一致:同一把闸。

## 待实测清单(桌面端装好后逐项收口)

| 项 | 现状 |
|---|---|
| 传输形态 stdio | **推断**(官方命令启动式示例);官方未明示,实测第一项 |
| `.workbuddy/mcp.json` 被拉起、wanning-mcp 进程出现 | 待实测 |
| 两工具注入 + 闸往返(allow/replay/over_budget 三判定落 WAL) | 待实测(机制同 Claude Code,闸零改动) |
| 工具调用审批流/「默认权限与安全沙箱」页 | W-37 未读到该页,待实测 |
| 项目级挂法是否触发 trust/信任提示 | 待实测(用户级 vs 项目级二选一,见 待实测清单) |
