/**
 * 文件预览内的查找：把纯逻辑集中在这里，组件只负责状态与滚动。
 * 单位统一为「行内字符偏移」，因为语法高亮后的文本节点会被 token 切开，
 * 需要按偏移重新拼回 <mark>。
 */

export interface FileSearchMatch {
  /** 1-based line number. */
  line: number;
  /** 0-based start offset within the line. */
  start: number;
  /** Exclusive end offset within the line. */
  end: number;
}

export interface LineSearchRange {
  start: number;
  end: number;
  /** Index into the full match list, used to flag the active match. */
  index: number;
}

export function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** 大小写与正则都固定成字面量匹配；空查询直接返回空数组。 */
export function findFileSearchMatches(
  content: string,
  query: string,
  caseSensitive: boolean,
): FileSearchMatch[] {
  if (!query) return [];

  const pattern = new RegExp(escapeRegExp(query), caseSensitive ? "g" : "gi");
  const matches: FileSearchMatch[] = [];

  content.split("\n").forEach((lineText, lineIndex) => {
    pattern.lastIndex = 0;
    let hit: RegExpExecArray | null;
    while ((hit = pattern.exec(lineText)) !== null) {
      matches.push({ line: lineIndex + 1, start: hit.index, end: hit.index + hit[0].length });
      // 零宽匹配（空查询已被拦截）兜底，避免死循环。
      if (hit.index === pattern.lastIndex) pattern.lastIndex += 1;
    }
  });

  return matches;
}

export function buildLineSearchRangeMap(
  matches: FileSearchMatch[],
): Map<number, LineSearchRange[]> {
  const ranges = new Map<number, LineSearchRange[]>();
  matches.forEach((match, index) => {
    const entry: LineSearchRange = { start: match.start, end: match.end, index };
    const existing = ranges.get(match.line);
    if (existing) existing.push(entry);
    else ranges.set(match.line, [entry]);
  });
  return ranges;
}

/** refractor/prism 交给 react-syntax-highlighter 的 AST 节点形状。 */
export interface SyntaxNode {
  type?: string;
  value?: string;
  children?: SyntaxNode[];
  [key: string]: unknown;
}

interface HighlightState {
  ranges: LineSearchRange[];
  rangeIndex: number;
  cursor: number;
  activeIndex: number;
}

function markNode(value: string, index: number, activeIndex: number): SyntaxNode {
  return {
    type: "element",
    tagName: "mark",
    properties: {
      className: index === activeIndex
        ? ["file-search-match", "is-active"]
        : ["file-search-match"],
    },
    children: [{ type: "text", value }],
  };
}

function splitTextNode(value: string, state: HighlightState): SyntaxNode[] {
  const start = state.cursor;
  const end = start + value.length;
  const nodes: SyntaxNode[] = [];
  let plainFrom = start;

  while (state.rangeIndex < state.ranges.length) {
    const range = state.ranges[state.rangeIndex];
    if (range.end <= plainFrom) {
      state.rangeIndex += 1;
      continue;
    }
    if (range.start >= end) break;

    // 一个匹配可能跨多个 token，所以按节点裁剪后再拼回同一段 <mark>。
    const matchStart = Math.max(range.start, plainFrom);
    const matchEnd = Math.min(range.end, end);
    if (matchStart > plainFrom) {
      nodes.push({ type: "text", value: value.slice(plainFrom - start, matchStart - start) });
    }
    nodes.push(markNode(value.slice(matchStart - start, matchEnd - start), range.index, state.activeIndex));
    plainFrom = matchEnd;

    if (range.end <= end) state.rangeIndex += 1;
    else break;
  }

  if (plainFrom < end) nodes.push({ type: "text", value: value.slice(plainFrom - start, end - start) });
  state.cursor = end;
  return nodes;
}

function transformNode(node: SyntaxNode, state: HighlightState): SyntaxNode[] {
  if (node.type === "text") return splitTextNode(node.value ?? "", state);

  const children = node.children;
  if (!children || children.length === 0) return [node];
  return [{ ...node, children: children.flatMap((child) => transformNode(child, state)) }];
}

/**
 * 把一行高亮 AST 里的匹配文本包成 <mark>。`ranges` 必须是该行按偏移升序的
 * 不重叠区间，`activeIndex` 指向当前正在导航的匹配。
 */
export function highlightSyntaxNodes(
  nodes: SyntaxNode[],
  ranges: LineSearchRange[],
  activeIndex: number,
): SyntaxNode[] {
  const state: HighlightState = { ranges, rangeIndex: 0, cursor: 0, activeIndex };
  return nodes.flatMap((node) => transformNode(node, state));
}
