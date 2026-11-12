/**
 * 把 OpenAI Chat Completions 请求体翻译成 Qoder 的 agent 请求体。
 *
 * Qoder 的对话入口不是「给一段 messages 就回一段 completion」，而是一次
 * agent 回合：`chat_context`、`model_config`、`business` 这些字段共同决定
 * 上游怎么排额度、归组用量、按哪个模型路由。骨架来自官方客户端的请求体
 * （qoder2api-hub 的 baseprompt.json），这里只保留静态键，动态部分全部
 * 按本次请求重算。
 *
 * 两条与上游行为强相关的约定：
 *   - 上游**只认** `model_config.key` / `x-model-key` 里的路由键，未知键不报错，
 *     会静默回落 `auto`（用户会莫名其妙用上另一个模型）；
 *   - 工具历史有「结构化」和「文本化」两种形态：结构化更准，但只有部分模型
 *     接受配对的 assistant.tool_calls / role:tool，其余模型必须序列化成文本标记。
 */
import { randomUUID } from "node:crypto";
import { QODER_CN } from "./constants";
import { QODER_DEFAULT_USER_TYPE } from "./cosy";
import type { QoderCnCatalogEntry } from "./catalog";
import { encodeQoderBase64 } from "./qoder-base64";

/** 文本化工具历史时，assistant 请求调用工具的那行标记。 */
const TOOL_CALL_MARKER = "[assistant 请求调用工具]";
/** 文本化工具历史时，工具结果消息的前缀（名字随后拼进来）。 */
const TOOL_RESULT_MARKER = "[工具结果";

/**
 * 允许「结构化」工具历史的模型族。
 *
 * 官方目录里的白名单是按客户端可见的模型名（qwen/glm/deepseek）写的；我们
 * 暴露给 pi 的 id 就是上游路由键，所以这里按族换算一次，而不是照抄字符串。
 * 命中不了的一律走文本化 —— 文本化对全家模型都成立，只是精度略低。
 */
const STRUCTURED_KEY_PREFIXES = [
  "qmodel", "qfmodel", "q37fmodel", // Qwen 族
  "gmodel", "gfmodel", "gm",        // GLM 族
  "dmodel", "dfmodel",              // DeepSeek 族
];

/** DeepSeek 族的多轮一致性依赖 reasoning_content，必须原样回传。 */
function isDeepSeekKey(modelKey: string): boolean {
  return modelKey.startsWith("dmodel") || modelKey.startsWith("dfmodel");
}

interface OpenAiContentPart {
  type?: unknown;
  text?: unknown;
  image_url?: unknown;
  url?: unknown;
}

interface FlattenedContent {
  text: string;
  images: string[];
}

function flattenContent(content: unknown): FlattenedContent {
  if (content === null || content === undefined) return { text: "", images: [] };
  if (typeof content === "string") return { text: content, images: [] };
  if (ArrayBuffer.isView(content)) return { text: "", images: [] };
  // 单 part 对象直接当 content 的客户端也存在：包装成单元素列表，绝不 stringify
  // （否则图片会变成一段 JSON 文本发给模型，图片从未真正到达）。
  const parts = Array.isArray(content) ? content : [content];
  const texts: string[] = [];
  const images: string[] = [];
  for (const part of parts) {
    if (typeof part === "string") {
      texts.push(part);
      continue;
    }
    if (!part || typeof part !== "object") continue;
    const piece = part as OpenAiContentPart;
    const type = typeof piece.type === "string" ? piece.type : "";
    if (type === "text" || type === "input_text" || type === "output_text" || type === "summary_text") {
      if (typeof piece.text === "string") texts.push(piece.text);
      continue;
    }
    if (type === "image_url" || type === "image" || type === "input_image" || "image_url" in piece) {
      const raw = piece.image_url ?? piece.url;
      const url = typeof raw === "string"
        ? raw
        : raw && typeof raw === "object" && typeof (raw as { url?: unknown }).url === "string"
          ? (raw as { url: string }).url
          : "";
      if (url) images.push(url);
    }
  }
  return { text: texts.join(""), images };
}

/** 带图消息保留 OpenAI vision parts 形态（上游只读 content 里的 parts）。 */
function visionContent(content: unknown, text: string): unknown {
  if (Array.isArray(content)) return content;
  if (typeof content === "string") return content;
  return text;
}

function toolArguments(value: unknown): string {
  if (typeof value === "string") return value;
  if (value === null || value === undefined) return "";
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}

interface ChatMessage {
  role?: unknown;
  content?: unknown;
  tool_calls?: unknown;
  tool_call_id?: unknown;
  name?: unknown;
  reasoning_content?: unknown;
}

export interface FlattenOptions {
  structured: boolean;
  keepReasoning: boolean;
}

export interface FlattenResult {
  systemText: string | null;
  messages: Array<Record<string, unknown>>;
  /** 出现过图片时，顶层 image_urls 需要跟着填（兼容无损，仅观测用）。 */
  hasImages: boolean;
}

