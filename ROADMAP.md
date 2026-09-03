# ROADMAP

> 只写事实与方向,不写日期承诺。当前进展见 README 平台矩阵(诚实标签)。

## 已完成

- 闸核心语义:预算判定 / 超额拒绝 / 撤销(不可逆)/ 防重放,四条硬语义类型强制
- WAL 账本:hash 完整性链,篡改任意记录读回当场报错;单写者锁(第二进程 fail-closed)
- 审计锚点:所有者侧 HMAC(v1)+ ed25519 第三方零密钥可验(v2,`--expect-key` 带外钉定)
- MCP server(stdio):Claude Code 真插实测;Kimi Code / Codex / OpenClaw / Hermes
  隔离实测(真宿主二进制 + mock 模型往返,判定落账、链连续)
- 插件配置生成器:claude-code / codex / kimi / trae / workbuddy / deepseek-harness /
  hermes / openclaw 八平台(`wanning-init`)
- 产品化三件:开箱默认值(默认账本路径 + 默认预算策略,零配置可用)/ 统一 CLI 入口
  `wanning init / audit / ui / demo / anchor-verify` / 本地只读仪表盘 `wanning ui`(仅 127.0.0.1)
- 装完即用三命令流:`wanning init --install` 直写宿主配置(merge 只动 wanning 条目,
  写前备份,`--dry-run` 预览;codex 拒装给人工指引)+ `wanning doctor` 挂载面体检
  (二进制/配置语义/真握手/账本目录可写/真实消费就绪度/版本一致性,每项 ❌ 带
  修复命令;真握手用隔离临时账本)
- 通道 adapter(京东/支付宝/微信/美团):mock 契约层备妥
- 性能基准、审计回放 HTML、嵌入 SDK、CI 三步门禁(fmt/clippy/test)

## 下一步

- 真实通道接线:授权与账本在仓内,结算留持牌通道;接线动作 fail-closed,
  需要的钥匙(平台账户、密钥)备齐前永远拒绝执行真实消费
- 预算策略层深化(速率 / 类目 / 商户 / 时段之上继续加语义)
- 平台矩阵实测收尾:Trae / WorkBuddy 桌面端就位后的真插复证
- 协议层:与通道方谈"授权层标准件"的采纳
