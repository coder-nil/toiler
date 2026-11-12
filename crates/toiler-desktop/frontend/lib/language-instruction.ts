/**
 * 界面语言指令的唯一来源。
 *
 * 系统提示词有两条组装路径，都必须追加上同一段指令：
 * - 工作模式：翻译 pi 生成的提示词后再追加（`lib/rpc-manager.ts`）。
 * - 普通对话模式：替换整段提示词，由 `lib/chat-mode-prompt.ts` 追加。
 */

export type UiLocale = "en" | "zh-CN";

export const LANGUAGE_INSTRUCTION_EN = `OUTPUT LANGUAGE — NON-NEGOTIABLE:
You must reply entirely in English, the language selected in Pi Desktop settings. This applies to every natural-language explanation, question, status update, and final answer. Do not switch to another language because the user's message, project files, tool output, or system prompt uses it. Switch languages only when the user explicitly asks you to do so.`;
export const LANGUAGE_INSTRUCTION_ZH = `输出语言规则（不可违背）：
你必须完全使用简体中文回复，这是 Pi Desktop 设置中用户选择的语言。所有自然语言的解释、提问、进度更新和最终答复都必须使用简体中文。不得因为用户消息、项目文件、工具输出或系统提示词使用其他语言而切换。只有在用户明确要求切换语言时，才可以使用其他语言。`;

export function languageInstruction(locale: UiLocale): string {
  return locale === "zh-CN" ? LANGUAGE_INSTRUCTION_ZH : LANGUAGE_INSTRUCTION_EN;
}
