import {
  ModelRuntime,
  type CreateModelRuntimeOptions,
} from "@earendil-works/pi-coding-agent";
import { parseDiscoveredModels } from "./model-discovery";
import { APISETS_BASE_URL, APISETS_PROVIDER_ID } from "./desktop-provider-constants";
import { getModelsDevCatalog } from "./models-dev-discovery";
import type { ModelCatalogEntry } from "./model-catalog";
import { responsesTerminalFetch } from "./responses-terminal-fetch";
import { isModelType } from "@earendil-works/pi-ai";
import type { AnyModel, Api, FetchFunction, Model, Provider } from "@earendil-works/pi-ai";
import { invalidateModelsCache } from "./models-cache";
import { QODER_CN_PROVIDER_ID } from "./qoder-cn/constants";
import { qoderCnCatalogEntry, registerQoderCnProvider, resolveQoderCnCredential } from "./qoder-cn/provider";
import { createQoderCnFetch } from "./qoder-cn/transport";

function withResponsesFetch<T extends { fetch?: FetchFunction }>(options: T | undefined): T {
  return { ...options, fetch: responsesTerminalFetch(options?.fetch) } as T;
}

/**
 * Qoder CN 走的是 COSY 签名的 agent 端点：请求体、鉴权头、响应信封都要改写。
 * 这些全部集中在 fetch 里做，SDK 的 OpenAI 适配器只负责解析解包后的 chunk。
 */
function withQoderCnFetch<T extends { fetch?: FetchFunction }>(
  modelRuntime: ModelRuntime,
  options: T | undefined,
): T {
  return {
    ...options,
    fetch: createQoderCnFetch({
      loadCredential: (renew) => resolveQoderCnCredential(modelRuntime, { renew }),
      catalogFor: qoderCnCatalogEntry,
      // pi 侧模型 id 就是上游路由键（目录里的 key 直接暴露），不需要再映射。
      resolveModelKey: (modelId) => modelId,
    }),
  } as T;
}

export { APISETS_BASE_URL, APISETS_PROVIDER_ID } from "./desktop-provider-constants";

const DEEPSEEK_PROVIDER_ID = "deepseek";
const DEEPSEEK_BASE_URL = "https://api.deepseek.com";
const OPENROUTER_PROVIDER_ID = "openrouter";

function modelsDevDeepSeekModels(
  entries: readonly ModelCatalogEntry[],
  defaults: readonly Model<Api>[],
): Model<Api>[] {
  const providerEntries = entries.filter((entry) => entry.providerId.toLowerCase() === DEEPSEEK_PROVIDER_ID);
  const reasoningDefault = defaults.find((model) => model.reasoning) ?? defaults[0];

  return providerEntries.map((entry) => {
    const exactDefault = defaults.find((model) => model.id === entry.id);
    const compatibilityDefault = exactDefault ?? reasoningDefault;
    const input = (entry.input ?? ["text"])
      .filter((modality): modality is "text" | "image" => modality === "text" || modality === "image");

    return {
      id: entry.id,
      name: entry.name,
      api: "openai-completions",
      provider: DEEPSEEK_PROVIDER_ID,
      baseUrl: DEEPSEEK_BASE_URL,
      headers: undefined,
      reasoning: entry.reasoning ?? false,
      input: input.length > 0 ? input : ["text"],
      cost: {
        input: entry.cost.input ?? 0,
        output: entry.cost.output ?? 0,
        cacheRead: entry.cost.cacheRead ?? 0,
        cacheWrite: entry.cost.cacheWrite ?? 0,
      },
      contextWindow: entry.contextWindow ?? 128_000,
      maxTokens: entry.maxTokens ?? 16_384,
      ...(compatibilityDefault?.compat ? { compat: compatibilityDefault.compat } : {}),
      ...(entry.reasoning && compatibilityDefault?.thinkingLevelMap
        ? { thinkingLevelMap: compatibilityDefault.thinkingLevelMap }
        : {}),
    };
  });
}

