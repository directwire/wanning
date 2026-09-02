# ROADMAP

> 只写事实与方向,不写日期承诺。当前进展见 README 平台矩阵(诚实标签)。

## 已完成

- 闸核心语义:预算判定 / 超额拒绝 / 撤销(不可逆)/ 防重放,四条硬语义类型强制
- WAL 账本:hash 完整性链,篡改任意记录读回当场报错;单写者锁(第二进程 fail-closed)
- 审计锚点:老板侧 HMAC(v1)+ ed25519 第三方零密钥可验(v2,`--expect-key` 带外钉定)
- MCP server(stdio):接入 Claude Code 真插实测;Kimi Code 隔离实测(mock LLM 往返落账)
- 插件配置生成器:claude-code / codex / kimi / trae / workbuddy 五平台
- 通道 adapter(京东/支付宝/微信/美团):mock 契约层备妥
- 性能基准、审计回放 HTML、嵌入 SDK、CI 三步门禁(fmt/clippy/test,310 测试)

## 进行中

- 开箱默认值:默认 WAL 路径与默认预算策略(零配置可用)
- CLI 统一入口:`wanning init / audit / ui / demo / anchor-verify`
- 本地只读仪表盘:`wanning ui`(127.0.0.1)
- 发布件:crates.io / 预编译二进制 / README 安装页

## 下一步

- DeepSeek Harness 接入(Cordis overlay)
- 真实通道接线:授权与账本在仓内,结算留持牌通道;接线动作 fail-closed,
  需要的钥匙(平台账户、密钥)备齐前永远拒绝执行真实消费
- 预算策略层扩展(速率 / 类目 / 商户 / 时段)深化
- 协议层:与通道方谈"授权层标准件"的采纳
