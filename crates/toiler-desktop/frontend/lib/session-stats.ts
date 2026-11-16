export interface ContextUsage {
  percent: number | null;
  contextWindow: number;
  tokens: number | null;
}

/** Rust 服务的会话统计响应，不依赖旧 Node SDK 的会话对象。 */
export interface SessionStatsInfo {
  sessionFile?: string;
  sessionId: string;
  sessionName?: string;
  userMessages: number;
  assistantMessages: number;
  toolCalls: number;
  toolResults: number;
  totalMessages: number;
  tokens: {
    input: number;
    output: number;
    cacheRead: number;
    cacheWrite: number;
    total: number;
  };
  cost: number;
  contextUsage?: ContextUsage;
  totalActiveMs?: number;
}
