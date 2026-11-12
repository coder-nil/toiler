import type { AgentMessage } from "./types";

/**
 * 手机遥控页的文本投影。
 *
 * 手机上不渲染 markdown 之外的重内容（代码高亮 / KaTeX / 图片预览），所以这里把会话
 * 消息压成「谁说了什么 + 思考 + 工具调用（含展开用的入参与结果）」。规则尽量保守：
 * 只保留人能快速扫读的信息，并在服务端就截断，避免手机拉到无用的长文本。
 */

/** 一次工具调用：一行摘要 + 展开后可见的入参/结果。 */
export interface MobileToolCall {
  /** 关联的结果消息用 `toolCallId` 对应，同时作为列表 key。 */
  id: string;
  name: string;
  /** 一行摘要里 `name` 后面的部分，例如 `components/SettingsPanel.tsx`。 */
  hint: string;
  /** 展开后的入参（美化 JSON 或流式中的原始 JSON 片段），已截断。 */
  input?: string;
  /** 配对的工具结果，已截断。 */
  output?: string;
  isError?: boolean;
}

export interface MobileMessage {
  /** 会话条目 id，作为列表 key；流式尾巴用组件自己的临时 id。 */
  id: string;
  role: "user" | "assistant";
  text: string;
  /** 思考过程：助手消息才有，已截断。 */
  thinking?: string;
  /** 工具调用（含展开用的入参/结果）。 */
  tools?: MobileToolCall[];
  at?: string;
}

const MAX_MESSAGE_CHARS = 1500;
const MAX_THINKING_CHARS = 1200;
const MAX_TOOL_INPUT_CHARS = 600;
const MAX_TOOL_OUTPUT_CHARS = 800;
const MAX_TOOL_HINT_CHARS = 60;

/** 手机端历史窗口：默认只送最新的这一截，更早的由「载入更早」一批批往回要。 */
export const MOBILE_DEFAULT_LIMIT = 30;
/** 窗口一次往回长多少条。 */
export const MOBILE_LIMIT_STEP = 30;
/** 手机上最多往回翻到这里（带细节的窗口更小，见 `detailWindow`）。 */
export const MOBILE_MAX_LIMIT = 300;

/** 窗口里最后 N 条保留思考与工具细节，更早的历史只留一行摘要。 */
export const MOBILE_DETAIL_WINDOW = 40;

/** 阻塞式扩展 UI 请求（select / confirm / input / editor / custom）。 */
const BLOCKING_UI_METHODS = new Set(["select", "confirm", "input", "editor", "custom"]);

/** 活会话状态里与「待回答的扩展请求」相关的部分（`get_state` 返回值的子集）。 */
export interface MobilePendingUiState {
  pendingUiRequests?: unknown[];
}

/**
 * 手机上要弹的待确认请求。
 *
 * `ask_user` 就是通过这条路径问问题的（扩展 UI 请求）：不问完，这一轮就一直挂着。
 * 这里把第一个阻塞请求带到快照里，手机端才能把它画出来并回答；同时存在多个时
 * 也只处理第一个，与桌面端一次只弹一个的行为一致。
 */
export function pickPendingUiRequest(
  state: MobilePendingUiState | null,
): Record<string, unknown> | null {
  const requests = state?.pendingUiRequests;
  if (!Array.isArray(requests)) return null;
  for (const request of requests) {
    if (typeof request !== "object" || request === null) continue;
    const record = request as Record<string, unknown>;
    if (typeof record.id !== "string" || typeof record.method !== "string") continue;
    if (!BLOCKING_UI_METHODS.has(record.method)) continue;
    return record;
  }
  return null;
}

export interface MobileProjectionOptions {
  /**
   * 保留「思考 + 工具入参/结果」的尾部条数，更早的消息降成摘要。
   * 不传则全部保留细节。一条带细节的消息大约 1.7KB，几百条就是几百 KB，
   * 而往回翻历史时看的是「做了什么」，不需要展开细节。
   */
  detailWindow?: number;
}

export interface MobileMessageWindow {
  messages: MobileMessage[];
  /** 窗口之外还有多少条更早的可投影消息（0 表示已经到头）。 */
  earlierCount: number;
}

/** 工具调用的可读摘要：`read src/a.ts`、`bash npm test`。 */
export function describeToolCall(name: string, args: unknown): string {
  const hint = toolHint(args);
  return hint ? `${name} ${hint}` : name;
}

