//! Qoder 自定义 Base64（出站请求体的编码格式）。
//!
//! 对照 `pi-desktop/lib/qoder-cn/qoder-base64.ts`，与标准 Base64 只差两步，
//! 但两步都必须逐字节对齐，否则上游解不开 body、直接按「签名不一致 / 参数非法」拒绝：
//!
//!   1. 标准 Base64 切成「尾 / 中 / 首」三段后轮转：`R = S[n-a:] + S[a:n-a] + S[:a]`
//!      （`a = n / 3` 向下取整）
//!   2. 逐字符换字母表，并把 `=` 换成 `$`
//!
//! 逆运算只用于自检与排查，生产链路只走上行编码。

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

const QODER_ALPHABET: &[u8; 64] =
    b"_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";
const STANDARD_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/// 标准 Base64 的填充符在 Qoder 变体里是 `$`。
const QODER_PAD: u8 = b'$';

/// 标准字符 → Qoder 字符。
fn to_qoder_char(byte: u8) -> u8 {
    if byte == b'=' {
        return QODER_PAD;
    }
    STANDARD_ALPHABET
        .iter()
        .position(|candidate| *candidate == byte)
        .map(|index| QODER_ALPHABET[index])
        .unwrap_or(byte)
}

/// Qoder 字符 → 标准字符。
fn to_standard_char(byte: u8) -> u8 {
    if byte == QODER_PAD {
        return b'=';
    }
    QODER_ALPHABET
        .iter()
        .position(|candidate| *candidate == byte)
        .map(|index| STANDARD_ALPHABET[index])
        .unwrap_or(byte)
}

/// 明文 → Qoder 变体编码（请求体形态）。
pub fn encode(plain: &[u8]) -> String {
    let standard = STANDARD.encode(plain);
    let bytes = standard.as_bytes();
    let n = bytes.len();
    let a = n / 3;
    // S[n-a:] + S[a:n-a] + S[:a]
    let mut rearranged = String::with_capacity(n);
    for &byte in bytes[n - a..].iter() {
        rearranged.push(to_qoder_char(byte) as char);
    }
    for &byte in bytes[a..n - a].iter() {
        rearranged.push(to_qoder_char(byte) as char);
    }
    for &byte in bytes[..a].iter() {
        rearranged.push(to_qoder_char(byte) as char);
    }
    rearranged
}

/// 字符串版本（pi 侧 `encodeQoderBase64(string)`）。
pub fn encode_str(plain: &str) -> String {
    encode(plain.as_bytes())
}

/// Qoder 变体编码 → 明文（仅用于测试与排查）。
pub fn decode(encoded: &str) -> Option<Vec<u8>> {
    let bytes = encoded.as_bytes();
    let n = bytes.len();
    let a = n / 3;
    // 逆轮转：R = S[n-a:] + S[a:n-a] + S[:a]，所以
    // S = R 的后 a 个 + R 的中间 + R 的前 a 个。
    let mut standard = String::with_capacity(n);
    for &byte in bytes[n - a..].iter() {
        standard.push(to_standard_char(byte) as char);
    }
    for &byte in bytes[a..n - a].iter() {
        standard.push(to_standard_char(byte) as char);
    }
    for &byte in bytes[..a].iter() {
        standard.push(to_standard_char(byte) as char);
    }
    STANDARD.decode(standard).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 期望值由 `pi-desktop/lib/qoder-cn/qoder-base64.ts` 的
    /// `encodeQoderBase64()` 生成（node v24 直接跑 TS）。
    #[test]
    fn matches_the_pi_encoder() {
        for (plain, expected) in [
            ("{}", "$kwm"),
            ("", ""),
            ("a", "$p$#"),
            ("ab", "$SB#"),
            ("abc", "MSK#"),
            ("hello world", "YuHp$Hq&J(WPHFru"),
            ("中文 body", "bDZz$)PZBHKQG*tQ"),
        ] {
            assert_eq!(encode_str(plain), expected, "明文 {plain:?}");
        }
    }

    #[test]
    fn round_trips() {
        for plain in ["{}", "", "a", "ab", "abc", "hello world", "中文 body"] {
            let encoded = encode_str(plain);
            let decoded = decode(&encoded).expect("解码");
            assert_eq!(String::from_utf8(decoded).unwrap(), plain);
        }
    }
}
