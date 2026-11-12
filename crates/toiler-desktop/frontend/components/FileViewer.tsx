"use client";

import { RevealFileButton } from "./RevealFileButton";

import { useEffect, useState, useRef, useCallback, useMemo, type ClipboardEvent as ReactClipboardEvent, type CSSProperties, type MouseEvent } from "react";
import {
  Prism as SyntaxHighlighter,
  createElement as renderSyntaxNode,
  type SyntaxHighlighterProps,
} from "react-syntax-highlighter";
import ReactMarkdown from "react-markdown";
import { useTheme } from "@/hooks/useTheme";
import {
  DOCX_PREVIEW_MAX_BYTES,
  getFileExt,
  isAudioPath,
  isDocumentPreviewPath,
  isImagePath,
} from "@/lib/file-types";
import { encodeFilePathForApi, getFileDirectory, getFileName, getRelativeFilePath } from "@/lib/file-paths";
import { resolveLocalFileTarget, type FileOpenLocation } from "@/lib/file-links";
import { parseFrontmatter } from "@/lib/frontmatter";
import { markdownPreviewRehypePlugins, markdownPreviewRemarkPlugins, normalizeDisplayMath } from "@/lib/markdown";
import { isMacPlatform, type ModifierKeyLabel } from "@/lib/platform-key";
import { CodeBlock, MermaidBlock } from "./MermaidBlock";
import { FrontmatterCard } from "./FrontmatterCard";
import { CaseSensitive, ChevronDown, ChevronUp, Redo2, Save, Search, Undo2, X } from "lucide-react";
import { parseUnifiedPatch } from "@/lib/patch";
import type { GitFileDiffResponse } from "@/lib/git-types";
import { MonacoFileEditor, type MonacoEditorHandle } from "./MonacoEditor";
import { useI18n } from "@/hooks/useI18n";
import { darkSyntaxTheme, lightSyntaxTheme } from "@/lib/syntax-highlighter-theme";
import { isOpenableExternalUrl, takeOverExternalLink } from "@/lib/open-external-url";
import {
  buildLineSearchRangeMap,
  findFileSearchMatches,
  highlightSyntaxNodes,
  type LineSearchRange,
  type SyntaxNode,
} from "@/lib/file-viewer-search";
import {
  defaultOpenDisplayMode,
  resolveInitialFileDisplayMode,
  type FileViewerDisplayMode as DisplayMode,
  type FileViewerState,
} from "@/lib/file-viewer-state";

export type { FileViewerState } from "@/lib/file-viewer-state";

interface Props {
  filePath: string;
  cwd?: string;
  sourceSessionId?: string | null;
  onOpenFile?: (filePath: string, location?: FileOpenLocation) => void;
  onMentionLines?: (relativePath: string, startLine: number, endLine: number) => void;
  /** Insert this file's relative path into the chat input (@ mention). */
  onAtMention?: (relativePath: string, isDir: boolean) => void;
  gitRefreshKey?: number;
  initialDisplayMode?: DisplayMode;
  initialState?: FileViewerState;
  /** 1-based line to reveal once the file loads (from a `path:12` style link). */
  initialLine?: number;
  /** Called after the initial line was revealed, so the caller can drop it. */
  onRevealHandled?: () => void;
  onStateChange?: (state: FileViewerState) => void;
  watchEnabled?: boolean;
  /** 允许用户进入编辑模式（手动保存）。 */
  editable?: boolean;
  /** 未保存改动变化：脏/干净，用于标签圆点与关闭前确认。 */
  onDirtyChange?: (tabId: string, dirty: boolean) => void;
  /** 标签页 ID，供 onDirtyChange 上报。 */
  tabId?: string;
  /** 选中的会话 ID（编辑保存回调用）。 */
  sessionId?: string | null;
}

interface FileData {
  content: string;
  language: string;
  size: number;
}

const DISPLAY_MODE_LABELS: Record<DisplayMode, string> = {
  edit: "Edit",
  source: "Source",
  preview: "Preview",
  diff: "Diff",
};

const FILE_CODE_STYLE: CSSProperties = {
  fontFamily: "var(--font-mono)",
  fontSize: 13,
  lineHeight: 1.6,
};

const FILE_LINE_NUMBER_STYLE: CSSProperties = {
  position: "sticky",
  left: 0,
  zIndex: 1,
  width: 48,
  minWidth: 48,
  padding: "0 10px",
  textAlign: "right",
  color: "var(--text-dim)",
  background: "var(--bg-panel)",
  borderRight: "1px solid var(--border)",
  fontFamily: "var(--font-mono)",
  fontSize: 11,
  fontStyle: "normal",
  fontVariantNumeric: "tabular-nums",
  lineHeight: "20.8px",
  userSelect: "none",
  WebkitUserSelect: "none",
  flexShrink: 0,
  verticalAlign: "top",
};

type SourceCodeRendererProps = Parameters<NonNullable<SyntaxHighlighterProps["renderer"]>>[0] & {
  wrapLines: boolean;
  flashLine?: number | null;
  lineSearchRanges?: Map<number, LineSearchRange[]>;
  activeSearchIndex?: number;
};

interface SelectedLineRange {
  startLine: number;
  endLine: number;
}

function MentionIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <circle cx="12" cy="12" r="4" />
      <path d="M16 8v5a3 3 0 0 0 6 0v-1a10 10 0 1 0-4 8" />
    </svg>
  );
}

function closestSourceLine(node: Node): HTMLElement | null {
  const element = node.nodeType === Node.ELEMENT_NODE
    ? node as Element
    : node.parentElement;
  return element?.closest<HTMLElement>(".file-source-line[data-line-number]") ?? null;
}

function getSelectedSourceLineRange(root: HTMLElement, selection: Selection | null): SelectedLineRange | null {
  if (!selection || selection.isCollapsed || selection.rangeCount === 0) return null;

  const range = selection.getRangeAt(0);
  if (!root.contains(range.startContainer) || !root.contains(range.endContainer)) return null;

  let startElement = closestSourceLine(range.startContainer);
  let endElement = closestSourceLine(range.endContainer);
  if (!startElement || !endElement || !root.contains(startElement) || !root.contains(endElement)) return null;

  let startLine = Number(startElement.dataset.lineNumber);
  let endLine = Number(endElement.dataset.lineNumber);
  if (!Number.isInteger(startLine) || !Number.isInteger(endLine)) return null;

  if (startLine < endLine) {
    // Browser ranges can start at the end of the preceding line or end at the
    // start of the following line. Exclude either boundary line when none of
    // its source text is actually selected.
    const startContent = startElement.querySelector<HTMLElement>(".file-source-line-content");
    if (startContent?.contains(range.startContainer)) {
      const selectedSuffix = document.createRange();
      selectedSuffix.selectNodeContents(startContent);
      selectedSuffix.setStart(range.startContainer, range.startOffset);
      if (selectedSuffix.toString().length === 0) {
        const nextLine = startElement.nextElementSibling;
        if (nextLine instanceof HTMLElement && nextLine.matches(".file-source-line[data-line-number]")) {
          startElement = nextLine;
          startLine = Number(startElement.dataset.lineNumber);
        }
      }
    }

    const endContent = endElement.querySelector<HTMLElement>(".file-source-line-content");
    if (endContent?.contains(range.endContainer)) {
      const selectedPrefix = document.createRange();
      selectedPrefix.selectNodeContents(endContent);
      selectedPrefix.setEnd(range.endContainer, range.endOffset);
      if (selectedPrefix.toString().length === 0) {
        const previousLine = endElement.previousElementSibling;
        if (previousLine instanceof HTMLElement && previousLine.matches(".file-source-line[data-line-number]")) {
          endElement = previousLine;
          endLine = Number(endElement.dataset.lineNumber);
        }
      }
    }
  }

  if (startLine > endLine) return null;
  return { startLine, endLine };
}

function copyFileViewerSelectionWithoutGutters(event: ReactClipboardEvent<HTMLElement>) {
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed || selection.rangeCount === 0) return;
  const range = selection.getRangeAt(0);
  if (!event.currentTarget.contains(range.commonAncestorContainer)) return;
  const startElement = range.startContainer.nodeType === Node.ELEMENT_NODE
    ? range.startContainer as Element
    : range.startContainer.parentElement;
  const endElement = range.endContainer.nodeType === Node.ELEMENT_NODE
    ? range.endContainer as Element
    : range.endContainer.parentElement;
  const codeView = startElement?.closest(".file-source-view, .file-diff-view");
  if (!codeView || !endElement || !codeView.contains(endElement)) return;

  const fragment = range.cloneContents();
  fragment.querySelectorAll(".file-viewer-copy-excluded").forEach((node) => node.remove());

  const container = document.createElement("div");
  container.style.position = "fixed";
  container.style.left = "-100000px";
  container.style.top = "0";
  container.style.whiteSpace = "pre";
  container.append(fragment);
  document.body.append(container);
  const text = container.innerText;
  container.remove();
  event.clipboardData.setData("text/plain", text);
  event.preventDefault();
}

