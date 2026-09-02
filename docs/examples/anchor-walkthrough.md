# 锚点签验走查(老板视角,W-33)

> 完整性链(W-21)有两个本地验不住的盲区:**只改最后一行内容**(无后继行引用)
> 与**整体截尾**(余下前缀自成合法链)。外部锚点是兜底。本页是老板视角的两条
> 命令走查;语义与诚实边界全文见 `README.md` 锚点节。

## 第 0 步:造一份样账(演示用;真实场景跳过这步)

```bash
cargo run -p wanning-demo -- --scenario full-loop-mock   # 输出末尾打印 WAL 路径
```

## v1(HMAC,只有老板能验)——两条命令

```bash
# 签:老板用自己的密钥(32 字节 = 64 位十六进制文件,绝不入仓)锚住前 N 行
cargo run -p wanning-demo -- --anchor-sign <审计文件.jsonl> --key key.hex --out anchor.json

# 验:改尾行/截尾当场 exit 1;锚定后合法追加不影响通过(前缀锚语义)
cargo run -p wanning-demo -- --anchor-verify <审计文件.jsonl> --anchor anchor.json --key key.hex
```

## v2(ed25519,第三方零密钥可验,W-31)——三条命令

```bash
# 签:老板用自己的种子(32 字节 ed25519,纪律同 --key)
cargo run -p wanning-demo -- --anchor-sign-v2 <审计文件.jsonl> --seed seed.hex --out anchor.json

# 验(老板或任何第三方,零密钥文件;先 cargo build -p wanning-demo)
cargo run -p wanning-demo --bin wanning-anchor-verify -- \
  --anchor anchor.json --wal <审计文件.jsonl>

# 验(钉定带外核对过的公钥:换公钥重签当场 fail-closed)
cargo run -p wanning-demo --bin wanning-anchor-verify -- \
  --anchor anchor.json --wal <审计文件.jsonl> \
  --expect-key <公钥hex>          # 公钥印在签出回执里
```

- 被签载荷 `WANNING-ANCHOR-v2` 含 `public_key=` 行:只换公钥不改签名,验签现形。
- **诚实边界**:签名只证明「持对应私钥者签的」,不证明「持钥者是老板」——不钉定
  期望公钥时,换钥重签的锚点内部自洽、验得过(回执照样打印提示);身份绑定在带外。

## 眼见为实(30 秒)

```bash
# ① 锚定当前账
cargo run -p wanning-demo -- --anchor-sign-v2 <审计文件.jsonl> --seed seed.hex --out anchor.json

# ② 改最后一行的 memo(完整性链抓不住的盲区)存成副本
# ③ 用锚点验副本:exit 1,报「前 N 行内容与锚点不符——被锚定的部分在锚定后被改过」
cargo run -p wanning-demo --bin wanning-anchor-verify -- --anchor anchor.json --wal 副本.jsonl
```

完整六步实测记录在档(含截尾/换钥重签/伪造字段的 exit code 与报错原文)。

## 保管纪律(唯一比命令重要的东西)

- 密钥/种子**绝不入仓、绝不在任何 Wanning 进程手里**(agent 能签就能伪造锚点,
  所以 MCP 工具面永不提供锚点能力)。
- 锚点文件**与 WAL 分开存放、离机备份**——锚点和账本放同一处、都能被写进程
  改到,锚点就成了自说自话。