function toolHint(args: unknown): string | undefined {
  if (typeof args !== "object" || args === null || Array.isArray(args)) return undefined;
  const record = args as Record<string, unknown>;
  for (const key of ["path", "file_path", "filePath", "command", "pattern", "query", "url", "prompt"]) {
    const value = record[key];
    if (typeof value !== "string") continue;
    const firstLine = value.trim().split("\n", 1)[0]?.trim();
    if (!firstLine) continue;
    return firstLine.length > MAX_TOOL_HINT_CHARS
      ? `${firstLine.slice(0, MAX_TOOL_HINT_CHARS - 3)}...`
      : firstLine;
  }
  return undefined;
}

function truncate(text: string, limit: number): string {
  return text.length > limit ? `${text.slice(0, limit)}\n…` : text;
}

export function textFromBlocks(blocks: unknown): { text: string; images: number } {
  if (typeof blocks === "string") return { text: blocks, images: 0 };
  if (!Array.isArray(blocks)) return { text: "", images: 0 };

  const parts: string[] = [];
  let images = 0;
  for (const block of blocks) {
    if (typeof block !== "object" || block === null) continue;
    const record = block as Record<string, unknown>;
    if (record.type === "text" && typeof record.text === "string") {
      parts.push(record.text);
    } else if (record.type === "image") {
      images += 1;
    }
  }
  return { text: parts.join("\n").trim(), images };
}

/** 思考过程：多个 thinking block 按出现顺序拼接，服务端就截断。 */
export function thinkingFromBlocks(blocks: unknown): string {
  if (!Array.isArray(blocks)) return "";
  const parts: string[] = [];
  for (const block of blocks) {
    if (typeof block !== "object" || block === null) continue;
    const record = block as Record<string, unknown>;
    if (record.type !== "thinking" || typeof record.thinking !== "string") continue;
    const text = record.thinking.trim();
    if (text) parts.push(text);
  }
  if (parts.length === 0) return "";
  return truncate(parts.join("\n\n"), MAX_THINKING_CHARS);
}

function formatToolInput(args: unknown, rawInput: unknown): string | undefined {
  // 流式中参数还在生成：`rawInput` 是尚未解析的 JSON 片段。它要排在 `arguments` 前面，
  // 因为流式块里 `arguments` 是占位的 `{}`，直接美化 JSON 只会把入参显示成「{}」。
  if (typeof rawInput === "string" && rawInput.trim()) return truncate(rawInput.trim(), MAX_TOOL_INPUT_CHARS);
  if (args !== undefined && args !== null) {
    if (typeof args === "string") {
      const text = args.trim();
      return text ? truncate(text, MAX_TOOL_INPUT_CHARS) : undefined;
    }
    try {
      return truncate(JSON.stringify(args, null, 2), MAX_TOOL_INPUT_CHARS);
    } catch {
      return undefined;
    }
  }
  return undefined;
}

function collectToolResults(
  messages: readonly AgentMessage[],
): Map<string, { text: string; isError: boolean }> {
  const results = new Map<string, { text: string; isError: boolean }>();
  for (const message of messages) {
    if (message.role !== "toolResult") continue;
    const record = message as { toolCallId?: unknown; content?: unknown; isError?: unknown };
    if (typeof record.toolCallId !== "string" || !record.toolCallId) continue;
    const { text } = textFromBlocks(record.content);
    results.set(record.toolCallId, {
      text: truncate(text.trim(), MAX_TOOL_OUTPUT_CHARS),
      isError: record.isError === true,
    });
  }
  return results;
}

function formatClock(timestamp: unknown): string | undefined {
  if (typeof timestamp !== "number" || !Number.isFinite(timestamp)) return undefined;
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) return undefined;
  return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}

/** 把一条助手消息的 content blocks 压成「正文 + 思考 + 工具调用」。 */
export function projectAssistantBlocks(blocks: unknown): {
  text: string;
  thinking: string;
  tools: MobileToolCall[];
} {
  const { text } = textFromBlocks(blocks);
  const thinking = thinkingFromBlocks(blocks);
  const tools: MobileToolCall[] = [];

  if (Array.isArray(blocks)) {
    blocks.forEach((block, index) => {
      if (typeof block !== "object" || block === null) return;
      const record = block as Record<string, unknown>;
      if (record.type !== "toolCall") return;
      const name = typeof record.name === "string"
        ? record.name
        : typeof record.toolName === "string"
          ? record.toolName
          : "tool";
      const args = record.arguments ?? record.input;
      const input = formatToolInput(args, record.rawInput);
      tools.push({
        id: typeof record.id === "string" && record.id
          ? record.id
          : typeof record.toolCallId === "string" && record.toolCallId
            ? record.toolCallId
            : `tool-${index}`,
        name,
        hint: toolHint(args) ?? "",
        ...(input ? { input } : {}),
      });
    });
  }

  return { text, thinking, tools };
}

