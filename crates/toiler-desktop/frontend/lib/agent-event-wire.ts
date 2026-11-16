/** Rust 服务投影后的 SSE 事件；完整消息与增量事件分别传输。 */
export interface AgentEventLike {
  type: string;
  [key: string]: unknown;
}

/** 对应 toiler-server 的 assistant_event_json，不引入供应商 SDK 类型。 */
export type ClientAssistantMessageEvent =
  | { type: "start" }
  | { type: "text_start" | "thinking_start"; contentIndex: number }
  | { type: "text_delta" | "thinking_delta"; contentIndex: number; delta: string }
  | { type: "text_end" | "thinking_end"; contentIndex: number; content: string }
  | { type: "toolcall_start"; contentIndex: number; id?: string; toolName?: string }
  | { type: "toolcall_delta"; contentIndex: number; delta: string; id?: string; toolName?: string }
  | {
      type: "toolcall_end";
      contentIndex: number;
      toolCall: { id: string; name: string; arguments: Record<string, unknown> };
    }
  | { type: "done"; reason: string; message: unknown }
  | { type: "error"; reason: string; error: unknown };

/** 系统提示属于模型上下文，不作为聊天消息展示。 */
export function isSystemMessageEvent(event: AgentEventLike): boolean {
  const message = event.message;
  return (event.type === "message_start" || event.type === "message_end")
    && typeof message === "object"
    && message !== null
    && "role" in message
    && message.role === "system";
}
