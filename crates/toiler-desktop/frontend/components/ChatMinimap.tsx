"use client";

import { memo, useEffect, useRef, useState, useCallback, useMemo, type RefObject } from "react";
import ReactMarkdown, { type Options as ReactMarkdownOptions } from "react-markdown";
import rehypeKatex from "rehype-katex";
import {
  markdownPreviewRemarkPlugins,
  normalizeDisplayMath,
} from "@/lib/markdown";
import { splitFinalAssistantBlocks } from "@/lib/message-display";
import { CHAT_TOP_BAR_HEIGHT } from "@/lib/panel-layout";
import type { AgentMessage, AssistantMessage, TextContent, UserMessage } from "@/lib/types";
import styles from "./ChatMinimap.module.css";

interface Props {
  messages: AgentMessage[];
  streamingMessage: Partial<AgentMessage> | null;
  scrollContainer: RefObject<HTMLDivElement | null>;
  messageRefs: RefObject<(HTMLDivElement | null)[]>;
  onRevealHistory: () => void;
  onStateChange?: (state: { visible: boolean; previewOpen: boolean }) => void;
}

const MAX_NODE_GAP = 50;
const MINIMAP_PADDING = 12;
/**
 * 右栏顶部给「显示文件面板」开关留出的高度。节点层从它下方开始，
 * 否则第一个方格会压在图标上。
 */
const MINIMAP_TOP_INSET = CHAT_TOP_BAR_HEIGHT;
/** 稀疏时的节点方块规格（含边框实占 11px）。 */
const MAX_NODE_SIZE = 8;
const MAX_NODE_BORDER = 1.5;
/** 间距压到极限时节点退化成 2px 的小点，保证几百轮也不糊成一条带。 */
const MIN_NODE_SIZE = 2;
/** 相邻方块之间至少保留的空白。 */
const NODE_CLEARANCE = 1;
const PREVIEW_SHOW_DELAY = 500;
const PREVIEW_HIDE_DELAY = 250;
const NAVIGATION_ACTIVE_LOCK_MS = 1600;

interface AssistantPreview {
  markdown: string;
  element: HTMLDivElement | null;
}

interface TurnInfo {
  userMessage: UserMessage;
  assistantPreviews: AssistantPreview[];
  scrollTop: number | null;
}

interface NodeInfo {
  topRatio: number;
  targetTurn: TurnInfo;
  index: number;
}

function getUserPreview(message: UserMessage): string {
  if (typeof message.content === "string") return message.content.trim();
  return message.content
    .filter((block): block is TextContent => block.type === "text")
    .map((block) => block.text)
    .join("\n")
    .trim();
}

function getAssistantAnswerMarkdown(message: AgentMessage | Partial<AgentMessage>): string {
  if (message.role !== "assistant") return "";
  const { answerBlocks } = splitFinalAssistantBlocks(message as AssistantMessage);
  return answerBlocks
    .filter((block): block is TextContent => block.type === "text")
    .map((block) => block.text)
    .join("\n\n")
    .trim();
}

function PreviewHeading({
  level,
  children,
  headingIndex,
  onClick,
}: {
  level: 1 | 2 | 3;
  children: React.ReactNode;
  headingIndex: number | null;
  onClick?: (headingIndex: number) => void;
}) {
  return (
    <button
      type="button"
      className={styles.heading}
      data-level={level}
      data-preview-heading-index={headingIndex ?? undefined}
      disabled={headingIndex === null || !onClick}
      onClick={(event) => {
        event.stopPropagation();
        if (headingIndex !== null) onClick?.(headingIndex);
      }}
    >
      {children}
    </button>
  );
}

interface PreviewAstNode {
  type?: string;
  depth?: number;
  data?: {
    hProperties?: Record<string, unknown>;
  };
}

function remarkPreviewOutline() {
  return (tree: { children?: PreviewAstNode[] }) => {
    if (!Array.isArray(tree.children)) return;
    const headings = tree.children.filter((node) => (
      node.type === "heading" && typeof node.depth === "number" && node.depth <= 3
    ));
    if (headings.length > 0) {
      headings.forEach((node, headingIndex) => {
        node.data = {
          ...node.data,
          hProperties: {
            ...node.data?.hProperties,
            "data-preview-heading-index": headingIndex,
          },
        };
      });
      tree.children = headings;
      return;
    }
    const firstParagraph = tree.children.find((node) => node.type === "paragraph");
    tree.children = firstParagraph ? [firstParagraph] : [];
  };
}

