//! # Wanning 闸核心(wanning-core)
//!
//! **中国合规场景下,给任何 agent 插的支付闸 —— 意图层标准件。**
//!
//! 闸坐在「agent 的消费意图」和「支付管道(支付宝免密/微信支付)」之间,
//! 只做四件事,不碰资金流、不参与结算、不持有任何资金:
//!
//! 1. **预算**——用户一次授权设总预算(单位:人民币**分**,u64,全程禁浮点),
//!    闸对每笔意图做原子扣减,超额即拒。
//! 2. **撤销**——kill switch:撤销即永久拒绝该委托的一切后续意图(即时生效)。
//! 3. **重放**——同一 nonce 在同一作用域内只允许成功消费一次,重放即拒。
//! 4. **审计**——每一条决策(放行/拒绝)append-only 落 WAL,可回放重建账本状态。
//!
//! 语义对齐 Mist 的 DSA 授权层(预算扣减 / 撤销即时 / 重放 nonce / fail-closed),
//! 但**刻意不依赖 `mist-core` crate**:Mist 是链上/ZK 形态(金额是链上单位,
//! 依赖 k256/ed25519/ZK 电路),Wanning 是人民币分语义的链下闸,强行复用会拖进
//! 一整棵加密依赖树却用不到任何代码。各语义点在代码里以 `// 语义对齐 mist-core`
//! 注释标注,便于将来对账。
//!
//! 模块地图(闸判定语义见 [`gate`]):
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`delegation`] | 授权(用户 → agent):预算上限 / 有效期 / nonce 作用域 / 支出策略 |
//! | [`intent`] | 消费意图(agent 发起的一笔待判定消费) |
//! | [`gate`] | 闸判定面:按 fail-closed 顺序检查并原子扣减 |
//! | [`policy`] | 支出策略(W-27):速率 / 类目预算 / 商户名单 / 禁止时段 |
//! | [`error`] | 错误分层:业务拒绝 ≠ 错误;`CoreError` 只管 API 误用/状态被破坏 |
//! | [`budget`] | 预算账本:分/元语义,remaining ≥ 0 不变量 |
//! | [`revocation`] | 撤销集合(kill switch) |
//! | [`replay`] | nonce 防重放登记 |
//! | [`wal`] | 审计日志(append-only JSONL)+ 回放重建 |
//! | [`state`] | 运行时状态:闸 + 审计(write-ahead)+ 时钟;`replay` 重建 |
//! | [`clock`] | 可注入时钟(测试不 sleep,过期语义可控) |
//! | [`sha256`] | SHA-256(FIPS 180-4,零依赖手写;W-23 锚点的底层哈希) |
//! | [`anchor`] | 审计锚点(W-23):老板侧密钥锚住链尾,堵住 W-21 已知边界 |
//!
//! 合规边界见 `docs/compliance-redlines.md`(无豁免):本 crate 是意图层软件,
//! 资金路径永远是「用户实名支付工具 → 商户」,没有任何我们的位置。

pub mod anchor;
pub mod budget;
pub mod clock;
pub mod delegation;
pub mod error;
pub mod gate;
pub mod intent;
pub mod policy;
pub mod replay;
pub mod revocation;
pub mod sha256;
pub mod state;
pub mod wal;
