/**
 * 把 Qoder CN 注册成 pi 的一个「订阅」provider。
 *
 * 组合方式（与 SDK 的扩展 provider 契约一致）：
 *   - `oauth`：设备码登录 + token 刷新，登录后凭证存进 auth.json，界面「订阅」
 *     区据此显示已登录状态；
 *   - `models`：内置官方目录快照，未登录也有模型可选；
 *   - `refreshModels`：登录后用 COSY 签名拉官方清单，覆盖内置快照；
 *   - 真正的请求改写 / 签名 / SSE 信封解包在 `transport.ts`，由
 *     `createDesktopModelRuntime()` 按 provider 注入 fetch。
 *
 * 凭证之外的账号身份（uid / 昵称 / 用户类型）不在 pi 的 AuthResult 里，因此
 * 这里用 access token 做键做一层进程内缓存；缓存未命中时用 userinfo 兜底
 * （应用重启后第一次请求会多打一次身份接口）。
 */
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";
import type { ProviderModelConfig } from "@earendil-works/pi-coding-agent";
import type { Api } from "@earendil-works/pi-ai";
import {
  QODER_CN_BASE_URL,
  QODER_CN_DISPLAY_NAME,
  QODER_CN_PROVIDER_ID,
} from "./constants";
import {
  QODER_CN_STATIC_CATALOG,
  type QoderCnCatalogEntry,
  fetchQoderCnCatalog,
  toPiModel,
} from "./catalog";
import type { QoderCnIdentityFields } from "./auth";
import { fetchQoderCnIdentity, refreshQoderCnToken, startQoderCnDeviceLogin } from "./auth";
import type { QoderCnTransportCredential } from "./transport";

/** access token → 账号身份（登录/刷新时写入，进程内有效）。 */
const identities = new Map<string, QoderCnIdentityFields>();

function rememberIdentity(token: string, identity: QoderCnIdentityFields): void {
  if (!token || !identity.uid) return;
  identities.set(token, identity);
  // 只保留最近登录过的若干账号，避免长时间运行后无限增长。
  if (identities.size > 8) {
    const oldest = identities.keys().next();
    if (!oldest.done) identities.delete(oldest.value);
  }
}

/** 目录：内置快照 + 本次运行中拉到的在线清单（在线优先）。 */
let liveCatalog: QoderCnCatalogEntry[] | null = null;

export function qoderCnCatalogEntry(modelId: string): QoderCnCatalogEntry {
  const pools = liveCatalog ? [liveCatalog, QODER_CN_STATIC_CATALOG] : [QODER_CN_STATIC_CATALOG];
  for (const pool of pools) {
    const hit = pool.find((entry) => entry.key === modelId);
    if (hit) return hit;
  }
  // 未知 id（比如手写在 enabledModels 里的别名）：按「非推理、纯文本」的保守元数据下发，
  // 上游对未知 key 会回落 auto，至少不会因为缺字段被拒。
  return {
    key: modelId,
    displayName: modelId,
    vision: false,
    reasoning: false,
    maxInputTokens: 180_000,
    efforts: [],
  };
}

function toModelConfig(entry: QoderCnCatalogEntry): ProviderModelConfig {  const model = toPiModel(entry);
  return {
    id: model.id,
    name: model.name,
    type: "chat",
    api: model.api,
    baseUrl: QODER_CN_BASE_URL,
    reasoning: model.reasoning,
    input: model.input,
    cost: model.cost,
    contextWindow: model.contextWindow,
    maxTokens: model.maxTokens,
    ...(model.thinkingLevelMap ? { thinkingLevelMap: model.thinkingLevelMap } : {}),
  };
}

/**
 * 内置目录对应的 provider 模型配置。
 *
 * refreshModels 在未登录时会把这份列表原样返回——composer 把 refreshModels 的
 * 返回值当作「新目录」发布，返回空数组会把静态目录清空。
 */
const STATIC_MODEL_CONFIGS: ProviderModelConfig[] = QODER_CN_STATIC_CATALOG.map(toModelConfig);

function identityFromCredential(credential: Record<string, unknown>): QoderCnIdentityFields | null {
  const uid = typeof credential.uid === "string" ? credential.uid : "";
  if (!uid) return null;
  return {
    uid,
    nickname: typeof credential.nickname === "string" ? credential.nickname : "",
    userType: typeof credential.userType === "string" ? credential.userType : "",
    organizationId: typeof credential.organizationId === "string" ? credential.organizationId : "",
    organizationName: typeof credential.organizationName === "string" ? credential.organizationName : "",
  };
}

/**
 * 取「可用的上游凭证」：access token + 账号身份。
 *
 * `renew` 为真时用极大的最小有效期阈值逼 pi 走一次 OAuth 刷新（刷新在凭证
 * 存储锁内完成，多个并发请求只会刷一次）。
 */
