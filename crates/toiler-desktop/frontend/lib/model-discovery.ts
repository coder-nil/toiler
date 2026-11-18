/** 模型发现接口返回的条目，远程请求由 Rust 服务执行。 */
export interface DiscoveredModel {
  id: string;
  name?: string;
  contextWindow?: number;
  maxTokens?: number;
  input?: ("text" | "image")[];
}

/** 只把供应商 id 发给服务端，已有 API Key 不需要读回浏览器。 */
export async function refreshProviderModels(
  providerName: string,
  fetcher: typeof fetch = fetch,
  signal?: AbortSignal,
): Promise<DiscoveredModel[]> {
  const response = await fetcher("/api/models-config/refresh", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ providerName }),
    signal,
  });
  const data = await response.json() as { models?: DiscoveredModel[]; error?: string };
  if (!response.ok || data.error || !Array.isArray(data.models)) {
    throw new Error(data.error ?? `HTTP ${response.status}`);
  }
  return data.models;
}