/**
 * 压平会话：首个 system/developer 抽为系统提示词，其余按上游可接受的形态排列。
 *
 * 「结构化」与「文本化」的差别只体现在工具历史上：
 *   - 结构化：保留 role:"tool" + tool_call_id、assistant.tool_calls（id/type/function）
 *   - 文本化：工具结果降级成 user 文本，assistant 的调用意图序列化进正文
 */
export function flattenMessages(messages: readonly unknown[], options: FlattenOptions): FlattenResult {
  let systemText: string | null = null;
  let hasImages = false;
  const flat: Array<Record<string, unknown>> = [];

  for (const raw of messages) {
    if (!raw || typeof raw !== "object") continue;
    const message = raw as ChatMessage;
    const role = typeof message.role === "string" ? message.role : "";
    const { text, images } = flattenContent(message.content);
    if (images.length > 0) hasImages = true;

    if (role === "system" || role === "developer") {
      // 只取第一条非空系统消息作为 model 的系统提示词：pi 的会话就是这个形态。
      if (systemText === null && text) systemText = text;
      continue;
    }

    if (role === "tool") {
      const toolName = typeof message.name === "string" && message.name ? ` (${message.name})` : "";
      if (options.structured) {
        flat.push({
          role: "tool",
          tool_call_id: typeof message.tool_call_id === "string" ? message.tool_call_id : "",
          // content 用空字符串而不是 null：null 会让上游丢掉配对的 assistant 消息。
          content: text || "",
        });
        continue;
      }
      flat.push({
        role: "user",
        content: `${TOOL_RESULT_MARKER}${toolName}]\n${text || ""}`,
      });
      continue;
    }

    if (role === "assistant" && Array.isArray(message.tool_calls) && message.tool_calls.length > 0) {
      if (options.structured) {
        const calls = message.tool_calls.map((rawCall) => {
          const call = (rawCall && typeof rawCall === "object" ? rawCall : {}) as {
            id?: unknown;
            type?: unknown;
            function?: { name?: unknown; arguments?: unknown };
          };
          return {
            id: typeof call.id === "string" ? call.id : "",
            type: typeof call.type === "string" ? call.type : "function",
            function: {
              name: typeof call.function?.name === "string" ? call.function.name : "",
              arguments: toolArguments(call.function?.arguments),
            },
          };
        });
        flat.push({
          role: "assistant",
          content: text || "",
          tool_calls: calls,
          ...(options.keepReasoning ? { reasoning_content: reasoningOf(message) } : {}),
        });
        continue;
      }
      const outlined = message.tool_calls.map((rawCall) => {
        const call = (rawCall && typeof rawCall === "object" ? rawCall : {}) as {
          function?: { name?: unknown; arguments?: unknown };
        };
        return {
          name: typeof call.function?.name === "string" ? call.function.name : "",
          arguments: toolArguments(call.function?.arguments),
        };
      });
      flat.push({
        role: "assistant",
        content: `${text || ""}\n\n${TOOL_CALL_MARKER}\n${JSON.stringify(outlined)}`,
        ...(options.keepReasoning ? { reasoning_content: reasoningOf(message) } : {}),
      });
      continue;
    }

    if (role === "user" || role === "assistant") {
      flat.push({
        role,
        content: images.length > 0 ? visionContent(message.content, text) : (text || ""),
        ...(role === "assistant" && options.keepReasoning
          ? { reasoning_content: reasoningOf(message) }
          : {}),
      });
      continue;
    }

    if (role) {
      // 未知角色一律降级为 user，不静默丢弃内容。
      flat.push({ role: "user", content: text || "" });
    }
  }

  return { systemText, messages: flat, hasImages };
}

function reasoningOf(message: ChatMessage): string {
  return typeof message.reasoning_content === "string" ? message.reasoning_content : "";
}

/** 上游要求 tools[] 是 {type:"function", function:{...}} 形态，平铺形态一律包一层。 */
export function normalizeTools(tools: unknown): Array<Record<string, unknown>> {
  if (!Array.isArray(tools)) return [];
  const normalized: Array<Record<string, unknown>> = [];
  for (const raw of tools) {
    if (!raw || typeof raw !== "object") continue;
    const tool = raw as Record<string, unknown>;
    const type = tool.type;
    if (type !== undefined && type !== null && type !== "function") continue;
    if (typeof tool.name === "string" && !("function" in tool)) {
      normalized.push({
        type: "function",
        function: {
          name: tool.name,
          description: typeof tool.description === "string" ? tool.description : "",
          parameters: tool.parameters ?? {},
          ...(typeof tool.strict === "boolean" ? { strict: tool.strict } : {}),
        },
      });
      continue;
    }
    normalized.push(tool);
  }
  return normalized;
}

export interface QoderBodyContext {
  /** 上游路由键（目录里的 key）。 */
  modelKey: string;
  /** 该 key 在官方目录里的元数据。 */
  catalog: QoderCnCatalogEntry;
  /** 账号用户类型（aliyun_user_type）。 */
  userType?: string;
  /** 同一段对话复用同一个 business id / begin_at，否则用量面板会把一轮拆成一堆碎片。 */
  conversation?: { id: string; beginAt: number };
}

