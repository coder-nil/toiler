import type { MobileUiRequest } from "../components/MobilePromptCard";

export type MobileEventAction = "start" | "snapshot" | "prompt" | "delta" | "settle" | "sync" | "ignore";

export interface MobileStreamPayload {
  type?: string;
  assistantMessageEvent?: unknown;
  message?: unknown;
  isStreaming?: unknown;
  id?: unknown;
}

const BLOCKING_UI_METHODS = new Set(["select", "confirm", "input", "editor", "custom"]);

export function blockingUiRequestFromEvent(payload: MobileStreamPayload): MobileUiRequest | null {
  const request = payload as unknown as MobileUiRequest;
  if (typeof request.id !== "string" || !request.id) return null;
  if (typeof request.method !== "string" || !BLOCKING_UI_METHODS.has(request.method)) return null;
  return typeof request.title === "string" ? request : null;
}

/** Rust 发 agent_end，旧 pi 发 prompt_done/agent_settled，三者都要刷新快照并收尾。 */
export function classifyAgentEvent(payload: MobileStreamPayload): MobileEventAction {
  switch (payload.type) {
    case "agent_start": return "start";
    case "connected": return payload.isStreaming === true ? "start" : "sync";
    case "message_start": {
      const message = payload.message;
      return message && typeof message === "object" && "role" in message && message.role === "assistant"
        ? "snapshot" : "ignore";
    }
    case "queue_update":
    case "extension_ui_resolved": return "sync";
    case "extension_ui_request": return blockingUiRequestFromEvent(payload) ? "prompt" : "ignore";
    case "message_update": return payload.assistantMessageEvent ? "delta" : "ignore";
    case "prompt_done":
    case "agent_settled":
    case "agent_end": return "settle";
    default: return "ignore";
  }
}