/**
 * 把会话消息压成手机端消息列表。
 *
 * `entryIds` 与 `messages` 平行（见 `SessionContext`），用于给每条消息一个稳定 key。
 * 工具结果按 `toolCallId` 配回对应的工具调用，不单独成条。
 */
export function toMobileMessages(
  messages: readonly AgentMessage[],
  entryIds: readonly string[] = [],
  limit = MOBILE_DEFAULT_LIMIT,
  options: MobileProjectionOptions = {},
): MobileMessage[] {
  return buildMobileWindow(messages, entryIds, limit, options).messages;
}

/**
 * 手机端要的那一窗消息：最新的 `limit` 条 + 更早还有多少条。
 *
 * 窗口固定贴尾（会话是追加的），往前翻靠把 `limit` 调大；`detailWindow` 之外的
 * 历史降成摘要（丢思考与工具入参/结果，保留工具名与摘要行），否则翻到几百条时
 * 单次快照会到几 MB。
 */
export function buildMobileWindow(
  messages: readonly AgentMessage[],
  entryIds: readonly string[] = [],
  limit = MOBILE_DEFAULT_LIMIT,
  options: MobileProjectionOptions = {},
): MobileMessageWindow {
  const projected = projectMobileMessages(messages, entryIds);
  const size = Math.max(1, limit);
  const visible = projected.slice(Math.max(0, projected.length - size));

  const detailWindow = options.detailWindow;
  if (detailWindow === undefined) {
    return { messages: visible, earlierCount: projected.length - visible.length };
  }

  const detailFrom = Math.max(0, visible.length - Math.max(0, detailWindow));
  return {
    messages: visible.map((message, index) => (index < detailFrom ? skimMobileMessage(message) : message)),
    earlierCount: projected.length - visible.length,
  };
}

/** 历史消息降成摘要：留正文与 `name hint`，丢掉思考与展开用的入参/结果。 */
export function skimMobileMessage(message: MobileMessage): MobileMessage {
  const hasDetail = Boolean(message.thinking) || Boolean(message.tools?.some((call) => call.input || call.output));
  if (!hasDetail) return message;

  // 显式重建而不是解构丢弃字段：省略号解构会留下未使用变量的告警。
  const tools = message.tools?.map((call) => ({
    id: call.id,
    name: call.name,
    hint: call.hint,
    ...(call.isError ? { isError: true } : {}),
  }));

  return {
    id: message.id,
    role: message.role,
    text: message.text,
    ...(tools && tools.length > 0 ? { tools } : {}),
    ...(message.at ? { at: message.at } : {}),
  };
}

function projectMobileMessages(
  messages: readonly AgentMessage[],
  entryIds: readonly string[],
): MobileMessage[] {
  const projected: MobileMessage[] = [];
  const toolResults = collectToolResults(messages);

  messages.forEach((message, index) => {
    const id = entryIds[index] ?? `m${index}`;
    const at = formatClock((message as { timestamp?: unknown }).timestamp);

    if (message.role === "user") {
      const { text, images } = textFromBlocks((message as { content?: unknown }).content);
      const body = [text, images > 0 ? `[${images} image${images > 1 ? "s" : ""}]` : ""]
        .filter(Boolean)
        .join("\n");
      if (!body) return;
      projected.push({ id, role: "user", text: truncate(body, MAX_MESSAGE_CHARS), ...(at ? { at } : {}) });
      return;
    }

    if (message.role === "assistant") {
      const { text, thinking, tools } = projectAssistantBlocks((message as { content?: unknown }).content);
      const withResults = tools.map((call) => {
        const result = toolResults.get(call.id);
        if (!result) return call;
        // 「(no output)」是 pi 对空结果的占位，展开时不必显示。
        const empty = result.text.length === 0 || result.text.trim() === "(no output)";
        return {
          ...call,
          ...(!empty ? { output: result.text } : {}),
          ...(result.isError ? { isError: true } : {}),
        };
      });
      if (!text && !thinking && withResults.length === 0) return;
      projected.push({
        id,
        role: "assistant",
        text: truncate(text, MAX_MESSAGE_CHARS),
        ...(thinking ? { thinking } : {}),
        ...(withResults.length > 0 ? { tools: withResults } : {}),
        ...(at ? { at } : {}),
      });
      return;
    }

    // 工具结果与思考过程已并进上一条助手消息（思考在 thinking 里），不单独成条。
  });

  return projected;
}