function pickMaxTokens(body: Record<string, unknown>): number | undefined {
  for (const key of ["max_completion_tokens", "max_tokens"]) {
    const value = body[key];
    if (typeof value === "number" && Number.isFinite(value) && value > 0) return Math.trunc(value);
  }
  return undefined;
}

function pickEffort(body: Record<string, unknown>, catalog: QoderCnCatalogEntry): string | undefined {
  const raw = body.reasoning_effort;
  if (typeof raw !== "string" || !raw) return undefined;
  const effort = raw.toLowerCase();
  // 目录没给档位的模型不接受任何 effort；给了档位的按官方词表校验一次。
  if (catalog.efforts.length === 0) return undefined;
  return catalog.efforts.includes(effort) ? effort : undefined;
}

function isStructuredKey(modelKey: string): boolean {
  return STRUCTURED_KEY_PREFIXES.some((prefix) => modelKey.startsWith(prefix));
}

/**
 * 构造并编码 Qoder 请求体。
 *
 * 返回的字符串就是签名与发送用的最终字节：`encodeQoderBase64` 之后不能再改，
 * 否则 COSY 签名（md5 覆盖 body）会与 body 不一致。
 */
export function buildQoderRequestBody(
  openAiBody: Record<string, unknown>,
  context: QoderBodyContext,
): string {
  const rawMessages = Array.isArray(openAiBody.messages) ? openAiBody.messages : [];
  const structured = isStructuredKey(context.modelKey);
  const flattened = flattenMessages(rawMessages, {
    structured,
    keepReasoning: isDeepSeekKey(context.modelKey),
  });

  const model = context.catalog;
  const modelConfig: Record<string, unknown> = {
    key: context.modelKey,
    display_name: model.displayName,
    model: "",
    format: "openai",
    is_vl: model.vision,
    is_reasoning: model.reasoning,
    api_key: "",
    url: "",
    source: "system",
    max_input_tokens: model.maxInputTokens,
  };

  // 系统提示词：pi 总会带自己的 agent 提示词，直接用；缺失时才回落到官方骨架的
  // 那一段（我们的模板不再内置 59KB 官方提示词，缺省即空）。
  const messages: Array<Record<string, unknown>> = [];
  if (flattened.systemText) messages.push({ role: "system", content: flattened.systemText });
  messages.push(...flattened.messages);

  const prompt = lastUserPrompt(flattened.messages);
  const requestId = randomUUID();
  const conversation = context.conversation;

  const parameters: Record<string, unknown> = {};
  const maxTokens = pickMaxTokens(openAiBody);
  if (maxTokens) parameters.max_tokens = maxTokens;
  const effort = pickEffort(openAiBody, model);
  if (effort) parameters.reasoning_effort = effort;

  const body: Record<string, unknown> = {
    request_id: requestId,
    request_set_id: randomUUID(),
    chat_record_id: conversation?.id ?? requestId,
    stream: true,
    chat_task: "FREE_INPUT",
    chat_context: {
      chatPrompt: "",
      extra: {
        context: [],
        modelConfig,
        originalContent: { type: "text", text: prompt },
      },
      features: [],
      imageUrls: null,
      text: { type: "text", text: prompt },
    },
    // 顶层 image_urls 官方默认不再填：上游只读 messages 里的 vision parts，
    // 顶层这份只会把请求体撑大（大图会被阿里云 SLB 直接 RST）。
    image_urls: null,
    is_reply: true,
    is_retry: false,
    session_id: randomUUID(),
    code_language: "",
    source: 1,
    version: "3",
    chat_prompt: "",
    parameters,
    aliyun_user_type: context.userType || QODER_DEFAULT_USER_TYPE,
    session_type: "qodercli",
    agent_id: "agent_common",
    task_id: "common",
    model_config: modelConfig,
    messages,
    tools: normalizeTools(openAiBody.tools),
    business: {
      product: "cli",
      type: "agent",
      stage: "start",
      id: conversation?.id ?? randomUUID(),
      name: prompt ? prompt.slice(0, 30) : "chat",
      begin_at: conversation?.beginAt ?? Date.now(),
      // 上游按 business.model_key 归因计费：缺了它，新模型会被静默记到 auto。
      model_key: context.modelKey,
      version: QODER_CN.cosyVersion,
    },
  };

  return encodeQoderBase64(JSON.stringify(body));
}

function lastUserPrompt(messages: ReadonlyArray<Record<string, unknown>>): string {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index]!;
    if (message.role !== "user") continue;
    const content = message.content;
    if (typeof content === "string" && content) return content;
    if (Array.isArray(content)) {
      const text = content
        .map((part) => (part && typeof part === "object" && typeof (part as { text?: unknown }).text === "string"
          ? (part as { text: string }).text
          : ""))
        .join("\n");
      if (text) return text;
    }
  }
  return "";
}