const previewRemarkPlugins = [
  ...(markdownPreviewRemarkPlugins ?? []),
  remarkPreviewOutline,
];
const previewRehypePlugins: ReactMarkdownOptions["rehypePlugins"] = [
  [rehypeKatex, { throwOnError: false, strict: false }],
];

function getPreviewHeadingIndex(node: unknown): number | null {
  const properties = (node as { properties?: Record<string, unknown> } | undefined)?.properties;
  const value = properties?.dataPreviewHeadingIndex ?? properties?.["data-preview-heading-index"];
  if (typeof value === "number") return value;
  if (typeof value === "string" && /^\d+$/.test(value)) return Number(value);
  return null;
}

export const AssistantOutline = memo(function AssistantOutline({
  markdown,
  onHeadingClick,
  onAnswerClick,
}: {
  markdown: string;
  onHeadingClick?: (headingIndex: number) => void;
  onAnswerClick?: () => void;
}) {
  const normalizedMarkdown = useMemo(() => normalizeDisplayMath(markdown), [markdown]);
  if (!markdown) return null;
  return (
    <div className={styles.outline}>
      <ReactMarkdown
        remarkPlugins={previewRemarkPlugins}
        rehypePlugins={previewRehypePlugins}
        components={{
          h1: ({ children, node }) => <PreviewHeading level={1} headingIndex={getPreviewHeadingIndex(node)} onClick={onHeadingClick}>{children}</PreviewHeading>,
          h2: ({ children, node }) => <PreviewHeading level={2} headingIndex={getPreviewHeadingIndex(node)} onClick={onHeadingClick}>{children}</PreviewHeading>,
          h3: ({ children, node }) => <PreviewHeading level={3} headingIndex={getPreviewHeadingIndex(node)} onClick={onHeadingClick}>{children}</PreviewHeading>,
          h4: () => null,
          h5: () => null,
          h6: () => null,
          p: ({ children }) => (
            <button
              type="button"
              className={styles.paragraph}
              onClick={onAnswerClick}
            >
              {children}
            </button>
          ),
          blockquote: () => null,
          ul: () => null,
          ol: () => null,
          pre: () => null,
          table: () => null,
          hr: () => null,
          a: ({ children }) => <>{children}</>,
          code: ({ children }) => <>{children}</>,
        }}
      >
        {normalizedMarkdown}
      </ReactMarkdown>
    </div>
  );
});

function createTurnNodes(turns: TurnInfo[]): NodeInfo[] {
  return turns.map((turn, index) => ({
    topRatio: 0,
    targetTurn: turn,
    index,
  }));
}

interface NodeLayout {
  nodes: NodeInfo[];
  gap: number;
  fillsHeight: boolean;
  /** 方块边长（不含边框）。 */
  nodeSize: number;
  nodeBorder: number;
}

/**
 * 间距一旦被压缩到放不下默认方块，就让方块同步缩小（并保持至少 NODE_CLEARANCE 的空隙），
 * 这样在有限的 minimap 高度里能塞下更多轮次，而不是让几十个 8px 方块叠成一片。
 */
function nodeVisualFor(gap: number): { nodeSize: number; nodeBorder: number } {
  const maxExtent = MAX_NODE_SIZE + MAX_NODE_BORDER * 2;
  const fittedExtent = Math.round(Math.max(0, gap - NODE_CLEARANCE) * 2) / 2;
  const extent = Math.max(MIN_NODE_SIZE, Math.min(maxExtent, fittedExtent));
  const nodeBorder = extent >= 9 ? MAX_NODE_BORDER : extent >= 6 ? 1 : 0;
  return { nodeSize: extent - nodeBorder * 2, nodeBorder };
}

