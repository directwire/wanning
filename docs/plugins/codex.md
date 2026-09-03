# Codex CLI × Wanning 插件页

> W-35 落地。调研全文(来源/实测细节):`docs/research/codex-mcp.md`。
> 状态:**配置面免登录已实测,钥匙即插;会话级使用待 OpenAI 登录**(烧 OpenAI
> 额度的动作一律所有者亲自)。
> 本机实测基线:codex-cli **0.133.0**(npm 全局);官方文档另有 0.152.1 可升级。

## 一键直写与体检(W-51)

- `wanning init --platform codex --install`:**不支持,fail-closed**——codex 主配置
  `config.toml` 是 TOML 文本面,文本合并的风险大于收益,报错给 `--out` 人工指引
  (生成片段后按本页「配置现物」人工追加);绝不乱写主配置。
- `wanning doctor --platform codex`:体检可跑——从 `$CODEX_HOME/config.toml` 容忍式
  读 `[mcp_servers.wanning]` 段,六项检查同其他平台(真握手用隔离临时账本,零模型
  零外网零真实消费;每项 ❌ 带 ✗ 修复命令)。

## 安装(所有者还没装的话)

```powershell
npm install -g @openai/codex      # 本机已有 0.133.0,勿升级(全局升级写 C 盘,违反铁律)
codex --version                   # → codex-cli 0.133.0
```

来源:官方 README 安装节(npm / 安装脚本 / brew 四法,2026-09-02 直核)。

## 配置现物(两选一)

**方式 A:命令行注册**(实测推荐;登录无关,只写配置)

```powershell
codex mcp add wanning -- <绝对路径>\target\debug\wanning-mcp.exe --wal <绝对路径>\target\codex-demo.wal
codex mcp list                    # 应看到 wanning 一行,Status=enabled
codex mcp get wanning             # 回执 enabled/transport: stdio/command/args
```

**方式 B:手写 config.toml 片段**(`~/.codex/config.toml` 全局,或
`<repo>/.codex/config.toml` project-scoped——后者 trust 机制待实测,先用手动全局)

```toml
[mcp_servers.wanning]
command = 'D:\path\to\Wanning\target\debug\wanning-mcp.exe'
args = ["--wal", 'D:\path\to\Wanning\target\codex-demo.wal']
# 可选加固(文档字段,待登录后实测):
# required = true                 # server 起不来就拒绝启动会话(fail-closed 同构)
# startup_timeout_sec = 10        # 默认 10s;cargo 冷编译可能不够,用方式 A' 见下
```

**cargo 冷编译注意**:上面两种都指向 `target\debug\wanning-mcp.exe`(先
`cargo build -p wanning-mcp`)。想免预构建可用 `cargo run` 形态——codex 没有
`.mcp.json` 式的路径变量,相对路径要配 `cwd` 字段:

```toml
[mcp_servers.wanning]
command = 'cargo'
args = ["run", "--quiet", "-p", "wanning-mcp", "--", "--wal", "target/codex-demo.wal"]
cwd = 'D:\path\to\Wanning'        # 相对 --wal 与包路径都按这里解析
startup_timeout_sec = 30          # cargo 冷编译超过默认 10s,放宽
```

(cargo 形态字段组合来自 config-reference 文档,未实测,标待验证。)

**已实测证据**:方式 A 写出的 TOML 被原样解析后 spawn,真走通 initialize(协商
2025-06-18)→ 2 工具 → allow(行2)→ replay 拒 → audit_tail,WAL 3 行完整性链
齐全(取证输出在档)。

## 工具面(登录后 codex 会话内可用)

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
  授权/撤销走所有者侧(见 `docs/examples/sdk-embed.md` 与白皮书 §4)。
- **同一 WAL 多平台并发**:`.mcp.json`(Claude Code)+ `.trae/mcp.json` + codex 的
  config.toml 若指向同一份 WAL,第二个写进程 fail-closed 拒启(W-18 单写者锁)——
  这是特性:两本账不会悄悄分叉。
- 预算内/超额/重放/撤销四条语义与 Claude Code 实测一致(W-19,同一把闸)。

## 阻塞清单

| 项 | 状态 |
|---|---|
| 会话内真调工具(端到端) | **待 OpenAI 登录**(且烧 OpenAI 额度,所有者亲自) |
| `required: true` / `startup_timeout_sec` 行为 | 文档直核,待登录实测 |
| project-scoped `.codex/config.toml` trust | 查不到细节,待实测 |
| codex 升级 0.152.1 | 不做(npm 全局写 C 盘);新版本行为差异待所有者拍板后重测 |
