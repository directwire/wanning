# Wanning · 给 AI 的支付闸

AI 编码工具越来越能干,也离你的钱包越来越近。Wanning 在 agent 和支付之间加了一道闸:**预算内才放行,超额即拒,一键撤销,全程留痕。**

- 真插五个平台:Claude Code / Codex / Kimi Code / Trae / WorkBuddy(MCP)
- 每笔判定落完整性链,篡改一行当场报错;ed25519 锚点,第三方**零密钥可验**
- 闸不碰钱:授权与账本在仓里,结算走持牌通道;真实消费路径 fail-closed 锁死,钥匙不齐永远拒绝执行

> 项目早期,全链离线 mock 实测(310 个测试全绿,零真实消费);中文文档,Apache-2.0。

---

## English (summary)

**Wanning is a payment gate for AI agents.** It sits between an agent and its spending: every spending intent must pass an authorization decision — inside budget → allowed, over budget → denied, one-key revoke, and every decision lands in a tamper-evident audit chain.

- **MCP server** — plugs into Claude Code, Codex, Kimi Code, Trae, and WorkBuddy; a config generator (`wanning-init`) wires it up.
- **Tamper-evident ledger** — hash-chained WAL; modifying any record fails loudly on read. Anchors are ed25519-signed, so a third party can verify with zero secrets.
- **Never touches money** — the gate holds authorization and the ledger only; settlement runs through licensed channels. The real-spend path is fail-closed: without every required key present, it refuses to run.
- **6 Rust crates, 310 tests green, all offline / mock, zero real transactions so far.** Apache-2.0. Docs in Chinese.

---

## 四条硬语义

| 语义 | 一句话 |
|---|---|
| 意图先过闸 | agent 的每笔消费意图,先经闸判定,才有资格动钱 |
| 无审计不服务 | 判定必须落审计链;落不了账的判定=不执行 |
| 预算即扣额 | 放行即扣减,撤销不可逆;账本序号不因拒绝而消耗 |
| 闸零网络零消费 | 闸本身不发网络请求、不经手资金;结算永远在持牌通道 |

## 平台矩阵

| 平台 | 接入形态 | 状态 |
|---|---|---|
| Claude Code | `.mcp.json`(MCP stdio) | ✅ 实测通过 |
| Kimi Code | `mcp.json` | ✅ 隔离实测(mock LLM 三轮往返落账) |
| Codex | 配置生成 | ✅ 配置直核;登录态验证待环境 |
| Trae | `.trae/mcp.json` | ✅ 配置直核 |
| WorkBuddy | `.workbuddy/mcp.json` | ✅ 按官方文档直核;桌面端实测待环境 |
| DeepSeek Harness | Cordis overlay(`dsh-mcp-client`) | 🚧 调研完成,接入排期 |

## 通道与 fail-closed

京东 / 支付宝 / 微信 / 美团四通道 adapter 已备(mock 契约)。**当前仓库不含任何真实消费能力**:真实消费需同时满足三道门——`WANNING_ALLOW_REAL_SPEND` 显式护栏 + 通道端点环境变量齐备 + 通道真实接线(仓库内不存在)。缺任何一项,闸在执行前直接拒绝。

## 快速上手

```bash
git clone https://github.com/directwire/wanning && cd wanning
cargo build --release          # 需稳定版 Rust

# 1. 生成你所用工具的插件配置(默认只打印,--out 才落盘,绝不覆盖已有文件)
wanning-init --platform claude-code

# 2. 按输出把配置粘进你的工具,重启后你会看到两个新工具:
#    wanning_gate_evaluate  —— 消费意图过闸
#    wanning_audit_tail     —— 追加读审计链

# 3. 验证:给一个超额意图 → 应得 DENY,且账本序号不消耗
```

第三方独立验证(可选):

```bash
wanning-anchor-verify --wal <path> --anchor <anchor-file> --expect-key <pubkey>
```

## 架构一览

```
agent(Claude Code / Codex / Kimi / Trae / WorkBuddy / dsh …)
   │  MCP stdio
   ▼
wanning-mcp ──► wanning-core(闸判定 + WAL 账本 + 完整性链 + ed25519 锚点)
   │                    │
   │                    └── 审计回放(HTML)/ 第三方锚点验证
   ▼(仅当三道门全开)
wanning-demo 通道 adapter(mock)──► 持牌结算通道
```

## 文档

- [合规红线](docs/compliance-redlines.md) — 本项目执行无豁免的铁律
- [基准](docs/benchmarks.md) · [示例](docs/examples/README.md) · [平台接入](docs/plugins/)
- [ROADMAP](ROADMAP.md)

## License

Apache-2.0
