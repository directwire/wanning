//! SHA-256(FIPS 180-4)——零依赖手写,老板侧审计锚点(W-23)的底层哈希。
//!
//! **为什么手写**:W-21 的完整性链用的是 FNV-1a64——它是确定性对账用的非密码学
//! 哈希,64 位空间谈不上抗碰撞;锚点要经得起「写进程就是 agent」这个对手方,
//! 必须用密码学哈希。本仓依赖刻意最小(serde/serde_json/ureq),为一条哈希引入
//! 一棵加密依赖树不值得,SHA-256 是规范唯一、测试向量齐全(FIPS 180-4 示例向量
//! + RFC 6234 + 本机 .NET oracle 交叉核验)的标准件,手写并全向量测试是更诚实的路。
//!
//! 参考:FIPS 180-4 <https://nvlpubs.nist.gov/nistpubs/FIPS/NIST.FIPS.180-4.pdf>;
//! RFC 6234(测试向量)<https://datatracker.ietf.org/doc/html/rfc6234>。
//! 本仓测试向量另经本机 .NET `SHA256`(独立实现)逐条交叉核验(W-23 取证)。

/// 计算 SHA-256,返回 32 字节摘要。
pub fn sha256(data: &[u8]) -> [u8; 32] {
    // 初始哈希值(前 8 个素数平方根的小数部分前 32 位,FIPS 180-4 §5.3.3)。
    let mut h: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];

    // 填充(FIPS 180-4 §5.1):追加 0x80,补 0 到 ≡56 (mod 64),再接 64 位大端比特长。
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = Vec::with_capacity(data.len() + 72);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    // 轮常数(前 64 个素数立方根的小数部分前 32 位,FIPS 180-4 §4.2.3)。
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    for block in msg.chunks_exact(64) {
        // 消息调度:前 16 字取自块,其余按 σ0/σ1 展开(FIPS 180-4 §6.2.2)。
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        // 压缩(FIPS 180-4 §6.2.2)。
        let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
        let (mut e, mut f, mut g, mut hh) = (h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (chunk, word) in out.chunks_exact_mut(4).zip(h.iter()) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// 小写十六进制(锚点文件/对账输出统一用它,可读且可肉眼比对)。
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试向量(W-23 取证,真实运行输出):本机 .NET `SHA256` 独立实现计算,
    /// 与 FIPS 180-4 示例 / RFC 6234 一致的条目就地标注。覆盖 padding 边界
    /// (55/56/63/64/65 字节)与多块(111/128/192/10⁶)。
    fn oracle_vector(input: &[u8], expected: &str) {
        let actual = hex(&sha256(input));
        assert_eq!(actual, expected, "输入 {} 字节", input.len());
    }

    #[test]
    fn fips180_4_and_rfc6234_vectors() {
        oracle_vector(
            b"",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        );
        oracle_vector(
            b"abc",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
    }

    #[test]
    fn rfc6234_one_million_a() {
        // RFC 6234 测试用例:10⁶ 个 'a'(用 Vec,避免 1MB 栈数组)。
        let input = vec![b'a'; 1_000_000];
        oracle_vector(
            &input,
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
        );
    }

    #[test]
    fn padding_boundaries() {
        // 55 = 恰好 0x80 + 8 字节长度装进同一块;56 起必须再多一块。
        oracle_vector(
            &[b'a'; 55],
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318",
        );
        oracle_vector(
            &[b'a'; 56],
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a",
        );
        oracle_vector(
            &[b'a'; 63],
            "7d3e74a05d7db15bce4ad9ec0658ea98e3f06eeecf16b4c6fff2da457ddc2f34",
        );
        oracle_vector(
            &[b'a'; 64],
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb",
        );
        oracle_vector(
            &[b'a'; 65],
            "635361c48bb9eab14198e76ea8ab7f1a41685d6ad62aa9146d301d4f17eb0ae0",
        );
    }

    #[test]
    fn multi_block_lengths() {
        oracle_vector(
            &[b'a'; 111],
            "6374f73208854473827f6f6a3f43b1f53eaa3b82c21c1a6d69a2110b2a79baad",
        );
        oracle_vector(
            &[b'a'; 128],
            "6836cf13bac400e9105071cd6af47084dfacad4e5e302c94bfed24e013afb73e",
        );
        oracle_vector(
            &[b'a'; 192],
            "7cee24628d290c16183532716cc5a8a889bc951b4b0a1507c32b8e29cee01052",
        );
    }

    #[test]
    fn all_byte_values() {
        let input: Vec<u8> = (0..=255u8).collect();
        oracle_vector(
            &input,
            "40aff2e9d2d8922e47afd4648e6967497158785fbd1da870e7110266bf944880",
        );
    }

    #[test]
    fn hex_is_lowercase_two_chars_per_byte() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00, 0x0a, 0xff]), "000aff");
    }
}
