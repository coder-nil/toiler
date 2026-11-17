export interface MinimapTurnPosition {
  /** 用户消息的真实位置；未渲染时仍须展开历史后才能跳转。 */
  scrollTop: number | null;
  /** 视口定位可以借用本轮仍在渲染的助手消息，不强制展开整轮。 */
  viewportTop: number | null;
}

/** messageTops 与消息引用数组一致，只包含 user / assistant 的槽位。 */
export function measureMinimapTurnPositions(
  messages: readonly { role?: string }[],
  messageTops: readonly (number | null)[],
): MinimapTurnPosition[] {
  const turns: MinimapTurnPosition[] = [];
  let currentTurn: MinimapTurnPosition | null = null;
  let refIndex = 0;
  for (const message of messages) {
    if (message.role !== "user" && message.role !== "assistant") continue;
    const top = messageTops[refIndex++] ?? null;
    if (message.role === "user") {
      currentTurn = { scrollTop: top, viewportTop: top };
      turns.push(currentTurn);
    } else if (currentTurn && currentTurn.viewportTop === null && top !== null) {
      // 运行中的单轮也会分页，工具调用多时用户消息已卸载，但尾部仍属于这一轮。
      currentTurn.viewportTop = top;
    }
  }
  return turns;
}

export function resolveMinimapViewport(
  positions: readonly MinimapTurnPosition[],
  scrollTop: number,
  clientHeight: number,
): { first: number; last: number; active: number } | null {
  const measured = positions.flatMap((position, index) => (
    position.viewportTop === null ? [] : [{ index, top: position.viewportTop }]
  ));
  if (measured.length === 0) return null;

  // 未渲染的历史节点没有像素坐标，不能把下标 0 当作默认视口位置。
  let first = measured[0].index;
  let last = first;
  let active = measured[0];
  const focusTop = scrollTop + clientHeight * 0.3;
  for (const node of measured) {
    if (node.top <= scrollTop + 4) first = node.index;
    if (node.top < scrollTop + clientHeight) last = node.index;
    if (Math.abs(node.top - focusTop) < Math.abs(active.top - focusTop)) active = node;
  }
  return { first, last: Math.max(first, last), active: active.index };
}