function SourceCodeRenderer({ rows, stylesheet, useInlineStyles, wrapLines, flashLine, lineSearchRanges, activeSearchIndex = -1 }: SourceCodeRendererProps) {
  return rows.map((row, lineIndex) => {
    const children = row.children ?? [];
    const firstChildClasses = children[0]?.properties?.className;
    const hasLineNumber = Array.isArray(firstChildClasses)
      && firstChildClasses.includes("react-syntax-highlighter-line-number");
    const lineNumberNode = hasLineNumber ? children[0] : null;
    const ranges = lineSearchRanges?.get(lineIndex + 1);
    const rawContentNodes = hasLineNumber ? children.slice(1) : children;
    const contentNodes = ranges && ranges.length > 0
      ? highlightSyntaxNodes(
          rawContentNodes as unknown as SyntaxNode[],
          ranges,
          activeSearchIndex,
        ) as unknown as typeof rawContentNodes
      : rawContentNodes;
    const isFlashLine = flashLine === lineIndex + 1;

    return (
      <span
        className="file-source-line"
        data-line-number={lineIndex + 1}
        data-flash-line={isFlashLine ? "true" : undefined}
        key={`source-line-${lineIndex}`}
        style={{
          display: "flex",
          minWidth: "100%",
          ...(isFlashLine ? {
            background: "rgba(37,99,235,0.14)",
            boxShadow: "inset 3px 0 0 var(--accent)",
          } : {}),
        }}
      >
        {lineNumberNode && <span className="file-viewer-copy-excluded" aria-hidden="true" style={{ display: "contents", userSelect: "none", WebkitUserSelect: "none" }}>{renderSyntaxNode({
          node: lineNumberNode,
          stylesheet,
          useInlineStyles,
          key: `source-line-number-${lineIndex}`,
        })}</span>}
        <span
          className="file-source-line-content"
          style={{
            flex: "1 1 auto",
            minWidth: 0,
            overflowWrap: wrapLines ? "anywhere" : "normal",
            whiteSpace: wrapLines ? "pre-wrap" : "pre",
          }}
        >
          {contentNodes.map((node, tokenIndex) => renderSyntaxNode({
            node,
            stylesheet,
            useInlineStyles,
            key: `source-token-${lineIndex}-${tokenIndex}`,
          }))}
        </span>
      </span>
    );
  });
}

function getFileApiUrl(
  filePath: string,
  type: "read" | "download" | "meta" | "preview" | "watch",
  sourceSessionId?: string | null,
  params: Record<string, string | number | undefined> = {},
): string {
  const encoded = encodeFilePathForApi(filePath);
  const searchParams = new URLSearchParams({ type });
  if (sourceSessionId) searchParams.set("sessionId", sourceSessionId);
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined) searchParams.set(key, String(value));
  }
  return `/api/files/${encoded}?${searchParams.toString()}`;
}


type DiffLine = {
  type: "unchanged" | "removed" | "added";
  text: string;
  oldLineNo: number | null;
  newLineNo: number | null;
};

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function diffLines(patch: string): DiffLine[] {
  const files = parseUnifiedPatch(patch);
  if (!files) return [];

  return files.flatMap((file) => file.rows.flatMap((row): DiffLine[] => {
    if (row.type === "hunk") return [];
    if (row.left.type === "context" && row.right.type === "context") {
      return [{
        type: "unchanged",
        text: row.right.text,
        oldLineNo: row.left.lineNo,
        newLineNo: row.right.lineNo,
      }];
    }

    const lines: DiffLine[] = [];
    if (row.left.type === "removed") {
      lines.push({
        type: "removed",
        text: row.left.text,
        oldLineNo: row.left.lineNo,
        newLineNo: null,
      });
    }
    if (row.right.type === "added") {
      lines.push({
        type: "added",
        text: row.right.text,
        oldLineNo: null,
        newLineNo: row.right.lineNo,
      });
    }
    return lines;
  }));
}

function DiffView({ patch }: { patch: string }) {
  const { t } = useI18n();
  const diff = diffLines(patch);

  const hasChanges = diff.some((l) => l.type !== "unchanged");
  if (!hasChanges) {
    return (
      <div style={{ padding: "12px 16px", fontSize: "var(--code-font-size)", color: "var(--text-dim)", fontFamily: "var(--font-mono)" }}>
        {t("i18n.noChanges")}
      </div>
    );
  }

  // Render with context: show 3 lines around each change, collapse the rest
  const CONTEXT = 3;
  const changed = new Set(diff.flatMap((l, i) => (l.type !== "unchanged" ? [i] : [])));
  const visible = new Set<number>();
  for (const ci of changed) {
    for (let j = Math.max(0, ci - CONTEXT); j <= Math.min(diff.length - 1, ci + CONTEXT); j++) {
      visible.add(j);
    }
  }

  const segments: Array<{ hidden: true; count: number } | { hidden: false; lines: DiffLine[] }> = [];
  let i = 0;
  while (i < diff.length) {
    if (visible.has(i)) {
      const block: DiffLine[] = [];
      while (i < diff.length && visible.has(i)) {
        block.push(diff[i]);
        i++;
      }
      segments.push({ hidden: false, lines: block });
    } else {
      let count = 0;
      while (i < diff.length && !visible.has(i)) {
        count++;
        i++;
      }
      segments.push({ hidden: true, count });
    }
  }

  return (
    <div
      className="file-diff-view"
      style={{
        width: "max-content",
        minWidth: "100%",
        ...FILE_CODE_STYLE,
      }}
    >
      {segments.map((seg, si) => {
        if (seg.hidden) {
          const result = (
            <div
              key={si}
              style={{
                padding: "2px 16px",
                color: "var(--text-dim)",
                background: "var(--bg-panel)",
                fontSize: 11,
                borderTop: "1px solid var(--border)",
                borderBottom: "1px solid var(--border)",
              }}
            >
              ... {seg.count} unchanged lines ...
            </div>
          );
          return result;
        }
        const lines = seg.lines.map((line, li) => {
          const bg =
            line.type === "added"
              ? "rgba(0,200,80,0.12)"
              : line.type === "removed"
              ? "rgba(240,60,60,0.14)"
              : "transparent";
          const prefix =
            line.type === "added" ? "+" : line.type === "removed" ? "-" : " ";
          const prefixColor =
            line.type === "added" ? "#4ade80" : line.type === "removed" ? "#f87171" : "var(--text-dim)";

          return (
            <div
              key={li}
              className="file-diff-line"
              style={{
                display: "flex",
                minWidth: "100%",
                background: bg,
                borderLeft: line.type === "added"
                  ? "3px solid #4ade80"
                  : line.type === "removed"
                  ? "3px solid #f87171"
                  : "3px solid transparent",
              }}
            >
              <span
                className="file-viewer-copy-excluded"
                aria-hidden="true"
                style={FILE_LINE_NUMBER_STYLE}
              >
                {line.type === "removed" ? line.oldLineNo : line.newLineNo}
              </span>
              <span
                className="file-viewer-copy-excluded"
                aria-hidden="true"
                style={{
                  minWidth: 16,
                  padding: "0 6px",
                  color: prefixColor,
                  userSelect: "none",
                  WebkitUserSelect: "none",
                  flexShrink: 0,
                  fontWeight: 600,
                }}
              >
                {prefix}
              </span>
              <span
                className="file-diff-line-content"
                style={{
                  flexShrink: 0,
                  padding: "0 8px 0 0",
                  whiteSpace: "pre",
                  color: "var(--text)",
                }}
              >
                {line.text || "\u00a0"}
              </span>
            </div>
          );
        });
        return <div key={si}>{lines}</div>;
      })}
    </div>
  );
}

