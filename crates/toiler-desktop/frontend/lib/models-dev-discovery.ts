import {
  flattenModelsDevCatalog,
  type ModelCatalogEntry,
} from "./model-catalog";
import {
  readModelsDevCatalog,
  writeModelsDevCatalog,
  type StoredModelsDevCatalog,
} from "./models-dev-catalog-store";

const MODELS_DEV_API = "https://models.dev/api.json";
const MODELS_DEV_MODELS = "https://models.dev/models.json";
const MODELS_DEV_TIMEOUT_MS = 15_000;
/**
 * 内存缓存新鲜期：命中后连数据库都不读。
 */
const MODELS_DEV_MEMORY_TTL_MS = 30 * 60 * 1000;
/**
 * 数据库缓存新鲜期：命中即直接返回，不访问 models.dev。
 *
 * models.dev/api.json 有数 MB，在网络不佳时单次请求要好几秒；打开设置页
 * 时等待它会让模型/服务商列表空白一段时间，所以磁盘缓存按天复用，过期后
 * 也只是先用旧目录渲染、再静默刷新（见 refreshCatalogInBackground）。
 */
const MODELS_DEV_DISK_TTL_MS = 24 * 60 * 60 * 1000;
/** 后台刷新失败后的退避时间，避免每个请求都重发数 MB 的目录请求。 */
const MODELS_DEV_RETRY_MS = 5 * 60 * 1000;

interface ModelsDevCache {
  entries: ModelCatalogEntry[];
  /** 内存条目的有效期：到期后重新检查磁盘缓存。 */
  expiresAt: number;
  /** 条目是否仍处于磁盘新鲜期内；过期条目每次使用都会尝试后台刷新。 */
  diskFresh: boolean;
}

/** 可替换的缓存后端，测试用它指向临时数据库。 */
export interface ModelsDevCatalogStore {
  read(): StoredModelsDevCatalog | null;
  write(entries: readonly ModelCatalogEntry[]): void;
}

declare global {
  var __piModelsDevCatalogCache: ModelsDevCache | undefined;
  var __piModelsDevCatalogRefresh: Promise<unknown> | undefined;
  var __piModelsDevCatalogRetryAt: number | undefined;
  var __piModelsDevCatalogStore: ModelsDevCatalogStore | undefined;
}

export interface ModelsDevCatalogOptions {
  signal?: AbortSignal;
  /** 忽略内存与数据库缓存，直接访问 models.dev。 */
  force?: boolean;
  /** 只用缓存，绝不访问网络。 */
  offline?: boolean;
}

function defaultStore(): ModelsDevCatalogStore {
  return {
    read: () => readModelsDevCatalog(),
    write: (entries) => writeModelsDevCatalog(entries),
  };
}

function getStore(): ModelsDevCatalogStore {
  return globalThis.__piModelsDevCatalogStore ??= defaultStore();
}

function rememberInMemory(entries: ModelCatalogEntry[], diskFresh: boolean): void {
  globalThis.__piModelsDevCatalogCache = {
    entries,
    expiresAt: Date.now() + MODELS_DEV_MEMORY_TTL_MS,
    diskFresh,
  };
}

function requestSignal(signal?: AbortSignal): AbortSignal {
  const timeout = AbortSignal.timeout(MODELS_DEV_TIMEOUT_MS);
  return signal ? AbortSignal.any([signal, timeout]) : timeout;
}

async function fetchCatalog(url: string, signal?: AbortSignal): Promise<ModelCatalogEntry[]> {
  const response = await fetch(url, { signal: requestSignal(signal) });
  if (!response.ok) throw new Error(`models.dev returned HTTP ${response.status}`);
  const entries = flattenModelsDevCatalog(await response.json());
  if (entries.length === 0) throw new Error("models.dev returned an empty model catalog");
  return entries;
}

/**
 * 强制访问 models.dev 并写入缓存。
 *
 * 依次尝试按 provider 分组的 api.json 与扁平 models.json。
 */
export async function refreshModelsDevCatalog(signal?: AbortSignal): Promise<ModelCatalogEntry[]> {
  const errors: unknown[] = [];
  try {
    const entries = await fetchCatalog(MODELS_DEV_API, signal);
    rememberInMemory(entries, true);
    getStore().write(entries);
    return entries;
  } catch (error) {
    errors.push(error);
  }

  if (signal?.aborted) throw signal.reason;

  try {
    const entries = await fetchCatalog(MODELS_DEV_MODELS, signal);
    rememberInMemory(entries, true);
    getStore().write(entries);
    return entries;
  } catch (error) {
    errors.push(error);
    throw new AggregateError(errors, "Unable to load the models.dev model catalog");
  }
}

/** 后台静默刷新，同一进程内的并发调用共享同一个任务。 */
function refreshCatalogInBackground(): void {
  if (globalThis.__piModelsDevCatalogRefresh) return;
  const retryAt = globalThis.__piModelsDevCatalogRetryAt;
  if (retryAt !== undefined && retryAt > Date.now()) return;

  const task = refreshModelsDevCatalog()
    .then(() => {
      globalThis.__piModelsDevCatalogRetryAt = undefined;
    })
    .catch(() => {
      globalThis.__piModelsDevCatalogRetryAt = Date.now() + MODELS_DEV_RETRY_MS;
    })
    .finally(() => {
      if (globalThis.__piModelsDevCatalogRefresh === task) {
        globalThis.__piModelsDevCatalogRefresh = undefined;
      }
    });
  globalThis.__piModelsDevCatalogRefresh = task;
}

/**
 * 读取 models.dev 目录，按「内存 → 数据库 → 网络」顺序取用。
 *
 * 数据库命中时立即返回：过期数据同样先返回，同时触发一次后台刷新，因此
 * 界面列表不会等待 models.dev 的响应。
 */
export async function getModelsDevCatalog(
  options: ModelsDevCatalogOptions = {},
): Promise<ModelCatalogEntry[]> {
  const now = Date.now();
  const cached = globalThis.__piModelsDevCatalogCache;
  if (!options.force && cached && cached.expiresAt > now) {
    // 过期的磁盘条目仍可继续服务，但每次使用都试着补一次后台刷新。
    if (!options.offline && !cached.diskFresh) refreshCatalogInBackground();
    return cached.entries;
  }

  const stored = options.force ? null : getStore().read();
  if (stored) {
    const diskFresh = stored.fetchedAt + MODELS_DEV_DISK_TTL_MS > now;
    rememberInMemory(stored.entries, diskFresh);
    if (!diskFresh && !options.offline) refreshCatalogInBackground();
    return stored.entries;
  }

  if (options.offline) return [];
  return refreshModelsDevCatalog(options.signal);
}
