/** 最近显式选择的推理级别：草稿尚未建会话时也能恢复。 */
export const THINKING_LEVEL_OPTIONS = ["auto", "off", "minimal", "low", "medium", "high", "xhigh", "max"] as const;
export type ThinkingLevelOption = typeof THINKING_LEVEL_OPTIONS[number];
const STORAGE_KEY = "toiler-preferred-thinking-level";
interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}
function browserStorage(): StorageLike | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage;
  } catch {
    return null;
  }
}
export function parseThinkingLevel(value: unknown): ThinkingLevelOption | null {
  return typeof value === "string" && (THINKING_LEVEL_OPTIONS as readonly string[]).includes(value)
    ? value as ThinkingLevelOption : null;
}
export function getPreferredThinkingLevel(storage: StorageLike | null = browserStorage()): ThinkingLevelOption | null {
  try {
    return parseThinkingLevel(storage?.getItem(STORAGE_KEY));
  } catch {
    return null;
  }
}
export function setPreferredThinkingLevel(level: ThinkingLevelOption, storage: StorageLike | null = browserStorage()): void {
  try {
    storage?.setItem(STORAGE_KEY, level);
  } catch {
    // 存储受限不应阻止用户调整推理级别。
  }
}
export function resolveThinkingLevel(
  preferred: unknown,
  saved: unknown,
  pinned: unknown,
  supported: readonly string[],
): ThinkingLevelOption {
  const level = parseThinkingLevel(preferred) ?? parseThinkingLevel(saved) ?? parseThinkingLevel(pinned) ?? "auto";
  // 不修改偏好本身，切回支持的模型仍可恢复原选择。
  return level === "auto" || supported.includes(level) ? level : "auto";
}
