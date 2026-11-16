/** 草稿尚未创建服务端会话时，也要记住用户显式选择的模型。 */
const STORAGE_KEY = "toiler-preferred-model";

export interface PreferredModel {
  provider: string;
  modelId: string;
}

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

export function getPreferredModel(storage: StorageLike | null = browserStorage()): PreferredModel | null {
  try {
    const value = JSON.parse(storage?.getItem(STORAGE_KEY) ?? "null");
    return value && typeof value.provider === "string" && value.provider.trim()
      && typeof value.modelId === "string" && value.modelId.trim()
      ? { provider: value.provider, modelId: value.modelId } : null;
  } catch {
    return null;
  }
}

export function setPreferredModel(model: PreferredModel, storage: StorageLike | null = browserStorage()): void {
  try {
    storage?.setItem(STORAGE_KEY, JSON.stringify(model));
  } catch {
    // 存储受限不应阻止切换模型。
  }
}

export function resolvePreferredModel<T extends { provider: string; id: string }>(
  models: T[],
  defaultModel: PreferredModel | null | undefined,
  preferredModel: PreferredModel | null = getPreferredModel(),
): T | undefined {
  const find = (model: PreferredModel | null | undefined) => model
    ? models.find((item) => item.provider === model.provider && item.id === model.modelId)
    : undefined;
  // 已删除或禁用的偏好不能绕过服务端给出的可见模型范围。
  return find(preferredModel) ?? find(defaultModel) ?? models[0];
}