function ImageViewer({ filePath, cwd, sourceSessionId, watchEnabled = true }: Props) {
  const { t } = useI18n();
  const [watching, setWatching] = useState(false);
  const [bust, setBust] = useState(0);
  const [size, setSize] = useState<number | null>(null);
  const [naturalSize, setNaturalSize] = useState<{ w: number; h: number } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const esRef = useRef<EventSource | null>(null);
  const syncRequestRef = useRef(0);

  const ext = getFileName(filePath).toLowerCase().split(".").pop() ?? "";

  useEffect(() => {
    setBust(0);
    setSize(null);
    setNaturalSize(null);
    setError(null);
    setWatching(false);
  }, [filePath, sourceSessionId]);

  useEffect(() => {
    setWatching(false);

    if (esRef.current) {
      esRef.current.close();
      esRef.current = null;
    }

    if (!watchEnabled) return;

    let active = true;
    const synchronize = () => {
      const requestId = ++syncRequestRef.current;
      fetch(getFileApiUrl(filePath, "meta", sourceSessionId))
        .then((response) => response.json())
        .then((next: { size?: number; error?: string }) => {
          if (!active || requestId !== syncRequestRef.current) return;
          if (next.error) {
            setError(next.error);
            return;
          }
          if (typeof next.size === "number") setSize(next.size);
          setNaturalSize(null);
          setError(null);
          setBust((value) => value + 1);
        })
        .catch((nextError) => {
          if (active && requestId === syncRequestRef.current) setError(String(nextError));
        });
    };

    const es = new EventSource(getFileApiUrl(filePath, "watch", sourceSessionId));
    esRef.current = es;

    es.addEventListener("connected", () => {
      setWatching(true);
      synchronize();
    });
    es.addEventListener("change", (e) => {
      syncRequestRef.current += 1;
      try {
        const d = JSON.parse((e as MessageEvent).data) as { size?: number };
        if (typeof d.size === "number") setSize(d.size);
      } catch { /* ignore */ }
      setNaturalSize(null);
      setError(null);
      setBust((b) => b + 1);
    });
    const markDisconnected = () => {
      setWatching(false);
    };
    es.addEventListener("error", markDisconnected);
    es.onerror = markDisconnected;

    return () => {
      active = false;
      es.close();
      if (esRef.current === es) esRef.current = null;
    };
  }, [filePath, sourceSessionId, watchEnabled]);

  const src = getFileApiUrl(filePath, "read", sourceSessionId, bust ? { v: bust } : undefined);

  const formatSizeStr = size != null ? formatSize(size) : null;

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%", overflow: "hidden" }}>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 12,
          padding: "4px 16px",
          borderBottom: "1px solid var(--border)",
          fontSize: 11,
          color: "var(--text-dim)",
          background: "var(--bg)",
          flexShrink: 0,
        }}
      >
        <span style={{ fontFamily: "var(--font-mono)" }} title={filePath}>
          {getRelativeFilePath(filePath, cwd)}
        </span>
        <span style={{ marginLeft: "auto" }}>{ext || "image"}</span>
        {naturalSize && <span>{naturalSize.w} × {naturalSize.h}</span>}
        {formatSizeStr && <span>{formatSizeStr}</span>}
        <span
          title={watching ? t("i18n.liveSync") : t("i18n.notWatching")}
          style={{ display: "flex", alignItems: "center", gap: 4, color: watching ? "#4ade80" : "var(--text-dim)" }}
        >
          <span
            style={{
              width: 7,
              height: 7,
              borderRadius: "50%",
              background: watching ? "#4ade80" : "var(--border)",
              display: "inline-block",
              boxShadow: watching ? "0 0 4px #4ade80" : "none",
            }}
          />
          {watching ? "live" : "static"}
        </span>
        <RevealFileButton filePath={filePath} sourceSessionId={sourceSessionId} className="file-viewer-icon-button" />
      </div>
      <div
        style={{
          flex: 1,
          overflow: "auto",
          background: "var(--bg-panel)",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          padding: 16,
          backgroundImage:
            "linear-gradient(45deg, var(--bg) 25%, transparent 25%), linear-gradient(-45deg, var(--bg) 25%, transparent 25%), linear-gradient(45deg, transparent 75%, var(--bg) 75%), linear-gradient(-45deg, transparent 75%, var(--bg) 75%)",
          backgroundSize: "16px 16px",
          backgroundPosition: "0 0, 0 8px, 8px -8px, -8px 0px",
        }}
      >
        {error ? (
          <div style={{ color: "#f87171", fontSize: 13 }}>{error}</div>
        ) : (
          // eslint-disable-next-line @next/next/no-img-element
          <img
            src={src}
            alt={filePath}
            onLoad={(e) => {
              const img = e.currentTarget;
              setNaturalSize({ w: img.naturalWidth, h: img.naturalHeight });
            }}
            onError={() => setError("Failed to load image")}
            style={{
              maxWidth: "100%",
              maxHeight: "100%",
              objectFit: "contain",
              boxShadow: "0 2px 8px rgba(0,0,0,0.15)",
            }}
          />
        )}
      </div>
    </div>
  );
}

function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds)) return "";
  const totalSeconds = Math.round(seconds);
  const mins = Math.floor(totalSeconds / 60);
  const secs = totalSeconds % 60;
  return `${mins}:${String(secs).padStart(2, "0")}`;
}

function AudioViewer({ filePath, cwd, sourceSessionId, watchEnabled = true }: Props) {
  const { t } = useI18n();
  const [watching, setWatching] = useState(false);
  const [bust, setBust] = useState(0);
  const [size, setSize] = useState<number | null>(null);
  const [duration, setDuration] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const esRef = useRef<EventSource | null>(null);
  const syncRequestRef = useRef(0);

  const ext = getFileName(filePath).toLowerCase().split(".").pop() ?? "";

  useEffect(() => {
    setBust(0);
    setSize(null);
    setDuration(null);
    setError(null);
    setWatching(false);
  }, [filePath, sourceSessionId]);

  useEffect(() => {
    setWatching(false);

    if (esRef.current) {
      esRef.current.close();
      esRef.current = null;
    }

    if (!watchEnabled) return;

    let active = true;
    const synchronize = () => {
      const requestId = ++syncRequestRef.current;
      fetch(getFileApiUrl(filePath, "meta", sourceSessionId))
        .then((response) => response.json())
        .then((next: { size?: number; error?: string }) => {
          if (!active || requestId !== syncRequestRef.current) return;
          if (next.error) {
            setError(next.error);
            return;
          }
          if (typeof next.size === "number") setSize(next.size);
          setDuration(null);
          setError(null);
          setBust((value) => value + 1);
        })
        .catch((nextError) => {
          if (active && requestId === syncRequestRef.current) setError(String(nextError));
        });
    };

    const es = new EventSource(getFileApiUrl(filePath, "watch", sourceSessionId));
    esRef.current = es;

    es.addEventListener("connected", () => {
      setWatching(true);
      synchronize();
    });
    es.addEventListener("change", (e) => {
      syncRequestRef.current += 1;
      try {
        const d = JSON.parse((e as MessageEvent).data) as { size?: number };
        if (typeof d.size === "number") setSize(d.size);
      } catch { /* ignore */ }
      setDuration(null);
      setError(null);
      setBust((b) => b + 1);
    });
    const markDisconnected = () => {
      setWatching(false);
    };
    es.addEventListener("error", markDisconnected);
    es.onerror = markDisconnected;

    return () => {
      active = false;
      es.close();
      if (esRef.current === es) esRef.current = null;
    };
  }, [filePath, sourceSessionId, watchEnabled]);

  const src = getFileApiUrl(filePath, "read", sourceSessionId, bust ? { v: bust } : undefined);

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%", overflow: "hidden" }}>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 12,
          padding: "4px 16px",
          borderBottom: "1px solid var(--border)",
          fontSize: 11,
          color: "var(--text-dim)",
          background: "var(--bg)",
          flexShrink: 0,
        }}
      >
        <span style={{ fontFamily: "var(--font-mono)" }} title={filePath}>
          {getRelativeFilePath(filePath, cwd)}
        </span>
        <span style={{ marginLeft: "auto" }}>{ext || "audio"}</span>
        {duration != null && <span>{formatDuration(duration)}</span>}
        {size != null && <span>{formatSize(size)}</span>}
        <span
          title={watching ? t("i18n.liveSync") : t("i18n.notWatching")}
          style={{ display: "flex", alignItems: "center", gap: 4, color: watching ? "#4ade80" : "var(--text-dim)" }}
        >
          <span
            style={{
              width: 7,
              height: 7,
              borderRadius: "50%",
              background: watching ? "#4ade80" : "var(--border)",
              display: "inline-block",
              boxShadow: watching ? "0 0 4px #4ade80" : "none",
            }}
          />
          {watching ? "live" : "static"}
        </span>
        <RevealFileButton filePath={filePath} sourceSessionId={sourceSessionId} className="file-viewer-icon-button" />
      </div>
      <div
        style={{
          flex: 1,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          padding: 24,
          background: "var(--bg-panel)",
        }}
      >
        <div style={{ width: "min(680px, 100%)" }}>
          {error && (
            <div style={{ color: "#f87171", fontSize: 13, marginBottom: 12, textAlign: "center" }}>
              {error}
            </div>
          )}
          <audio
            key={src}
            controls
            preload="metadata"
            src={src}
            onLoadedMetadata={(e) => setDuration(e.currentTarget.duration)}
            onError={() => setError("Failed to load audio")}
            style={{ width: "100%" }}
          />
        </div>
      </div>
    </div>
  );
}