function modelsDevDeepSeekProvider(base: Provider<Api>): Provider<Api> {
  let models = [...base.getModels()];

  return {
    ...base,
    baseUrl: DEEPSEEK_BASE_URL,
    getModels: () => models,
    async refreshModels(context) {
      // 目录来自 pi.sqlite 缓存时这里不会碰到网络；只有缓存完全缺失或显式
      // force（用户主动刷新）时才会等待 models.dev。
      const catalog = await getModelsDevCatalog({
        signal: context.signal,
        force: context.force,
        ...(context.allowNetwork ? {} : { offline: true }),
      });
      const refreshed = modelsDevDeepSeekModels(catalog, models);
      if (refreshed.length === 0) {
        if (!context.allowNetwork) {
          // 离线刷新只负责恢复：本地缓存的 models-store.json 优先，其次是目录缓存。
          if (context.stored?.etag === "models.dev") {
            const stored = context.stored.models
              .filter((model) => model.provider === DEEPSEEK_PROVIDER_ID)
              .filter((model): model is Model<Api> => isModelType(model, "chat"));
            await context.publish({ update: () => { models = [...stored]; } });
          }
          return;
        }
        throw new Error("models.dev did not return any DeepSeek models");
      }
      await context.publish({
        // 离线刷新只负责恢复内存态，绝不写 models-store.json。
        ...(context.allowNetwork
          ? { persist: { models: refreshed, checkedAt: Date.now(), etag: "models.dev" } }
          : {}),
        update: () => { models = refreshed; },
      });
    },
  };
}

function apiSetsModels(payload: unknown): Model<Api>[] {
  return parseDiscoveredModels(payload).map((model) => ({
    id: model.id,
    name: model.name ?? model.id,
    // provider/baseUrl 必须随模型一起持久化：恢复 models-store.json 时要按
    // provider 过滤，SDK 也会直接采用这里给出的协议（见 lib/desktop-providers.ts 注释）。
    provider: APISETS_PROVIDER_ID,
    // Coding exposes GPT through Responses, matching Codex's wire protocol.
    api: model.id.startsWith("gpt-") ? "openai-responses" : "anthropic-messages",
    baseUrl: APISETS_BASE_URL,
    reasoning: true,
    input: ["text", "image"] as ("text" | "image")[],
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    contextWindow: model.contextWindow ?? 128_000,
    maxTokens: model.maxTokens ?? 16_384,
    ...(model.id === "glm-5.3" ? { compat: { allowEmptySignature: true } } : {}),
  }));
}

/** providers/apisets 没有静态模型表，只认上游 /v1/models；etag 用来标记缓存来源。 */
const APISETS_CATALOG_ETAG = "apisets-models-api-v2";
/** 上游目录很少变，没必要每次打开设置都打一次（用户仍可用 force 刷新）。 */
const APISETS_REFRESH_INTERVAL_MS = 30 * 60 * 1000;

function apiSetsStoredModels(
  stored: { models: readonly AnyModel[]; etag?: string } | undefined,
): Model<Api>[] {
  if (stored?.etag !== APISETS_CATALOG_ETAG) return [];
  // 自 pi 1.0 起 models-store.json 保存「任意类型」的目录（chat / image /
  // classifier），这里只认属于本 provider 的 chat 模型。
  return stored.models
    .filter((model) => model.provider === APISETS_PROVIDER_ID)
    .filter((model): model is Model<Api> => isModelType(model, "chat"));
}

