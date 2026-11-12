/**
 * Qoder CN 的模型目录。
 *
 * 官方没有 OpenAI 形态的 /v1/models，模型清单来自 COSY 签名的
 * `GET /algo/api/v2/model/list?Encode=1`。这里有两级来源，与 qoder2api-hub
 * 国内版一致：
 *
 *   1. 在线清单（需要已登录凭证）—— 官方改显示名 / 上下架后立刻跟上
 *   2. 内置静态快照（下面这张表）—— 未登录、离线或接口失败时的兜底
 *
 * 表里的 `key` 是上游真正的路由键（`x-model-key` 头与 body 的 model_config.key
 * 都用它），不是给人看的名字：上游对未知 key 不报错，会**静默回落到 auto**，
 * 所以 key 必须逐字来自官方目录。
 */
import type { Model, Api, ThinkingLevelMap } from "@earendil-works/pi-ai";
import { QODER_CN_BASE_URL, QODER_CN_PROVIDER_ID } from "./constants";
import { createCosySession } from "./cosy";
import { encodeQoderBase64 } from "./qoder-base64";
import { QODER_CN, QODER_CN_MODELS_PATH } from "./constants";

export interface QoderCnCatalogEntry {
  /** 上游路由键。 */
  key: string;
  displayName: string;
  /** 是否支持图片输入。 */
  vision: boolean;
  reasoning: boolean;
  /** 官方给出的最大输入 token（上下文窗口的保守默认值）。 */
  maxInputTokens: number;
  /** 支持的思考档位（上游 effort 原值）。 */
  efforts: string[];
  isDefault?: boolean;
}

/** 官方国内版目录快照（顺序即官方 UI 顺序）。 */
export const QODER_CN_STATIC_CATALOG: readonly QoderCnCatalogEntry[] = [
  { key: "auto", displayName: "Auto", vision: true, reasoning: true, maxInputTokens: 200_000, efforts: [], isDefault: true },
  { key: "qmodel_38max", displayName: "Qwen3.8-Max", vision: true, reasoning: true, maxInputTokens: 180_000, efforts: ["low", "medium", "xhigh"] },
  { key: "qfmodel", displayName: "Qwen3.8-Flash", vision: true, reasoning: true, maxInputTokens: 180_000, efforts: ["low", "medium", "xhigh"] },
  { key: "qmodel_latest", displayName: "Qwen3.7-Max", vision: true, reasoning: true, maxInputTokens: 180_000, efforts: [] },
  { key: "qmodel", displayName: "Qwen3.7-Plus", vision: true, reasoning: true, maxInputTokens: 180_000, efforts: [] },
  { key: "q37fmodel", displayName: "Qwen3.7-Flash", vision: true, reasoning: true, maxInputTokens: 180_000, efforts: [] },
  { key: "dmodel", displayName: "DeepSeek-V4-Pro", vision: true, reasoning: true, maxInputTokens: 96_000, efforts: ["high", "max"] },
  { key: "dfmodel", displayName: "DeepSeek-Flash", vision: true, reasoning: false, maxInputTokens: 180_000, efforts: ["low", "high", "max"] },
  { key: "gmodel", displayName: "GLM-5.3", vision: true, reasoning: true, maxInputTokens: 180_000, efforts: ["low", "high", "max"] },
  { key: "gfmodel", displayName: "GLM-5.3-Flash", vision: true, reasoning: true, maxInputTokens: 1_000_000, efforts: ["high", "max"] },
  { key: "gm51model", displayName: "GLM-5.2", vision: true, reasoning: true, maxInputTokens: 180_000, efforts: ["high", "max"] },
  { key: "kmodel_latest", displayName: "Kimi-K3", vision: true, reasoning: false, maxInputTokens: 180_000, efforts: ["low", "high", "max"] },
  { key: "kmodel", displayName: "Kimi-K2.8-Preview", vision: true, reasoning: true, maxInputTokens: 180_000, efforts: ["low", "high", "max"] },
  { key: "mmodel", displayName: "MiniMax-M2.7", vision: false, reasoning: false, maxInputTokens: 180_000, efforts: [] },
  { key: "lmodel", displayName: "QoDuck", vision: true, reasoning: true, maxInputTokens: 180_000, efforts: ["low", "high", "xhigh"] },
] as const;

/** 上游输出上限的保守默认值（官方请求体骨架里的 parameters.max_tokens）。 */
const DEFAULT_MAX_OUTPUT_TOKENS = 32_768;

/** pi 的思考档位 → 上游 effort 值；不支持的档位显式映射为 null。 */
function thinkingLevelMap(entry: QoderCnCatalogEntry): ThinkingLevelMap | undefined {
  if (!entry.reasoning) return undefined;
  const supported = new Set(entry.efforts);
  if (supported.size === 0) {
    // 目录没给档位的 reasoning 模型：把 pi 的各档都按上游默认处理。
    return { off: null, minimal: null, low: null, medium: null, high: null, xhigh: null, max: null };
  }
  const map: ThinkingLevelMap = {};
  for (const level of ["off", "minimal", "low", "medium", "high", "xhigh", "max"] as const) {
    map[level] = supported.has(level) ? level : null;
  }
  return map;
}

