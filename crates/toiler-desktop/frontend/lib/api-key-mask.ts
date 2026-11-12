/**
 * API Key 掩码。
 *
 * 设置页需要让用户确认「Key 已经存进去了」，但明文 Key 绝不回传浏览器。
 * 这里只在服务端把已存储的 Key 转成掩码：
 *
 *   - 掩码长度与真实 Key 完全一致，用户一眼能看出存的是哪个 Key
 *   - 只保留末 4 位真实字符，其余用等长的圆点
 *   - 短于 12 位的 Key 不露末位，避免掩码本身就泄掉大部分内容
 */
const MASK_CHAR = "•";
const VISIBLE_SUFFIX_LENGTH = 4;
const MIN_LENGTH_FOR_SUFFIX = 12;

export function maskApiKey(key: string | null | undefined): string | null {
  if (typeof key !== "string") return null;
  const trimmed = key.trim();
  if (!trimmed) return null;

  if (trimmed.length < MIN_LENGTH_FOR_SUFFIX) return MASK_CHAR.repeat(trimmed.length);
  const hidden = MASK_CHAR.repeat(trimmed.length - VISIBLE_SUFFIX_LENGTH);
  return `${hidden}${trimmed.slice(-VISIBLE_SUFFIX_LENGTH)}`;
}