function DocumentViewer({ filePath, cwd, sourceSessionId, watchEnabled = true }: Props) {
  const { t } = useI18n();
  const [watching, setWatching] = useState(false);
  const [bust, setBust] = useState(0);
  const [size, setSize] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const esRef = useRef<EventSource | null>(null);
  const syncRequestRef = useRef(0);

  const ext = getFileExt(filePath);
  const isPdf = ext === "pdf";
  const previewUrl = isPdf
    ? getFileApiUrl(filePath, "read", sourceSessionId, bust ? { v: bust } : undefined)
    : getFileApiUrl(filePath, "preview", sourceSessionId, bust ? { v: bust } : undefined);

  useEffect(() => {
    setBust(0);
    setSize(null);
    setError(null);
    setWatching(false);

    let active = true;
    const requestId = ++syncRequestRef.current;
    fetch(getFileApiUrl(filePath, "meta", sourceSessionId))
      .then((r) => r.json())
      .then((d: { size?: number; error?: string }) => {
        if (!active || requestId !== syncRequestRef.current) return;
        if (d.error) setError(d.error);
        if (typeof d.size === "number") {
          setSize(d.size);
          if (!isPdf && d.size > DOCX_PREVIEW_MAX_BYTES) {
            setError("DOCX too large for preview (>10MB)");
          }
        }
      })
      .catch((nextError) => {
        if (active && requestId === syncRequestRef.current) setError(String(nextError));
      });

    return () => {
      active = false;
    };
  }, [filePath, isPdf, sourceSessionId]);

  useEffect(() => {
    setWatching(false);

    if (esRef.current) {
      esRef.current.close();
      esRef.current = null;
    }

    if (!watchEnabled) return;

    let active = true;
    const synchronize = () => {
      const requestId = ++syncRequestRef.current;
      fetch(getFileApiUrl(filePath, "meta", sourceSessionId))
        .then((r) => r.json())
        .then((d: { size?: number; error?: string }) => {
          if (!active || requestId !== syncRequestRef.current) return;
          if (d.error) {
            setError(d.error);
            return;
          }
          if (typeof d.size === "number") {
            setSize(d.size);
            if (!isPdf && d.size > DOCX_PREVIEW_MAX_BYTES) {
              setError("DOCX too large for preview (>10MB)");
              return;
            }
          }
          setError(null);
          setBust((value) => value + 1);
        })
        .catch((nextError) => {
          if (active && requestId === syncRequestRef.current) setError(String(nextError));
        });
    };

    const es = new EventSource(getFileApiUrl(filePath, "watch", sourceSessionId));
    esRef.current = es;

    es.addEventListener("connected", () => {
      setWatching(true);
      synchronize();
    });
    es.addEventListener("change", (e) => {
      syncRequestRef.current += 1;
      try {
        const d = JSON.parse((e as MessageEvent).data) as { size?: number };
        if (typeof d.size === "number") {
          setSize(d.size);
          if (!isPdf && d.size > DOCX_PREVIEW_MAX_BYTES) {
            setError("DOCX too large for preview (>10MB)");
            return;
          }
        }
      } catch { /* ignore */ }
      setError(null);
      setBust((b) => b + 1);
    });
    const markDisconnected = () => {
      setWatching(false);
    };
    es.addEventListener("error", markDisconnected);
    es.onerror = markDisconnected;

    return () => {
      active = false;
      es.close();
      if (esRef.current === es) esRef.current = null;
    };
  }, [filePath, isPdf, sourceSessionId, watchEnabled]);

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%", overflow: "hidden" }}>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 12,
          padding: "4px 16px",
          borderBottom: "1px solid var(--border)",
          fontSize: 11,
          color: "var(--text-dim)",
          background: "var(--bg)",
          flexShrink: 0,
        }}
      >
        <span style={{ fontFamily: "var(--font-mono)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={filePath}>
          {getRelativeFilePath(filePath, cwd)}
        </span>
        <span style={{ marginLeft: "auto" }}>{ext === "docx" ? "docx preview" : "pdf"}</span>
        {size != null && <span>{formatSize(size)}</span>}
        <RevealFileButton filePath={filePath} sourceSessionId={sourceSessionId} className="file-viewer-icon-button" />
        <span
          title={watching ? t("i18n.liveSync") : t("i18n.notWatching")}
          style={{ display: "flex", alignItems: "center", gap: 4, color: watching ? "#4ade80" : "var(--text-dim)", flexShrink: 0 }}
        >
          <span
            style={{
              width: 7,
              height: 7,
              borderRadius: "50%",
              background: watching ? "#4ade80" : "var(--border)",
              display: "inline-block",
              boxShadow: watching ? "0 0 4px #4ade80" : "none",
            }}
          />
          {watching ? "live" : "static"}
        </span>
      </div>
      <div style={{ flex: 1, minHeight: 0, background: "var(--bg-panel)" }}>
        {error ? (
          <div style={{ height: "100%", display: "flex", alignItems: "center", justifyContent: "center", padding: 24, color: "#f87171", fontSize: 13, textAlign: "center" }}>
            {error}
          </div>
        ) : (
          <iframe
            key={previewUrl}
            src={previewUrl}
            sandbox={isPdf ? undefined : "allow-same-origin"}
            title={t("i18n.previewFile", { file: getFileName(filePath) })}
            style={{ width: "100%", height: "100%", border: "none", background: isPdf ? "var(--bg)" : "#eef1f5" }}
          />
        )}
      </div>
    </div>
  );
}

export function FileViewer({
  filePath,
  cwd,
  sourceSessionId,
  onOpenFile,
  onMentionLines,
  onAtMention,
  gitRefreshKey,
  initialDisplayMode,
  initialState,
  initialLine,
  onRevealHandled,
  onStateChange,
  watchEnabled = true,
  editable = false,
  onDirtyChange,
  tabId,
  sessionId,
}: Props) {
  if (isImagePath(filePath)) {
    return <ImageViewer filePath={filePath} cwd={cwd} sourceSessionId={sourceSessionId} watchEnabled={watchEnabled} />;
  }
  if (isAudioPath(filePath)) {
    return <AudioViewer filePath={filePath} cwd={cwd} sourceSessionId={sourceSessionId} watchEnabled={watchEnabled} />;
  }
  if (isDocumentPreviewPath(filePath)) {
    return <DocumentViewer filePath={filePath} cwd={cwd} sourceSessionId={sourceSessionId} watchEnabled={watchEnabled} />;
  }
  return (
    <TextFileViewer
      filePath={filePath}
      cwd={cwd}
      sourceSessionId={sourceSessionId}
      onOpenFile={onOpenFile}
      onMentionLines={onMentionLines}
      onAtMention={onAtMention}
      gitRefreshKey={gitRefreshKey}
      initialDisplayMode={initialDisplayMode}
      initialState={initialState}
      initialLine={initialLine}
      onRevealHandled={onRevealHandled}
      onStateChange={onStateChange}
      watchEnabled={watchEnabled}
      editable={editable}
      onDirtyChange={onDirtyChange}
      tabId={tabId}
    />
  );
}

