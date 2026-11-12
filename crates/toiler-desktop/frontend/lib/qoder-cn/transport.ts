/**
 * Qoder CN 的传输层：把 pi 发往「OpenAI 兼容端点」的请求改写成 COSY 签名请求，
 * 再把上游的 SSE 信封拆回标准 OpenAI chunk 流。
 *
 * 为什么不自己实现一整套流式解析：上游信封里的 `body` 字段本身就是标准
 * OpenAI chunk（`{"headers":{…},"body":"<chunk>","statusCodeValue":200}`），
 * 所以只要把信封拆掉，pi 自带的 OpenAI 适配器就能原样消费——工具调用、
 * usage、reasoning 的解析都复用 SDK，避免两份协议实现漂移。
 */
import type { FetchFunction } from "@earendil-works/pi-ai";
import { createHash } from "node:crypto";
import { QODER_CN, QODER_CN_CHAT_PATH } from "./constants";
import type { QoderCnCatalogEntry } from "./catalog";
import { createCosySession } from "./cosy";
import { buildQoderRequestBody } from "./request-body";

/** 供 transport 使用的账号凭证视图（由 provider 注入）。 */
export interface QoderCnTransportCredential {
  uid: string;
  nickname?: string;
  accessToken: string;
  refreshToken?: string;
  userType?: string;
  organizationId?: string;
  organizationName?: string;
}

export interface QoderCnFetchOptions {
  /** 取当前凭证；`renew` 为 true 时要求强制刷新（上游刚回过 401）。 */
  loadCredential(renew: boolean): Promise<QoderCnTransportCredential>;
  /** 目录查询：上游路由键 → 官方元数据。 */
  catalogFor(modelKey: string): QoderCnCatalogEntry;
  /** 模型解析：pi 传来的模型 id → 上游路由键。 */
  resolveModelKey(modelId: string): string;
}

const GATEWAY_TIMEOUT_MS = 10 * 60 * 1_000;

/** 上游给 delta 补的空字段，透传会让客户端渲染出空文本气泡。 */
const NOISE_DELTA_KEYS = ["extra_fields", "refusal", "reasoning_content"];

/** 对话 id（business.id）缓存：同一段对话复用同一组 id / begin_at。 */
const conversationIds = new Map<string, { id: string; beginAt: number }>();

function conversationKey(body: Record<string, unknown>): string | null {
  const messages = Array.isArray(body.messages) ? body.messages : [];
  const system = messages.find((message) => (
    message && typeof message === "object"
    && ((message as { role?: unknown }).role === "system" || (message as { role?: unknown }).role === "developer")
  ));
  const firstUser = messages.find((message) => (
    message && typeof message === "object" && (message as { role?: unknown }).role === "user"
  ));
  if (!firstUser) return null;
  const systemText = typeof (system as { content?: unknown })?.content === "string"
    ? (system as { content: string }).content
    : "";
  const userContent = (firstUser as { content?: unknown }).content;
  const userText = typeof userContent === "string" ? userContent : JSON.stringify(userContent ?? "");
  return createHash("sha256").update(`${systemText}\u0000${userText}`).digest("hex").slice(0, 32);
}

function conversationFor(body: Record<string, unknown>): { id: string; beginAt: number } | undefined {
  const key = conversationKey(body);
  if (!key) return undefined;
  const existing = conversationIds.get(key);
  if (existing) return existing;
  const created = { id: crypto.randomUUID(), beginAt: Date.now() };
  conversationIds.set(key, created);
  // 只保留最近的条目：这是用量归组用的弱缓存，不需要无限长。
  if (conversationIds.size > 200) {
    const oldest = conversationIds.keys().next();
    if (!oldest.done) conversationIds.delete(oldest.value);
  }
  return created;
}

/** 把上游多套缓存字段归一到 OpenAI 标准位，避免客户端恒读 0。 */
function normalizeUsage(usage: Record<string, unknown>): void {
  const details = usage.completion_tokens_details;
  const promptDetails = usage.prompt_tokens_details;
  const target = promptDetails && typeof promptDetails === "object"
    ? { ...(promptDetails as Record<string, unknown>) }
    : {};
  const candidates = [
    usage.cached_tokens,
    usage.prompt_cache_hit_tokens,
    details && typeof details === "object" ? (details as Record<string, unknown>).cached_tokens : undefined,
    target.cached_tokens,
  ];
  let cached = 0;
  for (const candidate of candidates) {
    const value = Number(candidate ?? 0);
    if (Number.isFinite(value) && value > 0) {
      cached = value;
      break;
    }
  }
  if (cached <= 0) return;
  if (target.cached_tokens === cached) return;
  target.cached_tokens = cached;
  usage.prompt_tokens_details = target;
}

