# SDK 嵌入五步(W-33)

> 进程内嵌入(Android/自有 app 宿主用这条路径,不走 MCP stdio)。**本页不复制代码**:代码的权威出处是
> `crates/wanning-sdk/src/lib.rs` 模块文档里的 doctest(`cargo test -p wanning-sdk
> --doc` 真实跑过)与可运行示例 `cargo run -p wanning-sdk --example embed`。
> 四条硬语义的类型强制说明见该模块文档,此处只给走查顺序。

## 第 1 步:加依赖

```toml
[dependencies]
wanning-sdk = { path = "../wanning-sdk" }   # 或按平台 wrapper(未来)
```

SDK 零 serde 依赖、零网络、零支付通道——闸面不是通道。

## 第 2 步:开闸(`Wanning::open`,唯一入口)

```rust
let mut gate = Wanning::open(&wal_path)?;
```

- **必带 WAL**:没有审计不服务。
- **开机必续放**:已有旧账先回放对账,坏账 fail-closed 拒启——不回放的变体
  在 SDK 类型上不存在(W-17 nonce 洗白/撤销复活 bug 在这一面结构性不可复现)。
- 同一 WAL 第二个写句柄 fail-closed(`CoreError::WalLocked`,单写者锁 W-18)。

## 第 3 步:注册委托(`authorize`)

`Delegation::new(id, owner, agent, budget_cap_cents, valid_from, valid_until,
nonce_scope)`——字段语义见 `crates/wanning-core/src/delegation.rs`;预算单位
**分**,u64,全程禁浮点。

## 第 4 步:判定(`decide`)

```rust
let verdict = gate.decide("d1", SpendRequest::new(500, "jd:shop-1", "grocery", "午饭"))?;
```

- `SpendRequest` **没有** delegation_id/nonce 字段:委托 id 宿主显式给,nonce 由
  闸按作用域单调注入(拒绝不耗号/跨委托共享作用域不撞号/跨重启接续)。
- 每笔判定(放行与拒绝)write-ahead 落审计。
- 金额 0/商户空白等非法意图会得到**判过的业务拒绝**(落审计),不是 Err——
  宿主传错委托 id 才是 Err(嵌入方 bug,零审计噪音)。

## 第 5 步:kill switch 与自证(`revoke` / `self_check`)

- `gate.revoke("d1")`:单向,重复撤销落审计不报错;撤销后永不允许。
- `gate.self_check()`:验链 + 回放对账,行数/链尾/状态指纹三条独立口径全对上
  才发回执,任一不过 fail-closed——不可信的自证比不自证更危险。

## 验证你嵌对了

```bash
cargo run -p wanning-sdk --example embed   # 全离线可运行示例,回执含 nonce 序列与链尾
cargo test -p wanning-sdk --doc            # doctest 真实跑通五步
```

九条嵌入契约测试(`crates/wanning-sdk/tests/`)锁死上述每条语义——宿主侧不需要
(也不应该)自己再发明「续放/注入/自证」的逻辑。
