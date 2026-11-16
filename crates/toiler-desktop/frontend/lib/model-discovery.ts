/** 模型发现接口返回的条目，远程请求由 Rust 服务执行。 */
export interface DiscoveredModel {
  id: string;
  name?: string;
  contextWindow?: number;
  maxTokens?: number;
  input?: ("text" | "image")[];
}
