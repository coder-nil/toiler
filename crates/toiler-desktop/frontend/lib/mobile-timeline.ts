import type { MobileMessage } from "./mobile-state";

/**
 * 手机端视图的展示常量：整套 /m 只用这一套圆角，改了这里到处都改。
 *   6 小标签 / 12 输入容器 / 14 消息气泡 / 999 头部与主操作按钮
 */
export const MOBILE_RADIUS = { chip: 6, field: 12, bubble: 14, round: 999 } as const;

/**
 * 手机端时间线投影：把扁平的消息列表切成「用户消息 → 处理详情 → 最终回答」，
 * 以及展开/收起状态要用的两件小事（待确认气泡的接管、展示常量）。
 *
 * 分组规则与桌面 `ChatWindow` 的 `ProcessDetailsGroup` 对齐：
 *   - 一轮从用户消息开始，到下一个用户消息结束；
 *   - 一轮里最后一条带正文的助手条目当「最终回答」，正文留在折叠组外；
 *   - 其余助手条目（包括它们的正文和工具调用）折进「处理详情」；
 *   - 最终回答那一条自带的工具调用也属于处理详情（桌面同样把它算进折叠组）。
 *
 * 纯函数：不碰 DOM、不碰 React，便于单测覆盖「没有回答」「中途开始」等边界。
 */

export interface MobileTurnItem {
  kind: "turn";
  /** 折叠组与回答共用的 React key。 */
  key: string;
  /** 折进「处理详情」的过程消息。 */
  process: MobileMessage[];
  /** 被折叠的过程消息条数（不含最终回答）。 */
  messageCount: number;
  /** 本轮全部工具调用次数（含最终回答那一条）。 */
  toolCallCount: number;
  /** 最终回答：正文渲染在折叠组外面。 */
  answer: MobileMessage | null;
}

export interface MobileUserItem {
  kind: "user";
  key: string;
  message: MobileMessage;
}

export type MobileTimelineItem = MobileUserItem | MobileTurnItem;

export function buildMobileTimeline(messages: readonly MobileMessage[]): MobileTimelineItem[] {
  const items: MobileTimelineItem[] = [];
  let turn: MobileMessage[] = [];

  const flushTurn = () => {
    if (turn.length === 0) return;
    const assistants = turn;
    turn = [];

    let answerIndex = -1;
    for (let index = assistants.length - 1; index >= 0; index--) {
      if (assistants[index].text) {
        answerIndex = index;
        break;
      }
    }

    const answer = answerIndex >= 0 ? assistants[answerIndex] : null;
    const process = assistants.filter((_, index) => index !== answerIndex);
    const toolCallCount = assistants.reduce((total, message) => total + (message.tools?.length ?? 0), 0);

    // 既没有过程内容也没有回答（理论上投影层已经过滤掉）时不产生空轮。
    if (!answer && process.length === 0 && toolCallCount === 0) return;

    items.push({
      kind: "turn",
      key: answer?.id ?? process[0]?.id ?? `turn-${items.length}`,
      process,
      messageCount: process.length,
      toolCallCount,
      answer,
    });
  };

  for (const message of messages) {
    if (message.role === "user") {
      flushTurn();
      items.push({ kind: "user", key: message.id, message });
      continue;
    }
    turn.push(message);
  }
  flushTurn();

  return items;
}

/**
 * 手机端「刚发出去的消息」的本地兜底。
 *
 * 发出去的指令要等这轮跑完（prompt_done）才会重新拉快照，中间那段时间快照里还没有
 * 这条用户消息，屏幕上什么都看不到。所以发送成功后先在本地插一条气泡，等快照里出现
 * 「新的、同文的」用户消息时再把它交还给服务端版本。
 *
 * `knownIds` 是上一次快照里已经见过的条目 id：只有新出现的条目才能接管，否则
 * 「继续」这种重复文案会被旧条目误判成已经落库，气泡反而会提前消失。
 */
export function reconcilePendingUserMessages(
  pending: readonly MobileMessage[],
  incoming: readonly MobileMessage[],
  knownIds: ReadonlySet<string>,
): MobileMessage[] {
  if (pending.length === 0) return pending as MobileMessage[];

  // 新条目按文本计数：一条新用户消息只能接管一条同文的本地气泡。
  const fresh = new Map<string, number>();
  for (const message of incoming) {
    if (message.role !== "user" || knownIds.has(message.id)) continue;
    const text = message.text.trim();
    fresh.set(text, (fresh.get(text) ?? 0) + 1);
  }
  if (fresh.size === 0) return pending as MobileMessage[];

  let changed = false;
  const next: MobileMessage[] = [];
  for (const message of pending) {
    const text = message.text.trim();
    const remaining = fresh.get(text) ?? 0;
    if (remaining > 0) {
      fresh.set(text, remaining - 1);
      changed = true;
      continue;
    }
    next.push(message);
  }

  // 没有任何气泡被接管时返回原引用，React 不会因为一次刷新而白白重渲染。
  return changed ? next : (pending as MobileMessage[]);
}
