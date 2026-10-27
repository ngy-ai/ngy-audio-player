//! `UserChoice` 哈希：复刻 Windows 对「用户已经选过默认程序」这条记录的校验。
//!
//! 纯粹的计算，不读写注册表 —— 写键的部分在 [`super::defaults`]。
//! 移植自社区逆向实现（PS-SFTA，MIT），测试里拿参考实现算出的向量逐位比对。

use std::time::{SystemTime, UNIX_EPOCH};

use super::plan::{PROG_ID, USER_EXPERIENCE};

/// 「直接设为默认」的结果：成功几个、失败几个（失败带原因，界面上只显示第一条）。
#[derive(Debug)]
pub struct ForceReport {
    pub applied: usize,
    pub failures: Vec<String>,
}

/// 当前时间压成参与哈希的 16 位十六进制：FILETIME 的高 32 位 + 低 32 位。
///
/// **必须截到分钟**：Windows 用 `UserChoice` 键自身的写入时间（分钟级）重算哈希来校验，
/// 所以「算哈希」和「写键」必须落在同一分钟里，否则整条记录被判无效。
pub(super) fn timestamp_hex(now: SystemTime) -> String {
    let seconds = now
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    // FILETIME 是自 1601-01-01 起的 100ns 计数；Unix 纪元与它的差是 11644473600 秒。
    let filetime = (seconds / 60 + 11_644_473_600) * 10_000_000;
    format!("{:08x}{:08x}", filetime >> 32, filetime & 0xFFFF_FFFF)
}

/// 哈希的输入串：扩展名 + 用户 SID + ProgId + 时间戳 + 体验字符串，全部小写、无分隔符。
pub(super) fn hash_input(ext: &str, sid: &str, timestamp: &str) -> String {
    format!(
        ".{ext}{sid}{}{timestamp}{USER_EXPERIENCE}",
        PROG_ID.to_lowercase()
    )
    .to_lowercase()
}