export function layoutNodes(allNodes: NodeInfo[], minimapHeight: number, topInset = 0): NodeLayout {
  if (allNodes.length === 0) {
    return { nodes: [], gap: MAX_NODE_GAP, fillsHeight: false, ...nodeVisualFor(MAX_NODE_GAP) };
  }

  const height = Math.max(1, minimapHeight);
  const firstTop = Math.min(topInset + MINIMAP_PADDING, Math.max(0, height - MINIMAP_PADDING));
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

export function ChatMinimap({
  messages,
  streamingMessage,
  scrollContainer,
  messageRefs,
  onRevealHistory,
  onStateChange,
}: Props) {
  const [visible, setVisible] = useState(false);
  const [allNodes, setAllNodes] = useState<NodeInfo[]>([]);
  const [activeIndex, setActiveIndex] = useState<number | null>(null);
  const [minimapHeight, setMinimapHeight] = useState(600);
  const [minimapHovered, setMinimapHovered] = useState(false);
  const [mouseYRatio, setMouseYRatio] = useState<number | null>(null);
  const draggingRef = useRef(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const allNodesRef = useRef<NodeInfo[]>([]);
  const nodeLayoutRef = useRef<NodeLayout>({
    nodes: [],
    gap: MAX_NODE_GAP,
    fillsHeight: false,
    ...nodeVisualFor(MAX_NODE_GAP),
  });
  const previewBoxRef = useRef<HTMLDivElement>(null);
  const previewItemRefs = useRef(new Map<number, HTMLDivElement>());
  const previewShowTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const previewHideTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const activeNodeLockRef = useRef<{ index: number; until: number } | null>(null);
  const pendingNavigationRef = useRef<{
    nodeIndex: number;
    target: "user" | "assistant" | "heading";
    assistantIndex?: number;
    headingIndex?: number;
  } | null>(null);

  const allMessages = useMemo(
    () => (streamingMessage ? [...messages, streamingMessage] : messages) as (AgentMessage | Partial<AgentMessage>)[],
    [messages, streamingMessage],
  );
  const allMessagesRef = useRef(allMessages);
  allMessagesRef.current = allMessages;

  const nodeLayout = useMemo(
    () => layoutNodes(allNodes, minimapHeight, MINIMAP_TOP_INSET),
    [allNodes, minimapHeight],
  );
  const { nodes: positionedNodes, gap: nodeGap, nodeSize, nodeBorder } = nodeLayout;
  nodeLayoutRef.current = nodeLayout;

  const lockActiveNode = useCallback((index: number) => {
    activeNodeLockRef.current = {
      index,
      until: Date.now() + NAVIGATION_ACTIVE_LOCK_MS,
    };
    setActiveIndex(index);
  }, []);

  const syncActiveNode = useCallback((scrollEl: HTMLDivElement, nextNodes: NodeInfo[]) => {
    const activeLock = activeNodeLockRef.current;
    if (activeLock && Date.now() < activeLock.until) {
      setActiveIndex(activeLock.index);
      return;
    }
    activeNodeLockRef.current = null;

    const measuredNodes = nextNodes.filter((node) => node.targetTurn.scrollTop !== null);
    if (measuredNodes.length === 0) {
      setActiveIndex(null);
      return;
    }
    const focusTop = scrollEl.scrollTop + scrollEl.clientHeight * 0.3;
    const nextActiveNode = measuredNodes.reduce((bestNode, node) => (
      Math.abs((node.targetTurn.scrollTop ?? 0) - focusTop)
        < Math.abs((bestNode.targetTurn.scrollTop ?? 0) - focusTop)
        ? node
        : bestNode
    ), measuredNodes[0]);
    setActiveIndex(nextActiveNode.index);
  }, []);

  const updateScroll = useCallback(() => {
    const scrollEl = scrollContainer.current;
    if (!scrollEl) return;
    const scrollable = scrollEl.scrollHeight - scrollEl.clientHeight;
    const currentNodes = allNodesRef.current;
    setVisible(scrollable > 20);
    syncActiveNode(scrollEl, currentNodes);
  }, [scrollContainer, syncActiveNode]);

  const measureThrottleRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const measureNodes = useCallback(() => {
    if (measureThrottleRef.current) return;
    measureThrottleRef.current = setTimeout(() => {
      measureThrottleRef.current = null;
      const scrollEl = scrollContainer.current;
      const minimapEl = containerRef.current;
      if (!scrollEl || !minimapEl) return;

      const refs = messageRefs.current;
      const containerRect = scrollEl.getBoundingClientRect();
      const turns: TurnInfo[] = [];
      let refIndex = 0;
      let currentTurn: TurnInfo | null = null;

      for (const message of allMessagesRef.current) {
        if (message.role !== "user" && message.role !== "assistant") continue;
        const element = refs?.[refIndex];
        refIndex++;

        if (message.role === "user") {
          currentTurn = null;
          const elementRect = element?.getBoundingClientRect();
          currentTurn = {
            userMessage: message as UserMessage,
            assistantPreviews: [],
            scrollTop: elementRect
              ? elementRect.top - containerRect.top + scrollEl.scrollTop
              : null,
          };
          turns.push(currentTurn);
          continue;
        }

        if (!currentTurn) continue;
        const answerMarkdown = getAssistantAnswerMarkdown(message);
        if (answerMarkdown) {
          currentTurn.assistantPreviews.push({
            markdown: answerMarkdown,
            element,
          });
        }
      }

      const nextNodes = createTurnNodes(turns);
      // 节点层与预览浮层都铺满整条右栏（含顶栏与输入区那两段），而不是只占
      // 与消息区对齐的一段：容器高度就是量高度。
      setMinimapHeight(minimapEl.clientHeight);
      allNodesRef.current = nextNodes;
      setAllNodes(nextNodes);
      setVisible(scrollEl.scrollHeight - scrollEl.clientHeight > 20);
      syncActiveNode(scrollEl, nextNodes);

      const pendingNavigation = pendingNavigationRef.current;
      const pendingNode = pendingNavigation
        ? nextNodes[pendingNavigation.nodeIndex]
        : null;
      if (pendingNavigation && pendingNode) {
        const assistant = pendingNavigation.assistantIndex === undefined
          ? null
          : pendingNode.targetTurn.assistantPreviews[pendingNavigation.assistantIndex];
        let targetTop: number | null = pendingNode.targetTurn.scrollTop;
        if (pendingNavigation.target === "assistant") {
          const assistantRect = assistant?.element?.getBoundingClientRect();
          targetTop = assistantRect
            ? assistantRect.top - containerRect.top + scrollEl.scrollTop
            : null;
        } else if (pendingNavigation.target === "heading") {
          const heading = (
            pendingNavigation.headingIndex === undefined
              ? null
              : assistant?.element
                ?.querySelectorAll<HTMLElement>("h1, h2, h3")
                .item(pendingNavigation.headingIndex)
          );
          const headingRect = heading?.getBoundingClientRect();
          targetTop = headingRect
            ? headingRect.top - containerRect.top + scrollEl.scrollTop
            : null;
        }
        if (targetTop === null) return;
        pendingNavigationRef.current = null;
        lockActiveNode(pendingNode.index);
        const targetOffset = scrollEl.clientHeight * 0.3;
        scrollEl.scrollTo({ top: Math.max(0, targetTop - targetOffset), behavior: "smooth" });
      }
    }, 150);
  }, [lockActiveNode, messageRefs, scrollContainer, syncActiveNode]);

  useEffect(() => {
    const el = scrollContainer.current;
    if (!el) return;
    el.addEventListener("scroll", updateScroll, { passive: true });
    return () => el.removeEventListener("scroll", updateScroll);
  }, [scrollContainer, updateScroll]);

  useEffect(() => {
    const el = scrollContainer.current;
    if (!el) return;
    const syncLayout = () => {
      measureNodes();
      updateScroll();
    };
    const ro = new ResizeObserver(syncLayout);
    ro.observe(el);
    if (el.firstElementChild) ro.observe(el.firstElementChild);
    // 节点铺满整条右栏，所以右栏自身的高度变化也要重新布局。
    const minimapEl = containerRef.current;
    if (minimapEl) ro.observe(minimapEl);
    syncLayout();
    return () => {
      ro.disconnect();
      if (measureThrottleRef.current) {
        clearTimeout(measureThrottleRef.current);
        measureThrottleRef.current = null;
      }
    };
  }, [measureNodes, scrollContainer, updateScroll, visible]);

  useEffect(() => {
    const timeout = setTimeout(() => {
      measureNodes();
      updateScroll();
    }, 50);
    return () => clearTimeout(timeout);
  }, [messages.length, measureNodes, updateScroll]);

  const scrollToNode = useCallback((node: NodeInfo, behavior: ScrollBehavior) => {
    const scrollEl = scrollContainer.current;
    if (!scrollEl) return;
    lockActiveNode(node.index);
    if (node.targetTurn.scrollTop === null) {
      pendingNavigationRef.current = { nodeIndex: node.index, target: "user" };
      onRevealHistory();
      return;
    }
    const targetTop = Math.max(
      0,
      node.targetTurn.scrollTop - scrollEl.clientHeight * 0.3,
    );
    scrollEl.scrollTo({ top: targetTop, behavior });
  }, [lockActiveNode, onRevealHistory, scrollContainer]);

  const scrollToAssistant = useCallback((node: NodeInfo, assistantIndex: number) => {
    const scrollEl = scrollContainer.current;
    if (!scrollEl) return;
    const assistantElement = node.targetTurn.assistantPreviews[assistantIndex]?.element;
    if (!assistantElement) {
      pendingNavigationRef.current = {
        nodeIndex: node.index,
        target: "assistant",
        assistantIndex,
      };
      onRevealHistory();
      return;
    }
    const containerRect = scrollEl.getBoundingClientRect();
    const assistantRect = assistantElement.getBoundingClientRect();
    const targetTop = (
      assistantRect.top
      - containerRect.top
      + scrollEl.scrollTop
      - scrollEl.clientHeight * 0.3
    );
    lockActiveNode(node.index);
    scrollEl.scrollTo({ top: Math.max(0, targetTop), behavior: "smooth" });
  }, [lockActiveNode, onRevealHistory, scrollContainer]);

  const findNearestNode = useCallback((ratio: number): NodeInfo | null => {
    const { nodes, gap, fillsHeight } = nodeLayoutRef.current;
    const height = containerRef.current?.clientHeight ?? 0;
    if (nodes.length === 0 || height <= 0) return null;

    const pointerY = Math.max(0, Math.min(height, ratio * height));
    const firstNodeY = nodes[0].topRatio * height;
    const rawIndex = gap > 0 ? Math.round((pointerY - firstNodeY) / gap) : 0;
    const nodeIndex = Math.max(0, Math.min(nodes.length - 1, rawIndex));
    const nearestNode = nodes[nodeIndex];

    if (!fillsHeight) {
      const nodeY = nearestNode.topRatio * height;
      const hitRadius = Math.max(10, gap / 2);
      if (Math.abs(pointerY - nodeY) > hitRadius) return null;
    }
    return nearestNode;
  }, []);

  const scrollToHeading = useCallback((
    node: NodeInfo,
    assistantIndex: number,
    headingIndex: number,
  ) => {
    const scrollEl = scrollContainer.current;
    if (!scrollEl) return;
    const answerElement = node.targetTurn.assistantPreviews[assistantIndex]?.element;
    if (!answerElement) {
      pendingNavigationRef.current = {
        nodeIndex: node.index,
        target: "heading",
        assistantIndex,
        headingIndex,
      };
      onRevealHistory();
      return;
    }
    const heading = answerElement.querySelectorAll<HTMLElement>("h1, h2, h3").item(headingIndex);
    if (!heading) return;
    const containerRect = scrollEl.getBoundingClientRect();
    const headingRect = heading.getBoundingClientRect();
    const targetTop = (
      headingRect.top
      - containerRect.top
      + scrollEl.scrollTop
      - scrollEl.clientHeight * 0.3
    );
    lockActiveNode(node.index);
    scrollEl.scrollTo({ top: Math.max(0, targetTop), behavior: "smooth" });
  }, [lockActiveNode, onRevealHistory, scrollContainer]);

  const cancelPreviewShow = useCallback(() => {
    if (!previewShowTimerRef.current) return;
    clearTimeout(previewShowTimerRef.current);
    previewShowTimerRef.current = null;
  }, []);

  const cancelPreviewHide = useCallback(() => {
    if (!previewHideTimerRef.current) return;
    clearTimeout(previewHideTimerRef.current);
    previewHideTimerRef.current = null;
  }, []);

  const showPreview = useCallback(() => {
    cancelPreviewShow();
    cancelPreviewHide();
    setMinimapHovered(true);
  }, [cancelPreviewHide, cancelPreviewShow]);

  const schedulePreviewShow = useCallback(() => {
    cancelPreviewHide();
    if (previewShowTimerRef.current) return;
    previewShowTimerRef.current = setTimeout(() => {
      previewShowTimerRef.current = null;
      setMinimapHovered(true);
    }, PREVIEW_SHOW_DELAY);
  }, [cancelPreviewHide]);

  const schedulePreviewHide = useCallback(() => {
    cancelPreviewShow();
    cancelPreviewHide();
    previewHideTimerRef.current = setTimeout(() => {
      previewHideTimerRef.current = null;
      setMinimapHovered(false);
      setMouseYRatio(null);
    }, PREVIEW_HIDE_DELAY);
  }, [cancelPreviewHide, cancelPreviewShow]);

  useEffect(() => () => {
    cancelPreviewShow();
    cancelPreviewHide();
  }, [cancelPreviewHide, cancelPreviewShow]);

  const handleMouseDown = useCallback((event: React.MouseEvent<HTMLDivElement>) => {
    if (!visible) return;

    draggingRef.current = true;
    showPreview();
    const rect = event.currentTarget.getBoundingClientRect();
    const pointerRatio = Math.max(0, Math.min(1, (event.clientY - rect.top) / rect.height));
    setMouseYRatio(pointerRatio);
    const jumpToPointer = (clientY: number, behavior: ScrollBehavior) => {
      const ratio = Math.max(0, Math.min(1, (clientY - rect.top) / rect.height));
      const node = findNearestNode(ratio);
      if (node) {
        scrollToNode(node, behavior);
      }
    };

    jumpToPointer(event.clientY, "smooth");
    const onMove = (moveEvent: MouseEvent) => {
      if (!draggingRef.current) return;
      jumpToPointer(moveEvent.clientY, "auto");
    };
    const onUp = () => {
      draggingRef.current = false;
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }, [findNearestNode, scrollToNode, showPreview, visible]);

  const nearestNode = mouseYRatio === null ? null : findNearestNode(mouseYRatio);
  const nearestNodeIndex = nearestNode?.index ?? null;
  const previewOpen = visible && minimapHovered && allNodes.length > 0;

  useEffect(() => {
    onStateChange?.({ visible, previewOpen });
  }, [onStateChange, previewOpen, visible]);

  useEffect(() => () => {
    onStateChange?.({ visible: false, previewOpen: false });
  }, [onStateChange]);

  useEffect(() => {
    if (!minimapHovered || nearestNodeIndex === null) return;
    const previewBox = previewBoxRef.current;
    const previewItem = previewItemRefs.current.get(nearestNodeIndex);
    if (!previewBox || !previewItem) return;
    const targetTop = previewItem.offsetTop
      - (previewBox.clientHeight - previewItem.offsetHeight) / 2;
    previewBox.scrollTop = Math.max(0, targetTop);
  }, [allNodes, minimapHovered, nearestNodeIndex]);

  if (!visible) return null;

  // 背景、左边框和贯穿整列的竖线由 ChatWindow 的整条右栏（data-chat-minimap-rail）提供；
  // 这里只负责节点层，避免整条被切成两段。
  return (
    <div
      ref={containerRef}
      data-chat-minimap=""
      onMouseDown={handleMouseDown}
      onMouseEnter={schedulePreviewShow}
      onMouseLeave={schedulePreviewHide}
      onMouseMove={(event) => {
        const rect = event.currentTarget.getBoundingClientRect();
        setMouseYRatio((event.clientY - rect.top) / rect.height);
      }}
      style={{
        position: "absolute",
        // 撑满整条右栏：left/right 跟随列宽动画，top/bottom 贯穿到底。
        left: 0,
        right: 0,
        top: 0,
        bottom: 0,
        cursor: "pointer",
        userSelect: "none",
        background: "transparent",
        overflow: "visible",
      }}
    >
      {positionedNodes.map((node) => {
        const isNearest = minimapHovered && nearestNode?.index === node.index;
        const isActive = activeIndex === node.index;
        const fill = isActive ? "rgba(128,128,128,0.42)" : "rgba(128,128,128,0.16)";
        const stroke = isActive ? "rgba(128,128,128,0.95)" : "rgba(128,128,128,0.58)";

        return (
          <div
            key={node.index}
            data-minimap-node-index={node.index}
            data-minimap-node-active={isActive ? "" : undefined}
            style={{
              position: "absolute",
              top: `${node.topRatio * 100}%`,
              transform: "translateY(-50%)",
              left: 0,
              right: 0,
              height: Math.max(1, nodeGap),
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              pointerEvents: "auto",
              zIndex: 2,
            }}
          >
            <div
              style={{
                width: nodeSize,
                height: nodeSize,
                borderRadius: Math.min(2, nodeSize / 2),
                // 间距太密时描边会吃掉整个方块，于是直接用描边色填充
                background: nodeBorder > 0 ? fill : stroke,
                border: nodeBorder > 0 ? `${nodeBorder}px solid ${stroke}` : "none",
                // 激活光环只有在不会碰到相邻方块时才画
                boxShadow: isActive && nodeSize + nodeBorder * 2 + 4 <= nodeGap
                  ? "0 0 0 2px var(--bg-panel)"
                  : "none",
                transition: "transform 0.1s, background 0.1s",
                transform: isNearest ? "scale(1.25)" : "scale(1)",
              }}
            />
          </div>
        );
      })}

      {previewOpen && (
        <div
          ref={previewBoxRef}
          className={styles.preview}
          data-minimap-preview-box=""
          onMouseEnter={showPreview}
          onMouseDown={(event) => event.stopPropagation()}
          onMouseMove={(event) => event.stopPropagation()}
        >
          {allNodes.map((node) => {
            const isLocated = nearestNodeIndex === node.index;
            return (
              <div
                key={node.index}
                ref={(element) => {
                  if (element) previewItemRefs.current.set(node.index, element);
                  else previewItemRefs.current.delete(node.index);
                }}
                className={styles.turn}
                data-minimap-preview-index={node.index}
                data-located={isLocated ? "true" : undefined}
              >
                <span className={styles.number} aria-hidden="true">
                  {String(node.index + 1).padStart(2, "0")}
                </span>
                <div className={styles.content}>
                  <button
                    type="button"
                    className={styles.user}
                    data-minimap-preview-user={node.index}
                    onClick={() => {
                      scrollToNode(node, "smooth");
                    }}
                  >
                    <span className={styles.userText}>
                      {getUserPreview(node.targetTurn.userMessage)}
                    </span>
                  </button>

                  {node.targetTurn.assistantPreviews.map((assistant, assistantIndex) => (
                    <div
                      key={assistantIndex}
                      className={styles.assistant}
                    >
                      <button
                        type="button"
                        className={styles.assistantJump}
                        data-minimap-preview-assistant={`${node.index}-${assistantIndex}`}
                        onClick={() => scrollToAssistant(node, assistantIndex)}
                        aria-label="Locate assistant message"
                        title="Locate assistant message"
                      >
                        A
                      </button>
                      <AssistantOutline
                        markdown={assistant.markdown}
                        onAnswerClick={() => scrollToAssistant(node, assistantIndex)}
                        onHeadingClick={(headingIndex) => (
                          scrollToHeading(node, assistantIndex, headingIndex)
                        )}
                      />
                    </div>
                  ))}
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

// Hook to create a stable array of refs for messages
export function useMessageRefs(count: number): RefObject<(HTMLDivElement | null)[]> {
  const refs = useRef<(HTMLDivElement | null)[]>([]);
  refs.current = Array(count).fill(null).map((_, i) => refs.current[i] ?? null);
  return refs;
}