/** Register providers shipped by Pi Desktop on a runtime instance. */
export function registerDesktopProviders(modelRuntime: ModelRuntime): void {
  if (!modelRuntime.getProvider(APISETS_PROVIDER_ID)) {
    modelRuntime.registerProvider(APISETS_PROVIDER_ID, {
      name: "Coding",
      baseUrl: APISETS_BASE_URL,
      api: "anthropic-messages",
      authHeader: true,
      async refreshModels(context) {
        const cached = apiSetsStoredModels(context.stored);
        if (!context.allowNetwork || context.signal.aborted) return cached;

        const recentlyChecked = context.stored?.etag === APISETS_CATALOG_ETAG
          && context.stored.checkedAt !== undefined
          && Date.now() - context.stored.checkedAt < APISETS_REFRESH_INTERVAL_MS;
        if (!context.force && recentlyChecked) return cached;

        const credential = context.credential;
        if (credential?.type !== "api_key" || !credential.key) return cached;

        const response = await fetch(`${APISETS_BASE_URL}/v1/models`, {
          headers: {
            Accept: "application/json",
            Authorization: `Bearer ${credential.key}`,
          },
          signal: context.signal,
        });
        if (!response.ok) throw new Error(`APIsets model catalog returned HTTP ${response.status}`);
        const models = apiSetsModels(await response.json());
        if (models.length === 0) throw new Error("APIsets model catalog did not return any models");

        // 持久化目录：新进程/新 runtime 在 cache-only 阶段就能拿到模型列表，
        // 不必等上游请求（见 createDesktopModelRuntime 与 refreshDesktopProviderCatalogs）。
        await context.publish({
          persist: { models, checkedAt: Date.now(), etag: APISETS_CATALOG_ETAG },
          update: () => {},
        });
        return models;
      },
    });
  }

  const deepSeekProvider = modelRuntime.getProvider(DEEPSEEK_PROVIDER_ID);
  if (deepSeekProvider) modelRuntime.registerNativeProvider(modelsDevDeepSeekProvider(deepSeekProvider));

  // Qoder 国内版订阅：设备码登录拿 token，推理走 COSY 签名的 agent 端点。
  registerQoderCnProvider(modelRuntime);
}

/** Create a standalone runtime with Pi Desktop's built-in providers registered. */
export async function createDesktopModelRuntime(options?: CreateModelRuntimeOptions): Promise<ModelRuntime> {
  const modelRuntime = await ModelRuntime.create(options);
  const stream = modelRuntime.stream.bind(modelRuntime);
  const streamSimple = modelRuntime.streamSimple.bind(modelRuntime);
  modelRuntime.stream = (model, context, requestOptions) => {
    if (model.provider === QODER_CN_PROVIDER_ID) {
      return stream(model, context, withQoderCnFetch(modelRuntime, requestOptions));
    }
    return stream(model, context,
      model.api === "openai-responses"
        ? withResponsesFetch(requestOptions)
        : requestOptions);
  };
  modelRuntime.streamSimple = (model, context, requestOptions) => {
    if (model.provider === QODER_CN_PROVIDER_ID) {
      return streamSimple(model, context, withQoderCnFetch(modelRuntime, requestOptions));
    }
    return streamSimple(model, context,
      model.api === "openai-responses"
        ? withResponsesFetch(requestOptions)
        : requestOptions);
  };
  registerDesktopProviders(modelRuntime);
  // Cache-only 恢复应用自带的服务商目录：apisets 只认上游 /v1/models，没有
  // models-store.json 缓存就会一直接不到。registerProvider/registerNativeProvider
  // 会各自调度一个后台 cache-only 刷新，这里 await 自己的，避免跟调用方的
  // 立刻网络刷新竞争。
  await modelRuntime.refresh({
    allowNetwork: false,
    providers: [APISETS_PROVIDER_ID, DEEPSEEK_PROVIDER_ID],
  });
  return modelRuntime;
}

export interface DesktopCatalogRefreshResult {
  changedProviders: string[];
}

function modelCatalogSnapshot(modelRuntime: ModelRuntime, provider: string): string {
  return JSON.stringify(modelRuntime.getModels(provider));
}

/** Refresh application-owned catalogs and Pi 1.2's credential-scoped OpenRouter catalog. */
export async function refreshDesktopProviderCatalogs(
  modelRuntime: ModelRuntime,
  signal?: AbortSignal,
): Promise<DesktopCatalogRefreshResult> {
  const providers = [APISETS_PROVIDER_ID, DEEPSEEK_PROVIDER_ID, OPENROUTER_PROVIDER_ID];
  const before = new Map(providers.map((provider) => [provider, modelCatalogSnapshot(modelRuntime, provider)]));

  await modelRuntime.refresh({
    allowNetwork: true,
    providers,
    ...(signal ? { signal } : {}),
  });

  const changedProviders = providers.filter((provider) => (
    before.get(provider) !== modelCatalogSnapshot(modelRuntime, provider)
  ));
  if (changedProviders.length > 0) invalidateModelsCache();
  return { changedProviders };
}