export async function resolveQoderCnCredential(
  modelRuntime: ModelRuntime,
  options: { renew?: boolean; signal?: AbortSignal } = {},
): Promise<QoderCnTransportCredential> {
  const auth = await modelRuntime.getAuth(QODER_CN_PROVIDER_ID, {
    ...(options.renew ? { minOAuthValidityMs: 10 * 365 * 24 * 3600 * 1000 } : {}),
    ...(options.signal ? { signal: options.signal } : {}),
  });
  const accessToken = auth?.auth.apiKey;
  if (!accessToken) {
    throw new Error("Qoder CN 尚未登录：请到「模型 → 订阅」里完成登录");
  }
  let identity = identities.get(accessToken) ?? null;
  if (!identity) {
    const fetched = await fetchQoderCnIdentity(
      accessToken,
      options.signal ?? new AbortController().signal,
    );
    identity = {
      uid: fetched.uid ?? "",
      nickname: fetched.nickname ?? "",
      userType: fetched.userType ?? "",
      organizationId: fetched.organizationId ?? "",
      organizationName: fetched.organizationName ?? "",
    };
    if (identity.uid) rememberIdentity(accessToken, identity);
  }
  return {
    uid: identity.uid,
    nickname: identity.nickname,
    accessToken,
    userType: identity.userType,
    organizationId: identity.organizationId,
    organizationName: identity.organizationName,
  };
}

/** 注册 Qoder CN provider（幂等：已注册则跳过）。 */
export function registerQoderCnProvider(modelRuntime: ModelRuntime): void {
  if (modelRuntime.getProvider(QODER_CN_PROVIDER_ID)) return;

  modelRuntime.registerProvider(QODER_CN_PROVIDER_ID, {
    name: QODER_CN_DISPLAY_NAME,
    baseUrl: QODER_CN_BASE_URL,
    // 请求体由 transport 整体重写；这里声明 openai-completions 是为了让 SDK
    // 用它那套成熟的 chunk 解析（工具调用、usage、思考内容）消费我们的流。
    api: "openai-completions" as Api,
    authHeader: true,
    oauth: {
      name: "Qoder CN (subscription)",
      isSubscription: true,
      async login(callbacks) {
        const device = await startQoderCnDeviceLogin();
        callbacks.onAuth({
          url: device.authUrl,
          instructions: "在浏览器里确认登录后会自动继续（最长等待 5 分钟）",
        });
        const signal = callbacks.signal ?? new AbortController().signal;
        const result = await device.poll(signal);
        rememberIdentity(result.access, {
          uid: result.uid,
          nickname: result.nickname,
          userType: result.userType,
          organizationId: result.organizationId,
          organizationName: result.organizationName,
        });
        return {
          type: "oauth",
          access: result.access,
          refresh: result.refresh,
          expires: result.expires,
          uid: result.uid,
          nickname: result.nickname,
          userType: result.userType,
          organizationId: result.organizationId,
          organizationName: result.organizationName,
        };
      },
      async refreshToken(credential, signal) {
        const refreshed = await refreshQoderCnToken(credential.refresh, signal);
        const identity = identityFromCredential(credential);
        if (identity) rememberIdentity(refreshed.access, identity);
        return {
          ...credential,
          access: refreshed.access,
          refresh: refreshed.refresh,
          expires: refreshed.expires,
        };
      },
      getApiKey(credential) {
        return credential.access;
      },
    },
    models: STATIC_MODEL_CONFIGS,
    async refreshModels(context) {
      const credential = context.credential;
      // 注意：返回空数组等于把静态目录清空（composer 会把 [] 当成新目录发布），
      // 所以未登录 / 拉取失败时必须把内置快照原样返回。
      if (!credential || credential.type !== "oauth") return STATIC_MODEL_CONFIGS;
      const stored = credential as unknown as Record<string, unknown>;
      const identity = identityFromCredential(stored);
      const access = typeof stored.access === "string" ? stored.access : "";
      if (!access) return STATIC_MODEL_CONFIGS;
      const uid = identity?.uid
        ?? identities.get(access)?.uid
        ?? (await fetchQoderCnIdentity(access, context.signal)).uid
        ?? "";
      if (!uid) return STATIC_MODEL_CONFIGS;
      try {
        const catalog = await fetchQoderCnCatalog({
          uid,
          nickname: identity?.nickname,
          accessToken: access,
          refreshToken: typeof stored.refresh === "string" ? stored.refresh : undefined,
          userType: identity?.userType,
          organizationId: identity?.organizationId,
          organizationName: identity?.organizationName,
        }, context.signal);
        if (catalog.length === 0) return STATIC_MODEL_CONFIGS;
        liveCatalog = catalog;
        return catalog.map(toModelConfig);
      } catch {
        // 清单拉取失败不该让 provider 整体不可用：继续用内置快照。
        return STATIC_MODEL_CONFIGS;
      }
    },
  });
}

/** 供 transport 用的身份缓存查询（避免重复请求 userinfo）。 */
export function cachedQoderCnIdentity(token: string): QoderCnIdentityFields | undefined {
  return identities.get(token);
}
