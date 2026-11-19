/** 最近显式选择的推理级别：草稿尚未建会话时也能恢复。 */
export const THINKING_LEVEL_OPTIONS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"] as const;
export type ThinkingLevelOption = typeof THINKING_LEVEL_OPTIONS[number];
/**
 * 未选择（或旧数据里存着已废弃的 `auto`）时使用的级别。
 *
 * 曾经有第八个选项 `auto`（「默认推理」），但 rpi 的等级枚举里没有它，运行时必然落成
 * `off`；界面只能靠 `resolveDisplayThinkingLevel` 把生效值再还原成 `auto`，否则用户
 * 刚选完就跳成「关闭推理」。既然 `auto` 与 `off` 运行时完全等价，这个选项只是给用户
 * 制造了「选了什么也没发生」的错觉，已从菜单里去掉。现在列表为空时直接给一个真实
 * 档位，显示值与生效值不再需要还原。
 */
export const DEFAULT_THINKING_LEVEL: ThinkingLevelOption = "medium";
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
/** 当前模型允许选择的档位。`supported` 为 `null`/`undefined`（还没加载到）时不过滤。 */
export function availableThinkingLevels(
  supported: readonly string[] | null | undefined,
): ThinkingLevelOption[] {
  return supported
    ? THINKING_LEVEL_OPTIONS.filter((level) => supported.includes(level))
    : [...THINKING_LEVEL_OPTIONS];
}

/**
 * 下拉框是否值得展示。
 *
 * 只支持 `off` 的模型（`reasoning: false`，例如 qoder 的 GLM / Kimi / MiniMax 系列）
 * 只有一项可选：列一个「关闭推理」既没有可操作性，也让人以为推理能力坏了，所以直接
 * 不渲染入口。以前还能选 `auto`（默认推理）时菜单至少有 2 项，删掉 `auto` 后这里才
 * 会真的变成 1。
 */
export function isThinkingLevelAdjustable(supported: readonly string[] | null | undefined): boolean {
  return availableThinkingLevels(supported).length > 1;
}

export function resolveThinkingLevel(
  preferred: unknown,
  saved: unknown,
  pinned: unknown,
  supported: readonly string[],
): ThinkingLevelOption {
  const level = parseThinkingLevel(preferred) ?? parseThinkingLevel(saved) ?? parseThinkingLevel(pinned);
  // 不修改偏好本身，切回支持的模型仍可恢复原选择。
  if (!level) return fallbackThinkingLevel(supported);
  return supported.includes(level) ? level : fallbackThinkingLevel(supported);
}

/**
 * 当前模型不支持目标级别时的显示值：优先 `DEFAULT_THINKING_LEVEL`，其次该模型支持的
 * 第一档（不支持推理的模型只有 `off`，正好落到「关闭推理」）。
 */
function fallbackThinkingLevel(supported: readonly string[]): ThinkingLevelOption {
  if (supported.includes(DEFAULT_THINKING_LEVEL)) return DEFAULT_THINKING_LEVEL;
  return parseThinkingLevel(supported[0]) ?? DEFAULT_THINKING_LEVEL;
}

/**
 * 界面要显示的等级。
 *
 * 服务端只回报「实际生效级别」（`/api/agent/new`、`get_state`、
 * `/api/sessions/{id}` 的 `context.thinkingLevel`、v3 文件里的
 * `thinking_level_change`），用户选 `high` 而模型只支持 `off` 时会回 `off`——
 * 这时以生效值为准，界面不能继续显示一个没生效的档位。
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
  // 旧数据里可能存着已废弃的 `auto`：`parseThinkingLevel` 会把它当无效值，这里退回
  // 生效值，不存在就退回默认档，界面上绝不会再出现 `auto` 字样。
  return effective ?? desired ?? DEFAULT_THINKING_LEVEL;
}