function TextFileViewer({
  filePath,
  cwd,
  sourceSessionId,
  onOpenFile,
  onMentionLines,
  onAtMention,
  gitRefreshKey,
  initialDisplayMode,
  initialState,
  initialLine,
  onRevealHandled,
  onStateChange,
  watchEnabled = true,
  editable = false,
  onDirtyChange,
  tabId,
}: Props) {
  const { isDark } = useTheme();
  const { t } = useI18n();
  const [data, setData] = useState<FileData | null>(null);
  const [gitDiff, setGitDiff] = useState<GitFileDiffResponse | null>(null);
  const [gitDiffResolved, setGitDiffResolved] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const explicitInitialDisplayMode = resolveInitialFileDisplayMode(initialState, initialDisplayMode);
  // “打开时是否带行号”只在挂载时算一次：行号被消费后 initialLine 会变回 undefined，
  // 若继续跟随它，默认模式会在链接定位完成后从 source 翻成 edit —— 点链接看代码
  // 会突然变成编辑器，而且这次切换里 Monaco 只停在自己的 loading 占位上。
  const openedWithLineRef = useRef(initialLine !== undefined);
  // 没保存过的标签状态就按文件类型给默认模式：markdown 打开即预览，其余可编辑文件
  // 默认 Edit。显式模式提示、恢复的标签状态与行号跳转优先。
  const requestedInitialDisplayMode: DisplayMode = (
    initialState === undefined
    && initialDisplayMode === undefined
  )
    ? defaultOpenDisplayMode({
      editable,
      language: undefined,
      openedWithLine: openedWithLineRef.current,
    })
    : explicitInitialDisplayMode;
  const initialWrapLines = initialState?.wrapLines ?? false;
  const initialScrollTop = initialState?.scrollTop ?? 0;
  const initialScrollLeft = initialState?.scrollLeft ?? 0;
  const [displayMode, setDisplayMode] = useState<DisplayMode>(requestedInitialDisplayMode);
  const [wrapLines, setWrapLines] = useState(initialWrapLines);
  const [watching, setWatching] = useState(false);
  // --- 编辑模式 ---
  // 编辑是 displayMode 的第四种取值（edit/source/preview/diff），editing 由当前
  // 模式推导，不再是独立开关；草稿在保存后仍保留，切走再切回不会丢。
  /** 编辑缓冲区：null = 尚未进入编辑态；字符串 = 当前草稿。 */
  const [draft, setDraft] = useState<string | null>(null);
  // 编辑器撤销栈的空/满状态：工具栏的撤销/恢复按钮据此启用。
  const [undoState, setUndoState] = useState({ canUndo: false, canRedo: false });
  // 修饰键的显示名：服务端渲不出平台，首帧先给 Ctrl，客户端再改成 Cmd。
  const [modifierKey, setModifierKey] = useState<ModifierKeyLabel>("Ctrl");
  useEffect(() => {
    if (isMacPlatform()) setModifierKey("Cmd");
  }, []);
  const editing = displayMode === "edit" && editable && data !== null;
  const [saving, setSaving] = useState(false);
  const savingRef = useRef(false);
  /** 保存期间落盘的 watch 事件：保存结束后重新核对一次，避免漏掉外部写入。 */
  const resyncAfterSaveRef = useRef(false);
  /** 由 watch effect 装配的同步函数，供保存流程在结束后调用。 */
  const resyncRef = useRef<(() => void) | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  /** 磁盘内容变了但本地草稿是脏的：不覆盖草稿，只提示。 */
  const [diskChangePending, setDiskChangePending] = useState(false);
  const editorHandleRef = useRef<MonacoEditorHandle | null>(null);
  /** 把磁盘内容写回 Monaco 时也会触发 onChange，用它把这次变更排除在“用户编辑”之外。 */
  const applyingExternalRef = useRef(false);
  const esRef = useRef<EventSource | null>(null);
  const contentRequestRef = useRef(0);
  const gitDiffRequestRef = useRef(0);
  const contentRef = useRef<HTMLDivElement | null>(null);
  const autoDiffAppliedRef = useRef(false);
  // 默认模式的判定全靠 defaultOpenDisplayMode（见 lib/file-viewer-state.ts）：
  // markdown 无论能不能编辑都先给预览，html 只在不可编辑时预览，其余可编辑文件
  // 默认 Edit。语言要等文件内容回来才知道，所以正式裁决拖到这个 effect 里，
  // 资格 ref 也在这里才消费 —— 否则首帧（language 还是 undefined）就白跑了一轮。
  const defaultModeEligibleRef = useRef(
    initialState === undefined && initialDisplayMode === undefined && !openedWithLineRef.current,
  );
  const scrollRestorePendingRef = useRef(true);
  const viewerStateRef = useRef<FileViewerState>({
    displayMode: requestedInitialDisplayMode,
    wrapLines: initialWrapLines,
    scrollTop: initialScrollTop,
    scrollLeft: initialScrollLeft,
  });
  const onStateChangeRef = useRef(onStateChange);
  const onRevealHandledRef = useRef(onRevealHandled);
  const [selectedLineRange, setSelectedLineRange] = useState<SelectedLineRange | null>(null);
  const [flashLine, setFlashLine] = useState<number | null>(null);
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchCaseSensitive, setSearchCaseSensitive] = useState(false);
  const [currentMatchIndex, setCurrentMatchIndex] = useState(0);
  const searchInputRef = useRef<HTMLInputElement | null>(null);
  const shellRef = useRef<HTMLDivElement | null>(null);
  const pointerInsideRef = useRef(false);
  const content = data?.content ?? "";

  const searchMatches = useMemo(
    () => findFileSearchMatches(content, searchQuery, searchCaseSensitive),
    [content, searchQuery, searchCaseSensitive],
  );
  const lineSearchRanges = useMemo(
    () => buildLineSearchRangeMap(searchMatches),
    [searchMatches],
  );
  const activeMatchIndex = searchMatches.length === 0
    ? -1
    : Math.min(currentMatchIndex, searchMatches.length - 1);
  // 待定位的行在加载完成后消费一次；消费前不会被当成普通滚动状态写回。
  // 首次挂载即带入，重挂载（切文件/再点一次链接）由 AppShell 的 key 触发。
  const pendingRevealLineRef = useRef<number | null>(initialLine ?? null);

  onStateChangeRef.current = onStateChange;
  onRevealHandledRef.current = onRevealHandled;

  const updateDisplayMode = useCallback((nextDisplayMode: DisplayMode) => {
    viewerStateRef.current.displayMode = nextDisplayMode;
    setDisplayMode(nextDisplayMode);
  }, []);

  const toggleWrapLines = useCallback(() => {
    setWrapLines((current) => {
      const next = !current;
      viewerStateRef.current.wrapLines = next;
      return next;
    });
  }, []);

  // --- 编辑模式控制流 ---
  const draftRef = useRef(draft);
  draftRef.current = draft;
  const originalContentRef = useRef<string | null>(null);
  const userEditedRef = useRef(false);
  // 有未保存改动即为 dirty，与当前是否停在 Edit 模式无关。
  const isDirty = draft !== null && draft !== (data?.content ?? "");
  // watch 的事件回调在 effect 里，读 ref 才能拿到最新的 dirty 状态。
  const dirtyRef = useRef(isDirty);
  dirtyRef.current = isDirty;

  const persistContent = useCallback(async (content = draft) => {
    // Timers, blur, and the toolbar can all save the same edit nearly at once.
    if (content === null || savingRef.current) return;
    const originalContent = originalContentRef.current ?? data?.content ?? "";
    if (!userEditedRef.current || content === originalContent) {
      // 没有实际改动就保持现状，保存不再把人踢出编辑模式。
      setSaveError(null);
      return;
    }
    if (content === "" && originalContent !== "" && !window.confirm(t("i18n.confirmEmptyFile"))) {
      return;
    }

    savingRef.current = true;
    setSaving(true);
    setDraft(content);
    setSaveError(null);
    try {
      const res = await fetch(getFileApiUrl(filePath, "read", sourceSessionId), {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          content,
          allowEmpty: content === "",
        }),
      });
      const next = await res.json().catch(() => ({})) as { size?: number; error?: string };
      if (!res.ok || next.error) throw new Error(next.error ?? `HTTP ${res.status}`);
      setData((current) => current
        ? { ...current, content, size: next.size ?? current.size }
        : current);
      // 保存成功后继续留在编辑模式：基线前移，草稿与磁盘同步。
      originalContentRef.current = content;
      userEditedRef.current = false;
      draftRef.current = content;
      dirtyRef.current = false;
      setDraft(content);
      // 用户选择以本地草稿为准，之前记下的磁盘变更提示随之作废。
      setDiskChangePending(false);
    } catch (reason) {
      setDraft(content);
      setSaveError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      savingRef.current = false;
      setSaving(false);
      if (resyncAfterSaveRef.current) {
        resyncAfterSaveRef.current = false;
        resyncRef.current?.();
      }
    }
  }, [data?.content, draft, sourceSessionId, filePath, t]);

  // 编辑草稿只在显式保存时提交；切换标签或关闭窗口不会隐式写文件。
  // 这样可以避免失焦、定时器和组件卸载时把瞬时空值写入磁盘。
  const enterEditMode = useCallback(() => {
    if (!data) return;
    // 首次进入编辑时记录基线；再次切回 Edit 保留尚未保存的草稿。
    if (draftRef.current === null) {
      originalContentRef.current = data.content;
      userEditedRef.current = false;
      draftRef.current = data.content;
      setDraft(data.content);
    }
    setSaveError(null);
    // 只读搜索栏与 Monaco 无关，进入编辑态时收起，退出后不再自动弹出。
    setSearchOpen(false);
    updateDisplayMode("edit");
  }, [data, updateDisplayMode]);

  const handleEditorChange = useCallback((next: string) => {
    // 写回磁盘内容时 Monaco 也会回调 onChange，这不是用户编辑。
    if (applyingExternalRef.current) return;
    // 只有 Monaco 的实际变更回调才会标记为用户编辑。
    userEditedRef.current = true;
    draftRef.current = next;
    setDraft(next);
  }, []);

  const handleSave = useCallback(() => {
    void persistContent(draftRef.current ?? draft);
  }, [draft, persistContent]);

  // dirty 变化上报给 Tab 层（圆点标记 + 关闭确认）。
  useEffect(() => {
    if (!onDirtyChange || !tabId) return;
    onDirtyChange(tabId, isDirty);
    return () => onDirtyChange?.(tabId, false);
  }, [isDirty, onDirtyChange, tabId]);

  useEffect(() => {
    const nextState: FileViewerState = {
      displayMode: requestedInitialDisplayMode,
      wrapLines: initialWrapLines,
      scrollTop: initialScrollTop,
      scrollLeft: initialScrollLeft,
    };

    viewerStateRef.current = nextState;
    scrollRestorePendingRef.current = true;
    autoDiffAppliedRef.current = false;
    setFlashLine(null);
    setDisplayMode(requestedInitialDisplayMode);
    setWrapLines(initialWrapLines);
    // 文件身份变化时丢弃上一个文件的草稿，避免跨文件残留。
    draftRef.current = null;
    originalContentRef.current = null;
    userEditedRef.current = false;
    setDraft(null);
    setSaveError(null);
    setDiskChangePending(false);

    return () => {
      onStateChangeRef.current?.({ ...viewerStateRef.current });
    };
  }, [
    filePath,
    sourceSessionId,
    requestedInitialDisplayMode,
    initialWrapLines,
    initialScrollTop,
    initialScrollLeft,
  ]);

  const fetchContent = useCallback((filePath: string) => {
    const requestId = ++contentRequestRef.current;
    return fetch(getFileApiUrl(filePath, "read", sourceSessionId))
      .then((r) => r.json())
      .then((d: FileData & { error?: string }) => {
        if (requestId !== contentRequestRef.current) return null;
        if (d.error) {
          setError(d.error);
          return null;
        }
        setError(null);
        setData(d);
        return d;
      })
      .catch((e) => {
        if (requestId !== contentRequestRef.current) return null;
        setError(String(e));
        return null;
      });
  }, [sourceSessionId]);

  const fetchGitDiff = useCallback(async (targetPath: string) => {
    const requestId = ++gitDiffRequestRef.current;
    if (!cwd) {
      setGitDiff(null);
      setGitDiffResolved(true);
      return;
    }

    try {
      const params = new URLSearchParams({ cwd, path: targetPath });
      const response = await fetch(`/api/git/diff?${params.toString()}`);
      const next = await response.json() as GitFileDiffResponse & { error?: string };
      if (requestId !== gitDiffRequestRef.current) return;
      setGitDiff(response.ok && next.supported && typeof next.patch === "string" ? next : null);
    } catch {
      if (requestId === gitDiffRequestRef.current) setGitDiff(null);
    } finally {
      if (requestId === gitDiffRequestRef.current) setGitDiffResolved(true);
    }
  }, [cwd]);

  /**
   * 把磁盘上的新内容落到 data、草稿基线与 Monaco 模型上。调用方必须先确认草稿
   * 不脏：这里会直接丢弃当前草稿。
   */
  const applyDiskContent = useCallback((next: FileData) => {
    setData(next);
    setError(null);
    setDiskChangePending(false);
    const model = editorHandleRef.current?.getModel();
    // 从未进入过编辑态：只需要更新只读视图。
    if (draftRef.current === null && !model) return;
    originalContentRef.current = next.content;
    userEditedRef.current = false;
    draftRef.current = next.content;
    setDraft(next.content);
    // 编辑态下 Monaco 用 defaultValue 建模型，prop 变化不会回写，必须直接更新模型。
    if (model && !model.isDisposed() && model.getValue() !== next.content) {
      applyingExternalRef.current = true;
      try {
        model.setValue(next.content);
      } finally {
        applyingExternalRef.current = false;
      }
    }
  }, []);

  // Reset and load the file itself when its identity changes. Live watching is
  // managed separately so pausing it never clears the displayed content.
  useEffect(() => {
    let active = true;
    setLoading(true);
    setError(null);
    setData(null);
    setGitDiff(null);
    setGitDiffResolved(false);
    setWatching(false);

    fetchContent(filePath).finally(() => {
      if (active) setLoading(false);
    });

    return () => {
      active = false;
    };
  }, [filePath, fetchContent, sourceSessionId]);

  // 当前标签实时跟随磁盘变化：AppShell 只挂载当前标签的查看器，所以这里天然只
  // 监视正在看的那一个文件。磁盘内容绝不覆盖未保存的草稿，只置提示标志。
  useEffect(() => {
    if (!watchEnabled) return;

    if (esRef.current) {
      esRef.current.close();
      esRef.current = null;
    }
    setWatching(false);

    let active = true;
    const synchronize = () => {
      if (!active) return;
      // 保存期间的事件可能只是自己的写入（此时草稿还是脏的），记下来等保存结束再核对。
      if (savingRef.current) {
        resyncAfterSaveRef.current = true;
        return;
      }
      if (dirtyRef.current) {
        setDiskChangePending(true);
        return;
      }
      void fetchContent(filePath).then((next) => {
        if (!active || !next) return;
        applyDiskContent(next);
        void fetchGitDiff(filePath);
      });
    };
    resyncRef.current = synchronize;

    const es = new EventSource(getFileApiUrl(filePath, "watch", sourceSessionId));
    esRef.current = es;

    es.addEventListener("connected", () => {
      // 服务端只在 watcher 就绪后才发 connected，所以这次快照不会漏掉装配期间的
      // 变化；草稿脏时不校对，否则重连会被当成一次磁盘变更。
      setWatching(true);
      if (!dirtyRef.current) synchronize();
    });
    es.addEventListener("change", synchronize);
    const markDisconnected = () => {
      setWatching(false);
    };
    es.addEventListener("error", markDisconnected);
    es.onerror = markDisconnected;

    return () => {
      active = false;
      if (resyncRef.current === synchronize) resyncRef.current = null;
      es.close();
      if (esRef.current === es) esRef.current = null;
      setWatching(false);
    };
  }, [filePath, sourceSessionId, watchEnabled, fetchContent, applyDiskContent, fetchGitDiff]);

  /** 用户明确选择“以磁盘为准”：丢弃本地草稿，重新读取文件。 */
  const handleReloadFromDisk = useCallback(async () => {
    const next = await fetchContent(filePath);
    if (!next) return;
    applyDiskContent(next);
    void fetchGitDiff(filePath);
  }, [applyDiskContent, fetchContent, fetchGitDiff, filePath]);

  useEffect(() => {
    void fetchGitDiff(filePath);
  }, [fetchGitDiff, filePath, gitRefreshKey]);

  useEffect(() => {
    // A restored choice or explicit mode hint always wins over this default, so the
    // capability ref is only armed when neither was given.
    if (!defaultModeEligibleRef.current || !data?.language) return;
    defaultModeEligibleRef.current = false;
    const resolved = defaultOpenDisplayMode({
      editable,
      language: data.language,
      openedWithLine: false,
    });
    if (resolved !== viewerStateRef.current.displayMode) updateDisplayMode(resolved);
  }, [data?.language, editable, updateDisplayMode]);

  const hasGitDiff = gitDiff?.supported === true && typeof gitDiff.patch === "string";
  const isDeletedDiff = hasGitDiff && gitDiff.status === "deleted";

  // 没未提交改动不等于「打开源码」：对话里的文件链接一律以 diff 起步，文件其实没改动时
  // 退回这个文件本来该有的默认模式 —— 指向未改动 markdown 的链接仍然预览，可编辑的
  // 文件仍然进编辑器。语言要等内容回来才知道，所以 data 还没到就等它，别用 undefined
  // 的语言猜一个 edit。落定后打上 autoDiffApplied 标记：之后这个文件在磁盘上被改出
  // 差异时，不该把正在读或正在改的人拽进 diff。
  useEffect(() => {
    if (!gitDiffResolved || hasGitDiff || displayMode !== "diff" || !data) return;
    autoDiffAppliedRef.current = true;
    updateDisplayMode(defaultOpenDisplayMode({
      editable,
      language: data.language,
      openedWithLine: openedWithLineRef.current,
    }));
  }, [data, displayMode, editable, gitDiffResolved, hasGitDiff, updateDisplayMode]);

  // Wait for the git request before restoring diff mode so the unresolved
  // placeholder cannot immediately demote it back to source.
  useEffect(() => {
    if (requestedInitialDisplayMode === "diff" && hasGitDiff && !autoDiffAppliedRef.current) {
      autoDiffAppliedRef.current = true;
      updateDisplayMode("diff");
    }
  }, [requestedInitialDisplayMode, hasGitDiff, updateDisplayMode]);

  const markdownPreview = useMemo(
    () => (data?.language === "markdown" ? normalizeDisplayMath(data.content) : ""),
    [data],
  );

  const frontmatter = useMemo(
    () => (data?.language === "markdown" ? parseFrontmatter(data.content) : null),
    [data],
  );

  useEffect(() => {
    const updateSelectedLineRange = () => {
      const root = contentRef.current;
      setSelectedLineRange(
        onMentionLines && displayMode === "source" && root
          ? getSelectedSourceLineRange(root, window.getSelection())
          : null,
      );
    };

    updateSelectedLineRange();
    if (!onMentionLines || displayMode !== "source") return;

    document.addEventListener("selectionchange", updateSelectedLineRange);
    return () => document.removeEventListener("selectionchange", updateSelectedLineRange);
  }, [data?.content, displayMode, onMentionLines]);

  const mentionLineRange = useCallback((lineRange: SelectedLineRange | null) => {
    if (!onMentionLines || !lineRange) return;
    onMentionLines(
      getRelativeFilePath(filePath, cwd),
      lineRange.startLine,
      lineRange.endLine,
    );
  }, [cwd, filePath, onMentionLines]);

  useEffect(() => {
    if (!onMentionLines || displayMode !== "source") return;

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.repeat || event.key.toLowerCase() !== "i" || (!event.metaKey && !event.ctrlKey) || event.altKey || event.shiftKey) return;

      const target = event.target;
      if (target instanceof Element && target.closest("input, textarea, [contenteditable='true']")) return;

      const root = contentRef.current;
      const lineRange = root ? getSelectedSourceLineRange(root, window.getSelection()) : null;
      if (!lineRange) return;

      event.preventDefault();
      mentionLineRange(lineRange);
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [displayMode, mentionLineRange, onMentionLines]);

  useEffect(() => {
    if (!scrollRestorePendingRef.current || loading) return;
    if (error && !isDeletedDiff) return;
    if (requestedInitialDisplayMode === "diff" && !gitDiffResolved) return;
    if (requestedInitialDisplayMode === "diff" && hasGitDiff && displayMode !== "diff") return;

    const content = contentRef.current;
    if (!content) return;

    content.scrollTop = viewerStateRef.current.scrollTop;
    content.scrollLeft = viewerStateRef.current.scrollLeft;
    scrollRestorePendingRef.current = false;
  }, [
    data?.content,
    displayMode,
    error,
    gitDiffResolved,
    hasGitDiff,
    isDeletedDiff,
    loading,
    requestedInitialDisplayMode,
  ]);

  // 链接带行号时：加载完成后滚到该行并短暂高亮。只在源码视图生效，
  // 因为预览/差异视图没有与之对应的行号坐标系。
  useEffect(() => {
    const targetLine = pendingRevealLineRef.current;
    if (targetLine === null || loading || displayMode !== "source") return;

    const content = contentRef.current;
    if (!content) return;
    const row = content.querySelector<HTMLElement>(`[data-line-number="${targetLine}"]`);
    if (!row) return;

    pendingRevealLineRef.current = null;
    const contentRect = content.getBoundingClientRect();
    const rowRect = row.getBoundingClientRect();
    content.scrollTop += rowRect.top - contentRect.top - content.clientHeight / 2 + rowRect.height / 2;
    viewerStateRef.current.scrollTop = content.scrollTop;
    setFlashLine(targetLine);
    onRevealHandledRef.current?.();
  }, [data?.content, displayMode, loading, wrapLines]);

  useEffect(() => {
    if (flashLine === null) return;
    const timer = setTimeout(() => setFlashLine(null), 2000);
    return () => clearTimeout(timer);
  }, [flashLine]);

  const openSearch = useCallback(() => setSearchOpen(true), []);

  const closeSearch = useCallback(() => {
    setSearchOpen(false);
    setSearchQuery("");
  }, []);

  const stepMatch = useCallback((delta: number) => {
    setCurrentMatchIndex((current) => {
      const total = searchMatches.length;
      if (total === 0) return 0;
      return (current + delta + total) % total;
    });
  }, [searchMatches.length]);

  const updateSearchQuery = useCallback((value: string) => {
    setSearchQuery(value);
    setCurrentMatchIndex(0);
  }, []);

  useEffect(() => {
    if (searchOpen) searchInputRef.current?.focus();
  }, [searchOpen]);

  // Cmd/Ctrl+F 只在指针悬停或焦点已落在文件预览里时接管，避免抢聊天输入框的快捷键。
  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.repeat) return;
      if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey) return;
      if (event.key.toLowerCase() !== "f" || displayMode !== "source") return;

      const shell = shellRef.current;
      const focusInside = shell ? shell.contains(document.activeElement) : false;
      if (!focusInside && !pointerInsideRef.current) return;

      event.preventDefault();
      setSearchOpen(true);
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [displayMode]);

  // 输入时就要跳到第一个匹配；导航时滚动当前匹配所在行（必要时也横向滚动）。
  useEffect(() => {
    if (!searchOpen || displayMode !== "source" || activeMatchIndex < 0) return;
    const active = searchMatches[activeMatchIndex];
    const container = contentRef.current;
    if (!active || !container) return;

    const row = container.querySelector<HTMLElement>(`[data-line-number="${active.line}"]`);
    if (!row) return;

    const containerRect = container.getBoundingClientRect();
    const rowRect = row.getBoundingClientRect();
    if (rowRect.top < containerRect.top || rowRect.bottom > containerRect.bottom) {
      container.scrollTop += rowRect.top - containerRect.top - container.clientHeight / 2 + rowRect.height / 2;
    }

    if (!wrapLines) {
      const mark = row.querySelector<HTMLElement>(".file-search-match.is-active");
      const markRect = mark?.getBoundingClientRect();
      if (markRect && (markRect.left < containerRect.left + 56 || markRect.right > containerRect.right)) {
        container.scrollLeft += markRect.left - containerRect.left - 64;
      }
    }

    viewerStateRef.current.scrollTop = container.scrollTop;
    viewerStateRef.current.scrollLeft = container.scrollLeft;
  }, [activeMatchIndex, displayMode, searchMatches, searchOpen, wrapLines]);

  // diff 起步时也等 git 结果：先把整篇源码画出来再切成 diff 会闪一下，
  // 而 git diff 只剩一个请求，失败/非 git 仓库也会立即 resolved。
  if (loading || (requestedInitialDisplayMode === "diff" && !gitDiffResolved)) {
    return (
      <div style={{ height: "100%", display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-muted)", fontSize: 13 }}>
        {t("i18n.loading")}
      </div>
    );
  }

  if (error && !isDeletedDiff) {
    return (
      <div style={{ height: "100%", display: "flex", alignItems: "center", justifyContent: "center", color: "#f87171", fontSize: 13 }}>
        {error}
      </div>
    );
  }

  if (!data && !isDeletedDiff) return null;

  const language = data?.language ?? "text";
  const isHtml = language === "html";
  const isMarkdown = language === "markdown";
  const hasPreview = isHtml || isMarkdown;
  const markdownDirectory = getFileDirectory(filePath);
  const lines = content.split("\n");
  const effectiveDisplayMode = isDeletedDiff ? "diff" : displayMode;
  const displayModes: DisplayMode[] = isDeletedDiff
    ? ["diff"]
    : [
        ...(editable ? ["edit" as const] : []),
        "source",
        ...(hasPreview ? ["preview" as const] : []),
        ...(hasGitDiff ? ["diff" as const] : []),
      ];
  const metadata = isDeletedDiff
    ? t("files.deleted")
    : `${language} · ${lines.length} lines · ${formatSize(data!.size)}`;

  return (
    <div
      ref={shellRef}
      className="file-viewer-shell"
      onPointerEnter={() => { pointerInsideRef.current = true; }}
      onPointerLeave={() => { pointerInsideRef.current = false; }}
      style={{ display: "flex", flexDirection: "column", height: "100%", overflow: "hidden" }}
    >
      <div
        className="file-viewer-toolbar"
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          padding: "5px 12px",
          borderBottom: "1px solid var(--border)",
          fontSize: 11,
          color: "var(--text-dim)",
          background: "var(--bg)",
          flexShrink: 0,
        }}
      >
        <span className="file-viewer-path" style={{ fontFamily: "var(--font-mono)" }} title={filePath}>
          {getRelativeFilePath(filePath, cwd)}
        </span>

        <span className="file-viewer-meta" title={metadata}>{metadata}</span>
        {!isDeletedDiff && (
          <span
            title={diskChangePending
              ? t("i18n.fileChangedOnDisk")
              : watching ? t("i18n.liveSync") : t("i18n.notWatching")}
            aria-label={diskChangePending
              ? t("i18n.fileChangedOnDisk")
              : watching ? t("i18n.liveSync") : t("i18n.notWatching")}
            className="file-viewer-live-indicator"
            style={{
              background: diskChangePending ? "#fbbf24" : watching ? "#4ade80" : "var(--border)",
              boxShadow: diskChangePending
                ? "0 0 4px #fbbf24"
                : watching ? "0 0 4px #4ade80" : "none",
            }}
          />
        )}

        <div className="file-viewer-controls">
          {displayModes.length > 1 && (
            <div className="file-viewer-mode-switch" aria-label={t("i18n.fileViewMode")}>
              {displayModes.map((mode) => {
                const active = effectiveDisplayMode === mode;
                return (
                  <button
                    key={mode}
                    type="button"
                    onClick={() => (mode === "edit" ? enterEditMode() : updateDisplayMode(mode))}
                    disabled={mode === "edit" && !data}
                    title={mode === "diff" ? t("i18n.compareHead") : undefined}
                    aria-pressed={active}
                    className="file-viewer-mode-button"
                    style={{
                      background: active ? "var(--bg-selected)" : "transparent",
                      color: active ? "var(--text)" : "var(--text-muted)",
                    }}
                  >
                    {DISPLAY_MODE_LABELS[mode]}
                  </button>
                );
              })}
            </div>
          )}

          <div className="file-viewer-actions">
            {(onAtMention || onMentionLines) && (
              <button
                type="button"
                onPointerDown={(event) => event.preventDefault()}
                onClick={() => {
                  // Mention selected lines when a range is active (and line
                  // mention is wired up); otherwise fall back to a whole-file
                  // @mention. Same button, behavior follows the selection.
                  if (selectedLineRange && onMentionLines) {
                    mentionLineRange(selectedLineRange);
                  } else {
                    onAtMention?.(getRelativeFilePath(filePath, cwd), false);
                  }
                }}
                title={
                  selectedLineRange && onMentionLines
                    ? `${t("i18n.mentionSelectedLines")} (L${selectedLineRange.startLine}${selectedLineRange.startLine !== selectedLineRange.endLine ? `-L${selectedLineRange.endLine}` : ""})`
                    : t("files.insertPath")
                }
                aria-label={t("files.mention")}
                disabled={!onAtMention && !onMentionLines}
                className="file-viewer-icon-button"
              >
                <MentionIcon />
              </button>
            )}
            {effectiveDisplayMode === "source" && (
              <>
                <button
                  type="button"
                  onClick={openSearch}
                  title={t("i18n.findInFile")}
                  aria-label={t("i18n.findInFile")}
                  aria-pressed={searchOpen}
                  className="file-viewer-icon-button"
                  style={{
                    background: searchOpen ? "var(--bg-selected)" : "transparent",
                    color: searchOpen ? "var(--text)" : "var(--text-muted)",
                  }}
                >
                  <Search size={14} strokeWidth={2} aria-hidden="true" />
                </button>
                <button
                  type="button"
                  onClick={toggleWrapLines}
                  title={wrapLines ? t("i18n.disableWrap") : t("i18n.enableWrap")}
                  aria-label={wrapLines ? t("i18n.disableWrap") : t("i18n.enableWrap")}
                  aria-pressed={wrapLines}
                  className="file-viewer-icon-button"
                  style={{
                    background: wrapLines ? "var(--bg-selected)" : "transparent",
                    color: wrapLines ? "var(--text)" : "var(--text-muted)",
                  }}
                >
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                    <path d="M3 6h18" />
                    <path d="M3 12h15a3 3 0 1 1 0 6h-4" />
                    <path d="m16 16-2 2 2 2" />
                    <path d="M3 18h7" />
                  </svg>
                </button>
              </>
            )}
            {editing && data && (
              <>
                <button
                  type="button"
                  onPointerDown={(event) => event.preventDefault()}
                  onClick={() => editorHandleRef.current?.undo()}
                  title={t("i18n.undo", { mod: modifierKey })}
                  aria-label={t("i18n.undo", { mod: modifierKey })}
                  className="file-viewer-icon-button"
                  disabled={!undoState.canUndo}
                >
                  <Undo2 size={14} strokeWidth={2} aria-hidden="true" />
                </button>
                <button
                  type="button"
                  onPointerDown={(event) => event.preventDefault()}
                  onClick={() => editorHandleRef.current?.redo()}
                  title={t("i18n.redo", { mod: modifierKey })}
                  aria-label={t("i18n.redo", { mod: modifierKey })}
                  className="file-viewer-icon-button"
                  disabled={!undoState.canRedo}
                >
                  <Redo2 size={14} strokeWidth={2} aria-hidden="true" />
                </button>
              </>
            )}
            {editing && data && (
              <button
                type="button"
                onPointerDown={(event) => event.preventDefault()}
                onClick={handleSave}
                title={t("i18n.save")}
                aria-label={t("i18n.save")}
                className="file-viewer-icon-button"
                disabled={saving}
                style={{
                  background: isDirty ? "var(--bg-selected)" : "transparent",
                  color: isDirty ? "var(--text)" : "var(--text-muted)",
                }}
              >
                <Save size={14} strokeWidth={2} aria-hidden="true" />
              </button>
            )}
          </div>

          {!isDeletedDiff && <RevealFileButton filePath={filePath} sourceSessionId={sourceSessionId} className="file-viewer-icon-button" />}
        </div>
      </div>

      {saveError && (
        <div className="file-editor-error-bar" role="alert">
          {saveError}
        </div>
      )}

      {diskChangePending && (
        <div className="file-editor-notice-bar" role="status">
          <span>{t("i18n.fileChangedOnDisk")}</span>
          <button
            type="button"
            className="file-editor-notice-action"
            onClick={() => void handleReloadFromDisk()}
          >
            {t("i18n.reloadFromDisk")}
          </button>
        </div>
      )}

      {searchOpen && effectiveDisplayMode === "source" && (
        <div className="file-viewer-search">
          <Search className="file-viewer-search-icon" size={13} strokeWidth={2} aria-hidden="true" />
          <input
            ref={searchInputRef}
            className="file-viewer-search-input"
            value={searchQuery}
            onChange={(event) => updateSearchQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                event.stopPropagation();
                stepMatch(event.shiftKey ? -1 : 1);
              } else if (event.key === "Escape") {
                event.preventDefault();
                event.stopPropagation();
                closeSearch();
              }
            }}
            placeholder={t("i18n.findQueryPlaceholder")}
            aria-label={t("i18n.findInFile")}
            spellCheck={false}
            autoComplete="off"
          />
          <span className="file-viewer-search-count" aria-live="polite">
            {searchMatches.length === 0 ? "0/0" : `${activeMatchIndex + 1}/${searchMatches.length}`}
          </span>
          <button
            type="button"
            onClick={() => stepMatch(-1)}
            disabled={searchMatches.length === 0}
            title={t("i18n.findPrev")}
            aria-label={t("i18n.findPrev")}
            className="file-viewer-icon-button"
          >
            <ChevronUp size={14} strokeWidth={2} aria-hidden="true" />
          </button>
          <button
            type="button"
            onClick={() => stepMatch(1)}
            disabled={searchMatches.length === 0}
            title={t("i18n.findNext")}
            aria-label={t("i18n.findNext")}
            className="file-viewer-icon-button"
          >
            <ChevronDown size={14} strokeWidth={2} aria-hidden="true" />
          </button>
          <button
            type="button"
            onClick={() => {
              setSearchCaseSensitive((value) => !value);
              setCurrentMatchIndex(0);
            }}
            title={t("i18n.findCaseSensitive")}
            aria-label={t("i18n.findCaseSensitive")}
            aria-pressed={searchCaseSensitive}
            className="file-viewer-icon-button"
            style={{
              background: searchCaseSensitive ? "var(--bg-selected)" : "transparent",
              color: searchCaseSensitive ? "var(--text)" : "var(--text-muted)",
            }}
          >
            <CaseSensitive size={15} strokeWidth={2} aria-hidden="true" />
          </button>
          <button
            type="button"
            onClick={closeSearch}
            title={t("i18n.findClose")}
            aria-label={t("i18n.findClose")}
            className="file-viewer-icon-button"
          >
            <X size={14} strokeWidth={2} aria-hidden="true" />
          </button>
        </div>
      )}

      {/* Content area */}
      <div
        ref={contentRef}
        className="file-viewer-content"
        onCopy={copyFileViewerSelectionWithoutGutters}
        onScroll={(event) => {
          viewerStateRef.current.scrollTop = event.currentTarget.scrollTop;
          viewerStateRef.current.scrollLeft = event.currentTarget.scrollLeft;
        }}
        style={{ flex: 1, overflow: "auto", background: "var(--bg)" }}
      >
        {editing && data ? (
          <MonacoFileEditor
            value={draft ?? data.content}
            language={data.language}
            dark={isDark}
            path={filePath}
            onChange={handleEditorChange}
            onSave={(content) => void persistContent(content)}
            onUndoStateChange={setUndoState}
            handleRef={editorHandleRef}
          />
        ) : effectiveDisplayMode === "diff" && hasGitDiff ? (
          <DiffView patch={gitDiff.patch!} />
        ) : isHtml && effectiveDisplayMode === "preview" ? (
          <iframe
            srcDoc={content}
            sandbox="allow-scripts"
            style={{ width: "100%", height: "100%", border: "none", background: "var(--bg)" }}
             title={t("i18n.htmlPreview")}
          />
        ) : isMarkdown && effectiveDisplayMode === "preview" ? (
          <div
            className="markdown-body markdown-file-preview"
            style={{ padding: "24px 32px" }}
          >
            {frontmatter?.data && <FrontmatterCard data={frontmatter.data} />}
            <ReactMarkdown
              remarkPlugins={markdownPreviewRemarkPlugins}
              rehypePlugins={markdownPreviewRehypePlugins}
              components={{
                code({ className, children, ...props }) {
                  const lang = className?.replace("language-", "").toLowerCase() ?? "";
                  const raw = String(children);
                  const isBlock = className?.includes("language-") || raw.includes("\n");
                  if (isBlock) {
                    if (lang === "mermaid") {
                      return <MermaidBlock code={raw.replace(/\n$/, "")} defaultPreview />;
                    }
                    return <CodeBlock code={raw.replace(/\n$/, "")} lang={lang} />;
                  }
                  return (
                    <code className={["markdown-inline-code", className].filter(Boolean).join(" ")} {...props}>
                      {children}
                    </code>
                  );
                },
                pre({ children }) {
                  // Render the code block directly — CodeBlock provides its own wrapping.
                  // For non-mermaid blocks, pass through to default pre rendering.
                  return <>{children}</>;
                },
                a({ href, children, ...props }) {
                  delete props.node;
                  const externalHref = typeof href === "string" && isOpenableExternalUrl(href) ? href : null;
                  const linkedFile = onOpenFile
                    ? resolveLocalFileTarget(href, markdownDirectory, cwd ?? markdownDirectory)
                    : null;
                  if (!linkedFile || !onOpenFile) {
                    // 桌面端点外链否则会把整个 WebView 导航走（应用界面直接丢掉）。
                    return (
                      <a
                        href={href}
                        {...props}
                        onClick={(event) => { if (externalHref) takeOverExternalLink(event, externalHref); }}
                      >
                        {children}
                      </a>
                    );
                  }

                  const handleClick = (event: MouseEvent<HTMLAnchorElement>) => {
                    if (event.defaultPrevented || event.button !== 0) return;
                    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
                    event.preventDefault();
                    onOpenFile(linkedFile.filePath, { line: linkedFile.line, column: linkedFile.column });
                  };

                  return <a href={href} {...props} onClick={handleClick}>{children}</a>;
                },
                img({ src, alt, ...props }) {
                  delete props.node;
                  const imagePath = typeof src === "string"
                    ? resolveLocalFileTarget(src, markdownDirectory, cwd ?? markdownDirectory)?.filePath ?? null
                    : null;
                  const imageSrc = imagePath
                    ? getFileApiUrl(imagePath, "read", sourceSessionId)
                    : src;
                  // Dynamic local paths are served directly by the file API.
                  // eslint-disable-next-line @next/next/no-img-element
                  return <img src={imageSrc} alt={alt ?? ""} loading="lazy" {...props} />;
                },
              }}
            >
              {markdownPreview}
            </ReactMarkdown>
          </div>
        ) : (
          <SyntaxHighlighter
            className={wrapLines ? "file-source-view is-wrapped" : "file-source-view"}
            language={language === "text" ? "plaintext" : language}
            style={isDark ? darkSyntaxTheme : lightSyntaxTheme}
            showLineNumbers
            lineNumberStyle={{
              ...FILE_LINE_NUMBER_STYLE,
            }}
            customStyle={{
              margin: 0,
              padding: 0,
              border: 0,
              backgroundColor: "var(--bg)",
              ...FILE_CODE_STYLE,
              width: wrapLines ? "100%" : "max-content",
              minWidth: "100%",
              minHeight: "100%",
              overflow: "visible",
            }}
            codeTagProps={{
              style: {
                fontFamily: "var(--font-mono)",
                overflowWrap: wrapLines ? "anywhere" : "normal",
              },
            }}
            renderer={(rendererProps) => (
              <SourceCodeRenderer
                {...rendererProps}
                wrapLines={wrapLines}
                flashLine={flashLine}
                lineSearchRanges={searchOpen ? lineSearchRanges : undefined}
                activeSearchIndex={activeMatchIndex}
              />
            )}
            wrapLongLines={wrapLines}
          >
            {content}
          </SyntaxHighlighter>
        )}
      </div>
    </div>
  );
}
