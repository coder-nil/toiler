/**
 * Qoder 自定义 Base64 变体（出站请求体的编码格式）。
 *
 * 与标准 Base64 的差别只有两步，但两步都必须逐字节对齐，否则上游
 * 解不开 body、直接按「签名不一致 / 参数非法」拒绝：
 *
 *   1. 把标准 Base64 切成「尾 / 中 / 首」三段后轮转：R = S[n-a:] + S[a:n-a] + S[:a]
 *   2. 逐字符换字母表，并把 '=' 换成 '$'
 *
 * 逆运算（`decode`）只用于自检与排查，生产链路只走上行编码。
 */

const QODER_ALPHABET = "_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";
const STANDARD_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const QODER_PAD = "$";

if (new Set(QODER_ALPHABET).size !== 64 || QODER_ALPHABET.length !== 64) {
  throw new Error("qoder base64: alphabet must be 64 unique characters");
}

const STANDARD_TO_QODER = new Map<string, string>();
const QODER_TO_STANDARD = new Map<string, string>();
for (let i = 0; i < 64; i += 1) {
  STANDARD_TO_QODER.set(STANDARD_ALPHABET[i]!, QODER_ALPHABET[i]!);
  QODER_TO_STANDARD.set(QODER_ALPHABET[i]!, STANDARD_ALPHABET[i]!);
}
STANDARD_TO_QODER.set("=", QODER_PAD);
QODER_TO_STANDARD.set(QODER_PAD, "=");

/** 明文 → Qoder 变体编码（请求体形态）。 */
export function encodeQoderBase64(plain: Uint8Array | string): string {
  const bytes = typeof plain === "string" ? new TextEncoder().encode(plain) : plain;
  const standard = Buffer.from(bytes).toString("base64");
  const n = standard.length;
  const a = Math.floor(n / 3);
  const rearranged = standard.slice(n - a) + standard.slice(a, n - a) + standard.slice(0, a);
  let out = "";
  for (const char of rearranged) {
    out += STANDARD_TO_QODER.get(char) ?? char;
  }
  return out;
}

/** Qoder 变体编码 → 明文（仅用于测试与排查）。 */
export function decodeQoderBase64(encoded: string): Uint8Array {
  let standard = "";
  for (const char of encoded) {
    standard += QODER_TO_STANDARD.get(char) ?? char;
  }
  const n = standard.length;
  const a = Math.floor(n / 3);
  const original = standard.slice(n - a) + standard.slice(a, n - a) + standard.slice(0, a);
  return new Uint8Array(Buffer.from(original, "base64"));
}
