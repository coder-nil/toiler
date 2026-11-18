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

/** 视口带（轨道上的高亮胶囊）的最小高度。 */
const MIN_BAND_HEIGHT = 16;
/** 胶囊在区间两端最多外扩多少：半个节点间距在稀疏时（50px）会外扩到 25px，
    单轮对话下那圈余量比横条本身厚好几倍，看起来像胶囊自己没对齐。 */
const MAX_BAND_PAD = 10;
/** 横条外沿与胶囊边之间留的空隙，避免胶囊把横条切在边上。 */
const BAND_EDGE_INSET = 2;

export interface MinimapBandInput {
  /** 区间首个节点的中心线位置（px）。 */
  firstCenter: number;
  /** 区间末个节点的中心线位置（px）。 */
  lastCenter: number;
  /** 轨道总高度（px）。 */
  trackHeight: number;
  /** 相邻节点间距（px）。 */
  gap: number;
  /** 横条厚度（px）。 */
  nodeThickness: number;
}

/**
 * 把「当前视口覆盖的节点区间」换算成胶囊的 top / height。
 *
 * 不能用 `±gap / 2` 直接外扩：首尾节点没有相邻节点，外扩量一旦超过容器内边距
 * 就会被 `max(0, …)` 裁掉，结果是胶囊贴着窗口顶端、横条偏在胶囊上半截（只有
 * 一两轮对话时最明显）。所以端部外扩量要同时受容器边缘与 `MAX_BAND_PAD` 约束，
 * 并且胶囊始终以区间中心线为轴 —— 横条才留在胶囊正中，顶部也不会贴边。
 */
export function resolveMinimapViewportBand({
  firstCenter,
  lastCenter,
  trackHeight,
  gap,
  nodeThickness,
}: MinimapBandInput): { top: number; height: number } | null {
  if (!(trackHeight > 0)) return null;

  const first = Math.max(0, Math.min(trackHeight, firstCenter));
  const last = Math.max(first, Math.min(trackHeight, lastCenter));
  const center = (first + last) / 2;
  // 半径以 center 为轴：越过容器上下缘就会一侧被裁、横条再次偏心。
  const maxHalf = Math.max(0, Math.min(center, trackHeight - center));
  const halfSpan = (last - first) / 2;
  const minPad = Math.max(0, nodeThickness) / 2 + BAND_EDGE_INSET;
  const pad = Math.max(minPad, Math.min(gap / 2, MAX_BAND_PAD, maxHalf));
  const half = Math.min(maxHalf, Math.max(MIN_BAND_HEIGHT / 2, halfSpan + pad));
  const top = Math.max(0, center - half);

  return { top, height: Math.max(0, Math.min(trackHeight - top, half * 2)) };
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
