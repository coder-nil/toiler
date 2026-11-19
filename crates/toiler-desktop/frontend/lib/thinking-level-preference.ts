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

/**
 * 界面要显示的等级：服务端只回报「实际生效级别」，`auto` 不在 rpi 的等级表里，
 * 一律按 `off` 生效（`agent_runtime::effective_thinking_level`）。
 *
 * 直接把生效级别写回显示状态，用户刚选的「默认推理(auto)」会立刻跳成「关闭推理
 * (off)」，看起来像选择没生效。所以用户显式选过 `auto` 时，`off` 这个生效值
 * 回显成 `auto`：两者运行时完全等价（都是不请求推理），差别只在用户的原意。
 *
 * 只覆盖 `auto` 这一种情况 —— `off` 是用户自己选的，不能被改写成别的档位。
 *
 * @param recorded 服务端回报或会话文件里记录的生效级别（可能是 `null`）。
 * @param requested 用户显式选过的原始偏好（浏览器 / `settings.json` 里那份）。
 */
export function resolveDisplayThinkingLevel(
  recorded: unknown,
  requested: unknown,
): ThinkingLevelOption {
  const effective = parseThinkingLevel(recorded);
  const desired = parseThinkingLevel(requested);
  if (desired === "auto" && (effective === null || effective === "off")) {
    return "auto";
  }
  return effective ?? desired ?? "auto";
}