/// 复刻 Windows 对 `UserChoice` 的哈希校验：MD5 之后还要走两轮带魔数的 32 位混算，
/// 再把两组结果交叉异或、取 8 字节做 Base64（12 个字符，末尾带 `=`）。
///
/// 所有中间量都是 32 位、按补码回绕；右移必须按无符号数做（见下面的 `shr`）。
pub(super) fn user_choice_hash(base_info: &str) -> String {
    // UTF-16LE，再补两个 0 字节（等价于结尾的 NUL）。
    let mut buffer: Vec<u8> = Vec::with_capacity(base_info.len() * 2 + 2);
    for unit in base_info.encode_utf16() {
        buffer.extend_from_slice(&unit.to_le_bytes());
    }
    buffer.push(0);
    buffer.push(0);

    use md5::Digest as _;
    let digest = md5::Md5::digest(&buffer);

    // 轮数：长度（字节，含结尾）先折成 PS-SFTA 里的 `length`，再折成循环次数。
    let length_base = buffer.len() as i32;
    let length = i32::from(length_base & 4 == 0) + (length_base >> 2) - 1;
    if length <= 1 {
        return String::new();
    }
    let rounds = ((length - 2) >> 1) + 1;

    let word = |at: usize| -> i32 {
        let mut bytes = [0u8; 4];
        for (offset, slot) in bytes.iter_mut().enumerate() {
            if let Some(byte) = buffer.get(at + offset) {
                *slot = *byte;
            }
        }
        i32::from_le_bytes(bytes)
    };
    let digest_word = |at: usize| -> i32 {
        i32::from_le_bytes([digest[at], digest[at + 1], digest[at + 2], digest[at + 3]])
    };

    // 右移是**无符号**的：Windows 内部按 32 位无符号数移位，负数不做符号扩展。
    // 这一点搞错（写成 `i32 >> n`）会算出一个格式正确、但系统永远不认的哈希。
    let shr = |value: i32, bits: u32| ((value as u32) >> bits) as i32;

    // 第一轮：两个魔数各带一个加法常量。
    let md51 = (digest_word(0) | 1).wrapping_add(0x69FB_0000u32 as i32);
    let md52 = (digest_word(4) | 1).wrapping_add(0x13DB_0000u32 as i32);
    let (mut cache, mut out1, mut out2) = (0i32, 0i32, 0i32);
    let mut at = 0usize;
    for _ in 0..rounds {
        let r0 = word(at).wrapping_add(out1);
        let r1 = word(at + 4);
        at += 8;
        let r2a = r0
            .wrapping_mul(md51)
            .wrapping_sub((0x10FA_9605u32 as i32).wrapping_mul(shr(r0, 16)));
        let r2b = (0x79F8_A395u32 as i32)
            .wrapping_mul(r2a)
            .wrapping_add((0x689B_6B9Fu32 as i32).wrapping_mul(shr(r2a, 16)));
        let r3 = (0xEA97_0001u32 as i32)
            .wrapping_mul(r2b)
            .wrapping_sub((0x3C10_1569u32 as i32).wrapping_mul(shr(r2b, 16)));
        let r4 = r3.wrapping_add(r1);
        let r5 = cache.wrapping_add(r3);
        let r6a = r4
            .wrapping_mul(md52)
            .wrapping_sub((0x3CE8_EC25u32 as i32).wrapping_mul(shr(r4, 16)));
        let r6b = (0x59C3_AF2Du32 as i32)
            .wrapping_mul(r6a)
            .wrapping_sub((0x2232_E0F1u32 as i32).wrapping_mul(shr(r6a, 16)));
        out1 = (0x1EC9_0001u32 as i32)
            .wrapping_mul(r6b)
            .wrapping_add((0x35BD_1EC9u32 as i32).wrapping_mul(shr(r6b, 16)));
        out2 = r5.wrapping_add(out1);
        cache = out2;
    }
    let (first1, first2) = (out1, out2);

    // 第二轮：同样的结构，但不带加法常量。
    let md51 = digest_word(0) | 1;
    let md52 = digest_word(4) | 1;
    let (mut cache, mut out1, mut out2) = (0i32, 0i32, 0i32);
    let mut at = 0usize;
    for _ in 0..rounds {
        let r0 = word(at).wrapping_add(out1);
        at += 8;
        let r1a = r0.wrapping_mul(md51);
        let r1b = (0xB111_0000u32 as i32)
            .wrapping_mul(r1a)
            .wrapping_sub((0x3067_4EEFu32 as i32).wrapping_mul(shr(r1a, 16)));
        let r2a = (0x5B9F_0000u32 as i32)
            .wrapping_mul(r1b)
            .wrapping_sub((0x78F7_A461u32 as i32).wrapping_mul(shr(r1b, 16)));
        let r2b = (0x12CE_B96Du32 as i32)
            .wrapping_mul(shr(r2a, 16))
            .wrapping_sub((0x4693_0000u32 as i32).wrapping_mul(r2a));
        let r3 = (0x1D83_0000u32 as i32)
            .wrapping_mul(r2b)
            .wrapping_add((0x257E_1D83u32 as i32).wrapping_mul(shr(r2b, 16)));
        let r4a = md52.wrapping_mul(r3.wrapping_add(word(at - 4)));
        let r4b = (0x16F5_0000u32 as i32)
            .wrapping_mul(r4a)
            .wrapping_sub((0x5D8B_E90Bu32 as i32).wrapping_mul(shr(r4a, 16)));
        let r5a = (0x96FF_0000u32 as i32)
            .wrapping_mul(r4b)
            .wrapping_sub((0x2C7C_6901u32 as i32).wrapping_mul(shr(r4b, 16)));
        let r5b = (0x2B89_0000u32 as i32)
            .wrapping_mul(r5a)
            .wrapping_add((0x7C93_2B89u32 as i32).wrapping_mul(shr(r5a, 16)));
        out1 = (0x9F69_0000u32 as i32)
            .wrapping_mul(r5b)
            .wrapping_sub((0x405B_6097u32 as i32).wrapping_mul(shr(r5b, 16)));
        out2 = out1.wrapping_add(cache).wrapping_add(r3);
        cache = out2;
    }

    let mut raw = [0u8; 8];
    raw[..4].copy_from_slice(&(first1 ^ out1).to_le_bytes());
    raw[4..].copy_from_slice(&(first2 ^ out2).to_le_bytes());
    base64(&raw)
}

/// 标准 Base64（带 `=` 填充）。只为 8 字节的输出写一份，不引依赖。
pub(super) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b1 = u32::from(chunk[0]);
        let b2 = chunk.get(1).copied().map(u32::from).unwrap_or(0);
        let b3 = chunk.get(2).copied().map(u32::from).unwrap_or(0);
        let triple = (b1 << 16) | (b2 << 8) | b3;
        out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 63] as char
        } else {
            '='
        });
    }
    out
}
