/** 会话内容搜索由 Rust 服务完成，侧栏只接收匹配摘要。 */
export interface SessionSearchMatch {
  sessionId: string;
  snippet: string;
}
