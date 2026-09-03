# Wanning

**中国合规场景下,给任何 agent 插的支付闸。** 不是 agent,不是平台,不是钱包——
是坐在「agent 消费意图」和「支付宝/微信支付管道」之间的**授权标准件**:
预算、撤销、重放保护、全程审计。

技术底座:复用 [Mist](https://github.com/changshenhan/mist) 的 DSA 授权层语义
(`mist-core`:预算账本 / 撤销 / 重放 nonce / WAL 审计)。

## 它在哪一层

```
用户(钱的主人)── 一次授权(设预算/绑免密)
   ▼
Agent 建造者(Claude Code / Kimi / Trae / WorkBuddy / ANAI / 任何框架)
   ▼
【Wanning 闸:预算 / 策略 / 撤销 / 重放 / 审计】  ← 全部就在这一道
   ▼  通过 → 触发支付
支付宝免密 / 微信支付 / (预留:x402 链上)        ← 持牌管道,不知道我们存在
   ▼
商户(京东 / 美团 / …)                          ← 零感知
```

- 授权不止预算上限:同一道闸还管**速率限制 / 类目预算 / 商户名单 / 禁止时段**
  四个确定性策略维度(挂在委托上,缺省策略 = 行为不变;`wanning-core::policy`)

- 闸插在 **agent 层**(MCP server / SDK),管道留给持牌层,我们卖
  「任何 agent × 任何管道」之间的中立标准件
- 支付宝/微信**不集成我们**——免密协议是用户跟平台签的,闸通过的支付
  对管道就是一笔普通支付

## 合规红线(全文见 `docs/compliance-redlines.md`,执行无豁免)

1. 禁止任何资金沉淀/归集/转付(二清 = 刑事红线);钱永远用户直付商户
2. 只用本人账户小额真实消费;不给第三方代理支付
3. 必须走官方开放平台 API;禁止模拟点击/爬虫下单
4. 不碰数字人民币程序化接入;不碰虚拟货币结算
5. 密钥一律仓库外 env 文件;对外只谈架构不披露通道细节

## 安装(W-43c;三条路,按你手里的条件选)

**路线 1 · crates.io(cargo install)**——闸的统一入口一个包:

```bash
cargo install wanning-cli        # 装 wanning 一个二进制,覆盖 init/audit/ui/demo/anchor-verify
```

诚实状态:七个 crate 的 **0.1.1 已上 crates.io**(2026-09-03,含统一 CLI 入口
`wanning-cli`;0.1.0 为 2026-09-02 五 crate 首发版)。

**路线 2 · Release 下载(预编译二进制)**——不装 Rust 工具链:

```bash
# 仓库公开 + push v* tag 后,GitHub Actions 自动出三平台二进制 + SHA256 清单:
#   https://github.com/directwire/wanning/releases
# x86_64-pc-windows-msvc / x86_64-unknown-linux-gnu / aarch64-apple-darwin(.github/workflows/release.yml)
```

诚实状态:v0.1.0 release 在本仓真跑绿过(三平台二进制 + SHA256SUMS);
v0.1.1 tag push 后同 workflow 出新一版,以实际产物为准。

**路线 3 · 源码构建(今天就能用)**:

```bash
git clone <repo-url> wanning && cd wanning
cargo install --path crates/wanning-cli      # wanning 二进制进 $CARGO_HOME/bin
wanning --version
```

**装完第一步**(北极星:进仓到闸口在跑 ≤10 分钟):

```bash
wanning init --platform claude-code   # 或 codex / kimi / trae / workbuddy / deepseek-harness / openclaw / hermes
# 输出即贴进对应平台的 MCP 配置;写实路径零占位符,默认账本 ~/.wanning/wal.jsonl
# 重启你的编码工具 → 闸的两件工具(wanning_gate_evaluate / wanning_audit_tail)出现
# 让 agent 试一笔超额消费 → 应被拒(reason=over_budget,账本不动)
wanning audit                         # 看账本:行数/判定/链尾/预算台账
wanning ui                            # 本地仪表盘:127.0.0.1 随机端口,零 JS 自动刷新
```

## Quickstart(全离线,零真实消费,5 分钟)

```bash
git clone <repo-url> wanning && cd wanning
cargo test --workspace                       # 全部测试(含 property/回放对账/护栏两路)
cargo run -p wanning-demo -- --scenario four-selling-points   # 四卖点演示(下面就是真实输出)
cargo run -p wanning-demo -- --scenario full-loop-mock        # 全链闭环:闸→京东 mock→支付宝 mock→回调结算(一条命令看全貌)
cargo run -p wanning-bench --release                          # 性能基准:判定/WAL/回放/审计页(基线落 docs/benchmarks.md)
```

**接入路径现成答案**:`docs/examples/`(W-33 对外样板包)——多平台 MCP 配置
(Claude Code 已实测/Trae/Kimi Code CLI W-40 实测)、SDK 嵌入五步、锚点签验走查、
审计样例页(含脱敏自查记录);**插件页八平台齐**(W-41 补齐五页,W-44/W-45 各加一页):
Claude Code `docs/plugins/claude-code.md`(真插实测通过,W-19)、
Codex `docs/plugins/codex.md`(W-35)、Kimi `docs/plugins/kimi.md`(W-40)、
Trae `docs/plugins/trae.md`(机制已核,待 GUI)、
WorkBuddy `docs/plugins/workbuddy.md`(支持性已核,待桌面端)、
DeepSeek Harness `docs/plugins/deepseek-harness.md`(W-44,Cordis overlay patch,
本机 dsh 0.1.0-rc.7 `--dump-config --patch` 实测接受,会话级待所有者放行)、
OpenClaw `docs/plugins/openclaw.md`(W-45 配置面 + W-47 agent 回合,**全链路实测**:
「静默退出」根因查明 + 工具现身 `wanning__*` + allow/replay 落 WAL,真实模型会话待所有者放行)、
Hermes `docs/plugins/hermes.md`(W-45,**全链路实测**:挂载 2/2 工具发现 + one-shot
agent 回合 + 本地 mock LLM → allow 400 落 WAL、同 nonce replay 拒,真实模型会话待所有者放行)。
**配置生成器**(W-36):`cargo run -p wanning-init -- --platform claude-code|codex|kimi|trae|workbuddy|deepseek-harness|openclaw|hermes`
——默认只打印,`--out` 显式写且绝不覆盖已有文件;kimi 分支 W-40 按本机实测修订
(kimi-code 0.39.1 无 `kimi mcp` 子命令,改为生成 `.kimi-code/mcp.json` 内容);
workbuddy 按 W-37 直核官方 MCP-Guide 入矩阵(`.workbuddy/mcp.json`,真插实测待桌面端);
deepseek-harness 按 W-44 入矩阵(Cordis overlay `- insert:` patch,生成内容经真
dsh 二进制 `--dump-config --patch` 组合实测);openclaw / hermes 按 W-45 入矩阵
(两宿主原生支持 MCP:openclaw 产出 `openclaw mcp set` 命令行、hermes 产出
`hermes mcp add` 命令行,均经真宿主隔离实测——hermes 全链路含 agent 回合)。
**W-43a 起写实路径零占位符**:配置直写解析出的 wanning-mcp 绝对路径与审计账本
真实路径(默认 `~/.wanning/wal.jsonl`,Windows `%USERPROFILE%\.wanning`,目录自动创建),
默认预算 1000 分 + 每日 10 笔速率护栏随配置落 args(用户可改),拿到就能用不必手改。

**统一 CLI 入口**(W-43a):`cargo install wanning-cli`(或仓内 `cargo run -p wanning-cli -- …`)
收拢单一入口 `wanning`——`wanning init --platform <名>`(八平台同上,缺 wanning-mcp
时报错给安装指引)、`wanning audit [<账本>] [--out <report.html>]`(读账本汇总:
行数/判定/链尾/预算台账;坏账 fail-closed 拒读;`--out` 同时导出审计回放页)、
`wanning demo --scenario <name>`、`wanning anchor-verify --anchor <a.json> --wal <账本>`
(第三方零密钥验签)——后两个与 `wanning-demo` 走**同一段 lib 实现**,真实消费护栏
W-07 在直通路径原样生效;`wanning ui [--wal <账本>] [--port <端口>]`(W-43b)本地
只读仪表盘:127.0.0.1 默认随机端口、不监听外网,预算台账 / 判定实时滚动 / 一键撤销
(走闸本体落审计),页面零 JS 自动刷新;坏账亮横幅并隐藏全部撤销表单(fail-closed),
跨站三道防护(回环绑定 / Host 校验 / Origin+令牌)。旧 bin 名
(`wanning-demo`/`wanning-init`/`wanning-anchor-verify`)保留一个发行周期作 alias。

`four-selling-points` 是**离线脚本场景**(非模型决策):本地 MockClock + 临时 WAL,
零网络、零真实消费。真实输出(2026-09-02 实跑):

```
【卖点① 预算内放行】(证据:WAL 行 2)
  agent 请求 ¥5.00;闸放行,累计消费 500/1000 分,剩余 500 分。
【卖点② 超额拒绝】(证据:WAL 行 3)
  agent 再请求 ¥9.00,累计将达 ¥14.00 > 上限 ¥10.00;闸拒绝(reason=over_budget),
  账本不动、nonce 不耗——拒绝只是拒绝,不产生任何副作用。
【卖点③ 撤销后拒绝(kill switch)】(证据:WAL 行 4 收权 / 行 5 拒绝)
  所有者 revoke 委托 d1;此后 agent 再请求 ¥1.00 也被拒(reason=revoked)。
【卖点④ 全程审计导出 + 回放对账】(WAL 共 5 行)
  回放对账:live state_hash=f2caeef13331e869,replay state_hash=f2caeef13331e869,一致
  完整性链:审计逐行成链(seq=物理行号,prev=前行链值),live 链尾=3122bbbc71a1940d,
  读侧重算=3122bbbc71a1940d,一致 —— 改历史行/删行/重排/复制,读回验链当场报错
  已知边界:只改最后一行内容、整体截尾,链抓不住——需外部锚点兜底(已落地 W-23:
  wanning-demo --anchor-sign,所有者侧密钥签出锚点文件,验锚点时当场现形)
```

**真实消费路径(fail-closed,今晚未接线)**:`cargo run -p wanning-demo --
--scenario four-selling-points --dry-run false` 会先过护栏——`WANNING_ALLOW_REAL_SPEND=1`
与 GLM/京东/支付宝密钥不齐即 exit 1 并列出缺什么;全齐还会再撞「未接线」一道门。
密钥只放仓库外 env 文件(红线 5)。

**审计回放页(给人看的 protocol receipt)**:把一份审计日志渲染成自包含 HTML
时间线——每行意图/判定/账本逐笔可见,W-21 完整性链逐行成链(`prev → 本行链值`
肉眼可验);零 JS 零外链,`file://` 离线可开;先验链再回放对账,坏账绝不产出:

```bash
cargo run -p wanning-demo -- --export-audit <审计文件.jsonl> --out audit.html
# 浅/深双主题截图:docs/tasks/w22-audit-replay.png / w22-audit-replay-dark.png
```

**所有者侧审计锚点(W-23)**:W-21 完整性链有两个本地验不住的盲区——**只改最后一行内容**
(无后继行引用)与**整体截尾**(余下前缀自成合法链)。锚点用所有者自己的密钥
(32 字节,64 位十六进制文件,**绝不入仓、绝不在任何 Wanning 进程手里**)把
「前 N 行内容 SHA-256 + 行数 + 链尾」签成锚点文件,另行保管(与 WAL 分开存放):

```bash
cargo run -p wanning-demo -- --anchor-sign <审计文件.jsonl> --key key.hex --out anchor.json
cargo run -p wanning-demo -- --anchor-verify <审计文件.jsonl> --anchor anchor.json --key key.hex
```

- 签之前先对账(验链 + 回放),坏账绝不签;空账拒签;输出原子落盘,失败不碰旧文件
- 验:**先验锚点 MAC,再验完整性链,再比前缀**;改尾行/截尾当场现形(exit 1)
- 锚定后**合法追加的新行不影响通过**(前缀锚语义);锚点文件是普通 JSON,人可读
- 密钥 ≠ MCP 工具面:agent 能签就能伪造锚点,所以 MCP 永不提供锚点能力
- 对称 HMAC 的诚实边界:验锚点也要密钥(没有密钥的「只比内容不比 MAC」刻意不做)

**锚点 v2,ed25519(W-31)——第三方零密钥可验**:HMAC 对称,第三方拿不到所有者
密钥就验不了;v2 用非对称签名,**公钥随锚点走**,第三方拿锚点 + WAL 就能验:

```bash
# 所有者侧签出(种子 = 32 字节 ed25519,纪律同 --key:绝不入仓、绝不在 Wanning 进程手里)
cargo run -p wanning-demo -- --anchor-sign-v2 <审计文件.jsonl> --seed seed.hex --out anchor.json
# 第三方验签(独立 bin,没有 --key 选项;先 cargo build -p wanning-demo)
cargo run -p wanning-demo --bin wanning-anchor-verify -- \
  --anchor anchor.json --wal <审计文件.jsonl> \
  --expect-key <公钥hex>   # 带外核对过的公钥钉定,换钥重签当场 fail-closed
```

- 被签载荷 `WANNING-ANCHOR-v2` 含 `public_key=` 行:只换公钥不改签名,验签当场现形
- 诚实边界如实落测试:签名只证明「持对应私钥者签的」,不证明「持钥者是所有者」——
  不钉定期望公钥时,换钥重签的锚点内部自洽、验得过(回执照样打印「请核对公钥」
  提示);身份绑定在带外(第三方从所有者公开渠道核对公钥),密码学不替你做这一半
- v1 HMAC 保留(向后兼容,v1 文件格式字节不漂移);ed25519 实现用
  `ed25519-dalek`(本仓第一个运行时外部加密依赖,只进 demo 工具面,
  core/闸/MCP/SDK 依赖树零增长——曲线手写不可接受,哈希能手写是因为 spec 短向量密)

## MCP server(P1 前置骨架,已可 stdio 对话)

```bash
cargo run -p wanning-mcp -- --wal /tmp/wanning-audit.jsonl   # --wal 必填:没有审计不服务
```

stdio 上的 MCP server(协议版本 2025-06-18,method/字段/错误码按官方 spec 核对)。
**零网络、零真实消费**;工具面只有两个:`wanning_gate_evaluate`(闸评估,
判定与拒绝都落审计 WAL)、`wanning_audit_tail`(读审计尾部)。
撤销不设工具——那是所有者侧动作,agent 无权自撤销。
审计 WAL 逐行带**完整性链**(`seq`/`prev`):历史行被改/删/重排/复制,重启验链
fail-closed 拒启(实测证据在档(W-21 节))。

**平台接入配置已备好**(P1 真插实测的前置,字段按官方文档直核,详见
`docs/research/mcp-consumption.md`):仓库根 `.mcp.json` 给 Claude Code、
`.trae/mcp.json` 给 Trae,默认审计落在 `<仓库根>/target/mcp-demo.wal`(gitignore)。
同一 WAL 重启**接续旧账**(`WanningState::live_resuming`:回放对账 fail-closed →
续写;nonce 不洗白、撤销不复活),配置契约由
`crates/wanning-mcp/tests/mcp_json_config.rs` 锁定。
同一 WAL 同时只允许**一个活着的写进程**(`Wal::open` 自动持单写者锁):两个平台
并发双挂时第二个 server 拒启(fail-closed)——否则各自内存账本互不知情,预算硬上限
会被合力突破;持锁方正常退出即释放,崩溃留下的孤儿锁按报错删掉即可恢复。

## 嵌入 SDK(P2,进程内门面)

不想走 stdio/MCP 的宿主程序(如 ANAI 执行层),直接把闸嵌进自己进程——
同一个闸,两种接法:

```rust
use wanning_sdk::{Delegation, SpendRequest, Wanning};

let mut gate = Wanning::open(wal_path)?;   // 唯一入口,必带 WAL,开机即回放续放
gate.authorize(Delegation::new("d1", "boss", "claude-code",
    1000, 1000, u64::MAX - 1, "agent:claude-code"))?;    // 预算/有效期/nonce 作用域
let verdict = gate.decide("d1",
    SpendRequest::new(500, "jd:shop-1", "grocery", "午饭"))?;  // nonce 闸注入,判定与拒绝都落审计
gate.revoke("d1")?;                        // kill switch(授权者动作,单向)
gate.self_check()?;                        // 验链+回放对账,三条口径全对上才发回执
```

四条硬语义在 SDK 面是**类型系统强制**,不是调用方纪律:

1. `open` 必回放续放——core 里的 `live`/`with_wal`(不回放,W-17 nonce 洗白/
   撤销复活 bug 的根源)在 SDK 面不存在;
2. `SpendRequest` 根本没有 `delegation_id`/`nonce` 字段——模型越权字段在类型上进不来;
3. 没有「无审计的闸」——open 必带 WAL,每笔判定(放行与拒绝)write-ahead 落审计;
4. 零网络零消费——SDK 面只有判定/撤销/审计读取,支付通道永远在闸的面外。

可运行示例(全离线,零真实消费):`cargo run -p wanning-sdk --example embed`;
嵌入契约测试 `crates/wanning-sdk/tests/embedding.rs` 逐条锁这四条语义。

## 授权协议白皮书(P3 首项,v0.1)

对外谈协议用的一页架构文件:白皮书(在档,签核后随版发布)——
四层分层(授权/账本/结算/受理,受理留给持牌)、闸判定语义(六道门 fail-closed)、
威胁模型(agent 是不可信方,十条攻击逐条对应已落地防御)、与代扣协议义务条款的
逐条对照、诚实边界与发布前自查。只谈分层与语义,不含通道接入细节/账号/密钥。

## 阶段

| 阶段 | 交付 | 状态 |
|---|---|---|
| P0 | CLI 闭环 demo:GLM 决策 + 闸 + 支付宝免密/京东开放平台真实小额下单(四卖点实测) | **离线闭环完成**(四卖点+回放对账+护栏/adapter mock 全测试;**预算策略层落地** W-27:速率/类目/商户/时段四维确定性策略;**性能基线** W-30:判定 ~1.2M/s、WAL 追加 ~0.33M 行/s、回放 ~0.42M 行/s,`docs/benchmarks.md`);真实小额下单待账户开通。**产品化首砖+W-43a/W-43b**:统一
CLI 入口 `wanning`(init/audit/demo/anchor-verify/ui,默认账本 ~/.wanning)、
本地只读仪表盘 `wanning ui` |
| P1 | 闸做成 **MCP server**——Claude Code / Kimi / Trae 等真实平台真插 | 骨架+协议边界加固完成;**Claude Code 真插实测通过**(2026-09-02,W-19);**Codex 配置面免登录实测 + 插件页落地**(W-35,`docs/plugins/codex.md`,生成的 TOML 已实证可启动闸;会话级待 OpenAI 登录);**Kimi Code CLI 挂法+闸往返本机实测**(W-40:0.39.1 无 `kimi mcp` 子命令→`.kimi-code/mcp.json` 挂法,隔离 `KIMI_CODE_HOME` 下真二进制完成 allow/replay/over_budget 三判定落 WAL,模型侧本地 mock,真实模型会话复证待所有者放行烧额度——登录凭证 2026-08-30 已在档,`kimi login` 大概率可省(W-42 修正),`docs/plugins/kimi.md`);**WorkBuddy 调研破冰**(W-37:腾讯 AI 办公工作台,支持 MCP,`.workbuddy/mcp.json`,生成器已入矩阵,真插待桌面端);Trae 实测被 GUI 挡;**DeepSeek Harness 接入**(W-44:Cordis overlay patch 生成器分支 + 插件页,本机 dsh 0.1.0-rc.7 `--dump-config --patch` 实测接受,会话级待所有者放行);**OpenClaw + Hermes 接入**(W-45:两宿主原生支持 MCP,`openclaw mcp set`/`hermes mcp add` 生成器分支 + 插件页,真宿主隔离实测——hermes **全链路**:挂载 2/2 工具发现 + one-shot agent 回合经 `tool_call` 间接层 → allow 400 落 WAL、同 nonce replay 拒;openclaw 配置面 + mock 模型注册绿,agent 回合 **W-47 收口**:静默退出根因查明(缺 `-m`/缺会话选择器/cwd 扫描税),隔离 mock 真回合工具现身 `wanning__*`、allow/replay 落 WAL、链连续);**八平台配置生成器**(`wanning-init`,W-36/W-44/W-45) |
| P2 | SDK + 微信通道 + 对外样板 | **SDK 完成**(W-25);审计回放页(W-22)/所有者侧锚点(W-23,**v2 ed25519 第三方零密钥可验** W-31,独立 bin `wanning-anchor-verify`)/微信调研(W-24)+ **微信 adapter 骨架**(W-27,W-24 直核字段落代码;接入待账户开通);**美团 adapter 契约占位**(W-39,W-38 结论=查不到用户侧免密/代扣 API,管线照建契约体留空);**四渠道×四工具矩阵 mock 备妥**(W-39,矩阵在档);剩对外样板录像 |
| P3 | 标准输出:拿协议谈开放平台/银行 | **白皮书第一稿完成**(2026-09-02,W-26,在档);拿协议谈待所有者 |
