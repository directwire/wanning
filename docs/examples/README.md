# docs/examples · 对外样板包(W-33)

> 所有者/合作方 5 分钟找到任一接入路径的现成答案。所有样例全离线 + 本地 mock,
> 零真实消费(铁律 2)。

## 目录

| 文件 | 给谁看 | 一句话 |
|---|---|---|
| [multi-platform.md](multi-platform.md) | 想把闸挂上 agent 平台的人 | Claude Code(已实测)/ Trae / Kimi CLI 现成配置,WorkBuddy 查不到待人工 |
| [sdk-embed.md](sdk-embed.md) | 想把闸嵌进自己 app 的人 | SDK 嵌入五步走查(权威代码在 doctest,本页不复制代码) |
| [anchor-walkthrough.md](anchor-walkthrough.md) | 所有者/审计方 | 锚点签验两条命令走查(v1 HMAC + v2 ed25519) |
| [audit-sample.html](audit-sample.html) | 合作方/审计方 | full-loop-mock 真实产出的审计回放页(4 行账:注册/放行/拒绝/放行),自包含零 JS 零外链,file:// 离线可开 |

## 对外样板清单(谈合作时的三件套)

1. **四卖点输出**:预算内放行/超额拒绝/撤销即停/全程审计——
   `cargo run -p wanning-demo -- --scenario four-selling-points` 真实跑出,
   复现:`README.md` Quickstart。
2. **审计回放页**:`docs/examples/audit-sample.html`(本包),或任何 WAL 一条命令
   生成:`cargo run -p wanning-demo -- --export-audit <wal> --out <html>`。

## 脱敏自查记录(2026-09-02,W-33,样本 = audit-sample.html)

来源:`--scenario full-loop-mock` 的 WAL(脚本意图 + mock 渠道,零网络),导出
命令真实跑出(链尾 0x30ef89232fbfcc89,回放对账两遍 hash 一致 0xa04f33df28ec4b45)。

| 检查项 | 结果 |
|---|---|
| 真实姓名 | 无。owner=`所有者`(占位代号),agent=`claude-code`(平台名,非个人) |
| 手机号/邮箱 | 无。页内长数字串逐条核对均为完整性链 19–20 位链值与 16 位链尾 hex 的片段(grep 上下文逐条确认),非电话号码 |
| 真实商户/订单号 | 无。merchant=`jd:shop-1/2/3`(mock 占位);full-loop-mock 零出网,无真实订单 |
| 金额 | mock 脚本固定值(500/900/200 分),非真实消费 |
| 本机路径/账号名 | 无。样本 WAL 复制到仓库内中性路径后导出,页内路径不含 Windows 账号名/临时目录(第一版导出含 `C:\Users\<用户名>\…` 临时路径,已用中性路径重导出替换——自查抓出来的,记录在案) |
| 零外链零 JS | 通过。grep `<script`/`<link`/`src="http` 零命中 |
| 委托 id / nonce 作用域 | `d1` / `agent:claude-code`,demo 惯用占位,无真实账户标识 |

## ci.yml 语法校验(顺带项,W-33)

`.github/workflows/ci.yml`(W-01 骨架:fmt + clippy + test)用 Python
`yaml.safe_load` 解析通过(jobs=['check'],steps=6)。

**诚实边界**:本仓无 remote,CI 无法实测跑通——只做了静态语法校验,remote 接上
后第一次 push 应人工核对 Actions 结果;workflow 的 `on: push/pull_request` 与
三步命令同本机门禁口径(fmt --check / clippy -D warnings / test --workspace),
行为差异(如 Actions 环境无预编译缓存)待实测。