/** 目录条目 → pi 的模型定义。 */
export function toPiModel(entry: QoderCnCatalogEntry): Model<Api> {
  const thinking = thinkingLevelMap(entry);
  return {
    id: entry.key,
    name: entry.displayName,
    api: "openai-completions",
    provider: QODER_CN_PROVIDER_ID,
    baseUrl: QODER_CN_BASE_URL,
    reasoning: entry.reasoning,
    input: entry.vision ? ["text", "image"] : ["text"],
    // 订阅制：额度由 Qoder 账号套餐承担，这里按 0 记账，避免把套餐算成余额消耗。
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    contextWindow: entry.maxInputTokens,
    maxTokens: DEFAULT_MAX_OUTPUT_TOKENS,
    ...(thinking ? { thinkingLevelMap: thinking } : {}),
  };
}

export function staticCatalogModels(): Model<Api>[] {
  return QODER_CN_STATIC_CATALOG.map(toPiModel);
}

/** 按模型 id（上游 key）取目录元数据；未知 id 返回 undefined。 */
export function catalogEntry(
  models: readonly QoderCnCatalogEntry[],
  modelId: string,
): QoderCnCatalogEntry | undefined {
  return models.find((entry) => entry.key === modelId);
}

interface RemoteCatalogRow {
  key?: unknown;
  display_name?: unknown;
  is_vl?: unknown;
  is_reasoning?: unknown;
  max_input_tokens?: unknown;
  thinking_config?: unknown;
  enable?: unknown;
}

function remoteEfforts(row: RemoteCatalogRow): string[] {
  const thinking = row.thinking_config;
  if (!thinking || typeof thinking !== "object") return [];
  const enabled = (thinking as Record<string, unknown>).enabled;
  if (!enabled || typeof enabled !== "object") return [];
  const efforts = (enabled as Record<string, unknown>).efforts;
  if (!efforts || typeof efforts !== "object") return [];
  return Object.keys(efforts as Record<string, unknown>);
}

/**
 * 把上游清单响应转成目录条目。
 *
 * 只取 `chat` / `app` 两个场景（官方把对话模型分在这两处），其余场景
 * （inline / assistant / quest / BYOK）语义不同，混进来会把权限和定价搞乱。
 */
export function parseCatalogPayload(payload: unknown): QoderCnCatalogEntry[] {
  if (!payload || typeof payload !== "object") return [];
  const source = payload as Record<string, unknown>;
  const rows: QoderCnCatalogEntry[] = [];
  const seen = new Set<string>();
  for (const scene of ["chat", "app"]) {
    const candidates = source[scene];
    if (!Array.isArray(candidates)) continue;
    for (const candidate of candidates) {
      if (!candidate || typeof candidate !== "object") continue;
      const row = candidate as RemoteCatalogRow;
      const key = typeof row.key === "string" ? row.key.trim() : "";
      if (!key || seen.has(key)) continue;
      if (row.enable === false) continue;
      seen.add(key);
      rows.push({
        key,
        displayName: typeof row.display_name === "string" && row.display_name ? row.display_name : key,
        vision: Boolean(row.is_vl),
        reasoning: Boolean(row.is_reasoning),
        maxInputTokens: typeof row.max_input_tokens === "number" && row.max_input_tokens > 0
          ? row.max_input_tokens
          : 180_000,
        efforts: remoteEfforts(row),
      });
    }
  }
  return rows;
}

export interface QoderCnCatalogCredential {
  uid: string;
  nickname?: string;
  accessToken: string;
  refreshToken?: string;
  userType?: string;
  organizationId?: string;
  organizationName?: string;
}

/**
 * COSY 签名拉取官方模型清单。
 *
 * 注意签名体必须是 `encodeQoderBase64("{}")`，而且请求要**真的带上这串 body**
 * ——服务端会校验签名与 body 一致，裸 GET 直接 403。
 */
export async function fetchQoderCnCatalog(
  credential: QoderCnCatalogCredential,
  signal: AbortSignal,
): Promise<QoderCnCatalogEntry[]> {
  const session = createCosySession({
    uid: credential.uid,
    nickname: credential.nickname,
    accessToken: credential.accessToken,
    refreshToken: credential.refreshToken,
    userType: credential.userType,
    organizationId: credential.organizationId,
    organizationName: credential.organizationName,
  });
  const rawUrl = `${QODER_CN.gateway}${QODER_CN_MODELS_PATH}`;
  const body = encodeQoderBase64("{}");
  const response = await fetch(rawUrl, {
    method: "GET",
    headers: session.headers(body, rawUrl, { sse: false, accept: "application/json" }),
    body,
    signal: AbortSignal.any([AbortSignal.timeout(20_000), signal]),
  });
  if (!response.ok) {
    throw new Error(`Qoder CN 模型清单拉取失败：HTTP ${response.status}`);
  }
  return parseCatalogPayload(await response.json());
}
