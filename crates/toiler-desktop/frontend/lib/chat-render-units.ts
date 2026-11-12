import { findFinalAssistantIndex } from "./message-display";
import { isGroupAnchorMessage } from "./session-paging";
import type { AgentMessage } from "./types";

/**
 * One screen unit of the chat transcript.
 *
 * A completed turn collapses into a single unit (prompt → collapsible process
 * group → final answer) so paging never renders half a turn. Anything outside
 * a collapsed turn is its own unit.
 *
 * Building these is cheap index arithmetic; the expensive part (JSX, markdown,
 * highlighting) only runs for the units inside the visible window.
 */
export type ChatRenderUnit =
  | { kind: "message"; startIdx: number; idx: number }
  | { kind: "turn"; startIdx: number; userIdx: number; endIdx: number; finalAssistantIdx: number };

export interface ChatRenderUnitOptions {
  /** A run is in flight or streaming, so the last turn is rendered expanded. */
  liveTail?: boolean;
}

function indexOfLastAnchor(messages: AgentMessage[]): number {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    if (isGroupAnchorMessage(messages[index])) return index;
  }
  return -1;
}

function pushRange(units: ChatRenderUnit[], messages: AgentMessage[], from: number, to: number): void {
  for (let idx = from; idx < to; idx += 1) units.push({ kind: "message", startIdx: idx, idx });
}

export function buildChatRenderUnits(
  messages: AgentMessage[],
  options: ChatRenderUnitOptions = {},
): ChatRenderUnit[] {
  const units: ChatRenderUnit[] = [];
  const lastAnchorIdx = indexOfLastAnchor(messages);

  for (let idx = 0; idx < messages.length;) {
    if (!isGroupAnchorMessage(messages[idx])) {
      units.push({ kind: "message", startIdx: idx, idx });
      idx += 1;
      continue;
    }

    const userIdx = idx;
    let endIdx = userIdx + 1;
    while (endIdx < messages.length && !isGroupAnchorMessage(messages[endIdx])) endIdx += 1;

    const finalAssistantIdx = findFinalAssistantIndex(messages, userIdx, endIdx);
    if (finalAssistantIdx === -1) {
      pushRange(units, messages, userIdx, endIdx);
      idx = endIdx;
      continue;
    }

    // The turn that is still running keeps every entry standalone: its process
    // group stays open and its answer is not folded yet.
    const isLiveTail = Boolean(options.liveTail)
      && endIdx === messages.length
      && userIdx === lastAnchorIdx;
    if (isLiveTail) {
      pushRange(units, messages, userIdx, endIdx);
      idx = endIdx;
      continue;
    }

    units.push({ kind: "turn", startIdx: userIdx, userIdx, endIdx, finalAssistantIdx });
    idx = endIdx;
  }

  return units;
}