/** 清洗一个内层 chunk：去掉空噪声字段，并补标准 usage 位。 */
export function cleanChunk(raw: string): string {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return raw;
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return raw;
  const chunk = parsed as Record<string, unknown>;
  let changed = false;
  if (chunk.usage && typeof chunk.usage === "object") {
    const before = JSON.stringify(chunk.usage);
    normalizeUsage(chunk.usage as Record<string, unknown>);
    if (JSON.stringify(chunk.usage) !== before) changed = true;
  }
  const choices = chunk.choices;
  if (Array.isArray(choices)) {
    for (const choice of choices) {
      if (!choice || typeof choice !== "object") continue;
      const delta = (choice as { delta?: unknown }).delta;
      if (!delta || typeof delta !== "object") continue;
      const record = delta as Record<string, unknown>;
      if (Array.isArray(record.tool_calls) && record.tool_calls.length === 0) {
        delete record.tool_calls;
        changed = true;
      }
      for (const key of NOISE_DELTA_KEYS) {
        if (key in record && !record[key]) {
          delete record[key];
          changed = true;
        }
      }
      const finishReason = (choice as { finish_reason?: unknown }).finish_reason;
      if (Object.keys(record).length === 0 && !finishReason) {
        return "";
      }
    }
  }
  return changed ? JSON.stringify(chunk) : raw;
}

interface EnvelopeFrame {
  status: number;
  body: string;
}

function stripDataPrefix(line: string): string {
  let value = line.trim();
  if (!value || value.startsWith(":")) return "";
  while (value.startsWith("data:")) value = value.slice(5).trim();
  return value.startsWith(":") ? "" : value;
}

/** 解析一帧 SSE 信封；非信封帧（event: finish 之类的计时元数据）返回 null。 */
function parseEnvelopeFrame(frame: string): EnvelopeFrame | null {
  const payload = stripDataPrefix(frame);
  if (!payload) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(payload);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object") return null;
  const record = parsed as { body?: unknown; statusCodeValue?: unknown };
  const status = Number(record.statusCodeValue ?? 200);
  return {
    status: Number.isFinite(status) ? status : 200,
    body: typeof record.body === "string" ? record.body : "",
  };
}

async function readUpstreamError(response: Response): Promise<string> {
  const text = await response.text().catch(() => "");
  if (!text) return `Qoder CN 上游返回 HTTP ${response.status}`;
  const frame = parseEnvelopeFrame(text);
  const detail = frame?.body || text;
  return `Qoder CN 上游返回 HTTP ${response.status}：${detail.slice(0, 600)}`;
}

/**
 * 构造注入给 pi 的 fetch。
 *
 * 入参是 pi 生成的 OpenAI 请求（URL 指向占位 baseUrl、body 是标准 chat
 * completions），出参是包装好的 OpenAI SSE 响应。
 */
