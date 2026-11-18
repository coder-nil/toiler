/**
 * minimap 轨道的节点布局几何。
 *
 * 抽成纯模块是为了能被测试直接拿到真实常量：第一轮节点的落点必须给视口胶囊
 * 留够余量（胶囊以节点为轴，上下各占半高），否则就会出现「胶囊贴住轨道顶端、
 * 蓝色横条偏在胶囊上半截」。组件只负责把这些数字写进 style。
 */

export const MAX_NODE_GAP = 50;

/**
 * 轨道首尾的内间距。
 *
 * 比胶囊半高（`resolveMinimapViewportBand` 里最多 10px）明显更大：单轮对话时
 * 节点就落在 22px 处，胶囊占 12~32px，与窗口顶端之间还留着 12px，看起来才是
 * 「浮在轨道里」而不是「被切在边上」。原来 12px 只够剩 2px，视觉上就是贴顶。
 */
export const MINIMAP_PADDING = 22;

/**
 * 节点是横条而不是方块：轨道只有 28px 宽，同样面积下横向更长更好点中，
 * 密集时又能压成一根根刻度线，不会叠成一串圆点。
 */
export const MAX_NODE_WIDTH = 14;
export const MIN_NODE_WIDTH = 4;
/** 厚度上限 4px，几百轮时才不会糊成一条带。 */
export const MAX_NODE_HEIGHT = 4;
export const MIN_NODE_HEIGHT = 2;
/** 相邻横条之间至少保留的空白。 */
export const NODE_CLEARANCE = 1;

export interface NodeLayout<T> {
  nodes: T[];
  gap: number;
  fillsHeight: boolean;
  /** 横条长度。 */
  nodeWidth: number;
  /** 横条厚度。 */
  nodeHeight: number;
}

/**
 * 横条的长度随间距生长（稀疏时是一条能看清的短线），厚度一旦放不下就压扁
 * （并保持至少 NODE_CLEARANCE 的空隙），这样有限的 minimap 高度里能塞下更多轮次，
 * 而不是让几十根横条叠成一片。
 */
export function nodeVisualFor(gap: number): { nodeWidth: number; nodeHeight: number } {
  const clearance = Math.max(0, gap - NODE_CLEARANCE);
  const nodeHeight = Math.round(
    Math.max(MIN_NODE_HEIGHT, Math.min(MAX_NODE_HEIGHT, clearance)) * 2,
  ) / 2;
  // 长度也跟着间距收缩：密集时还维持 16px 宽的话，就会连成一条实心竖带。
  const nodeWidth = Math.round(
    Math.min(MAX_NODE_WIDTH, Math.max(MIN_NODE_WIDTH, clearance * 2.2)),
  );
  return { nodeWidth, nodeHeight };
}

export function layoutNodes<T extends { topRatio: number }>(
  allNodes: readonly T[],
  minimapHeight: number,
): NodeLayout<T> {
  if (allNodes.length === 0) {
    return { nodes: [], gap: MAX_NODE_GAP, fillsHeight: false, ...nodeVisualFor(MAX_NODE_GAP) };
  }

  const height = Math.max(1, minimapHeight);
  const firstTop = Math.min(MINIMAP_PADDING, Math.max(0, height - MINIMAP_PADDING));
  const usableHeight = Math.max(0, height - firstTop - MINIMAP_PADDING);
  if (allNodes.length === 1) {
    return {
      nodes: [{ ...allNodes[0], topRatio: firstTop / height }],
      gap: MAX_NODE_GAP,
      fillsHeight: false,
      ...nodeVisualFor(MAX_NODE_GAP),
    };
  }

  const naturalGap = usableHeight / (allNodes.length - 1);
  const gap = Math.min(MAX_NODE_GAP, naturalGap);
  return {
    nodes: allNodes.map((node, index) => ({
      ...node,
      topRatio: (firstTop + index * gap) / height,
    })),
    gap,
    fillsHeight: naturalGap <= MAX_NODE_GAP,
    ...nodeVisualFor(gap),
  };
}