export function createQoderCnFetch(options: QoderCnFetchOptions): FetchFunction {
  return async (input, init) => {
    const request = new Request(input as RequestInfo, init as RequestInit);
    const rawBody = await request.text();
    let openAiBody: Record<string, unknown>;
    try {
      const parsed: unknown = rawBody ? JSON.parse(rawBody) : {};
      openAiBody = parsed && typeof parsed === "object" ? parsed as Record<string, unknown> : {};
    } catch {
      return new Response(
        `data: ${JSON.stringify({ error: { message: "Qoder CN：无法解析请求体" } })}\n\n`,
        { status: 400, headers: { "content-type": "text/event-stream" } },
      );
    }

    const requestedModel = typeof openAiBody.model === "string" ? openAiBody.model : "";
    const modelKey = options.resolveModelKey(requestedModel);
    const catalog = options.catalogFor(modelKey);
    const send = async (renew: boolean) => {
      const credential = await options.loadCredential(renew);
      const session = createCosySession({
        uid: credential.uid,
        nickname: credential.nickname,
        accessToken: credential.accessToken,
        refreshToken: credential.refreshToken,
        userType: credential.userType,
        organizationId: credential.organizationId,
        organizationName: credential.organizationName,
      });
      const rawUrl = `${QODER_CN.gateway}${QODER_CN_CHAT_PATH}`;
      const body = buildQoderRequestBody(openAiBody, {
        modelKey,
        catalog,
        userType: credential.userType,
        conversation: conversationFor(openAiBody),
      });
      const response = await fetch(rawUrl, {
        method: "POST",
        headers: session.headers(body, rawUrl, { modelKey, sse: true }),
        body,
        signal: AbortSignal.any([
          AbortSignal.timeout(GATEWAY_TIMEOUT_MS),
          ...(request.signal ? [request.signal] : []),
        ]),
      });
      return { response, credential };
    };

    let attempt = await send(false);
    // 凭证可能刚被上游吊销：强制刷新后再试一次（只重试一次，避免打爆登录链路）。
    if (attempt.response.status === 401 || attempt.response.status === 403) {
      const failed = await readUpstreamError(attempt.response);
      if (/TOKEN_INVALID|TOKEN_EXPIRE|401/.test(failed)) {
        try {
          attempt = await send(true);
        } catch {
          return Response.json({ error: { message: failed } }, { status: 502 });
        }
      } else {
        return Response.json({ error: { message: failed } }, { status: 502 });
      }
    }

    const upstream = attempt.response;
    if (!upstream.ok) {
      return Response.json({ error: { message: await readUpstreamError(upstream) } }, { status: 502 });
    }
    if (!upstream.body) {
      return Response.json(
        { error: { message: "Qoder CN 上游返回了空响应体" } },
        { status: 502 },
      );
    }

    const reader = upstream.body.getReader();
    const decoder = new TextDecoder();
    const encoder = new TextEncoder();
    let pending = "";
    let firstFrame: EnvelopeFrame | null = null;

    /** 从 pending 里取下一帧；返回 null 表示需要更多数据。 */
    const takeFrame = (): string | null => {
      const boundary = /\r\n\r\n|\n\n|\r\r/.exec(pending);
      if (!boundary) return null;
      const end = boundary.index + boundary[0].length;
      const frame = pending.slice(0, boundary.index);
      pending = pending.slice(end);
      return frame;
    };

    // 先读到第一帧：信封里的 statusCodeValue 决定这次请求算成功还是失败，
    // 必须在返回 Response 之前判定，否则错误会伪装成一条空流。
    while (true) {
      const frame = takeFrame();
      if (frame !== null) {
        const envelope = parseEnvelopeFrame(frame);
        if (envelope) {
          firstFrame = envelope;
          break;
        }
        continue;
      }
      const { value, done } = await reader.read();
      pending += decoder.decode(value, { stream: !done });
      if (done) break;
    }

    if (firstFrame && firstFrame.status !== 200) {
      void reader.cancel().catch(() => {});
      return Response.json(
        { error: { message: `Qoder CN 上游返回 statusCodeValue=${firstFrame.status}：${firstFrame.body.slice(0, 600)}` } },
        { status: 502 },
      );
    }

    const stream = new ReadableStream<Uint8Array>({
      async start(controller) {
        const emit = (chunk: string) => {
          if (chunk) controller.enqueue(encoder.encode(chunk));
        };
        const emitEnvelope = (envelope: EnvelopeFrame): boolean => {
          if (envelope.body === "[DONE]") {
            emit("data: [DONE]\n\n");
            return true;
          }
          const cleaned = cleanChunk(envelope.body);
          if (cleaned) emit(`data: ${cleaned}\n\n`);
          return false;
        };

        try {
          if (firstFrame && emitEnvelope(firstFrame)) {
            firstFrame = null;
            void reader.cancel().catch(() => {});
            controller.close();
            return;
          }
          while (true) {
            const frame = takeFrame();
            if (frame === null) {
              const { value, done } = await reader.read();
              pending += decoder.decode(value, { stream: !done });
              if (done) {
                // 上游没发 [DONE] 就直接断流：补一个，避免客户端一直等结束。
                const envelope = parseEnvelopeFrame(pending);
                pending = "";
                if (envelope) emitEnvelope(envelope);
                emit("data: [DONE]\n\n");
                break;
              }
              continue;
            }
            const envelope = parseEnvelopeFrame(frame);
            if (!envelope) continue;
            if (emitEnvelope(envelope)) break;
          }
          controller.close();
        } catch (error) {
          try {
            controller.error(error);
          } catch {
            // controller 已关闭时忽略。
          }
        } finally {
          void reader.cancel().catch(() => {});
        }
      },
      cancel() {
        void reader.cancel().catch(() => {});
      },
    });

    return new Response(stream, {
      status: 200,
      headers: { "content-type": "text/event-stream", "cache-control": "no-cache" },
    });
  };
}
