"use client";

import { useEffect, useImperativeHandle, useLayoutEffect, useMemo, useRef, useState, type Ref } from "react";
import { ChevronLeft, ChevronRight, LockKeyhole, X } from "lucide-react";
import type * as Monaco from "monaco-editor";
import { useI18n } from "@/hooks/useI18n";
import { useTheme } from "@/hooks/useTheme";
import { listenForNativeEditorHistory, type EditorHistoryAction } from "@/lib/editor-history";
import { alignMergeRows, changedMergeColumns, mergeBlockText, normalizeMergeText, parseMergeDraft, type MergeSide } from "@/lib/git-conflicts";
import { simpleMergeEdits, type SimpleMergePlans } from "@/lib/git-simple-merge";
import styles from "./GitConflictEditor.module.css";

const LINE_HEIGHT = 22;
const panes = ["local", "result", "incoming"] as const;
const sides = ["local", "incoming"] as const;
const otherSide = (side: MergeSide): MergeSide => side === "local" ? "incoming" : "local";

type Editor = Monaco.editor.IStandaloneCodeEditor;
type Editors = [Editor, Editor, Editor];
export type MergeHighlight = "words" | "lines" | "off";
export interface MergeEditorHandle {
  reveal: (line: number) => void;
  accept: (side: MergeSide, line?: number) => void;
  mergeSimple: (plans: SimpleMergePlans) => void;
}
interface Props {
  path: string;
  source: { local: string | null; incoming: string | null; result: string | null; markerSize: number };
  labels: [string, string, string];
  text: string;
  parsed: ReturnType<typeof parseMergeDraft>;
  highlight: MergeHighlight;
  busy: boolean;
  remove: boolean;
  handleRef: Ref<MergeEditorHandle>;
  onChange: (text: string) => void;
  onReady: () => void;
}

// Monaco 0.57 默认从 blob 加载 Webpack 导出的相对地址，浏览器无法解析。使用
// Webpack 的 Worker 入口打包实际依赖，保持静态导出可离线运行。
function configureMergeWorkers() {
  globalThis.MonacoEnvironment ??= {
    getWorker(_id, label) {
      switch (label) {
        case "json": return new Worker(new URL("monaco-editor/language/json/json.worker.js", import.meta.url), { type: "module" });
        case "css": case "scss": case "less": return new Worker(new URL("monaco-editor/language/css/css.worker.js", import.meta.url), { type: "module" });
        case "html": case "handlebars": case "razor": return new Worker(new URL("monaco-editor/language/html/html.worker.js", import.meta.url), { type: "module" });
        case "typescript": case "javascript": return new Worker(new URL("monaco-editor/language/typescript/ts.worker.js", import.meta.url), { type: "module" });
        default: return new Worker(new URL("monaco-editor/editor/editor.worker.js", import.meta.url), { type: "module" });
      }
    },
  };
}

function languageForPath(api: typeof Monaco, path: string) {
  const name = path.split("/").pop()?.toLowerCase() ?? "";
  if (name === ".env" || name.startsWith(".env.")) {
    const id = "toiler-dotenv";
    if (!api.languages.getLanguages().some((language) => language.id === id)) {
      api.languages.register({ id });
      api.languages.setMonarchTokensProvider(id, { tokenizer: { root: [
        [/^\s*#.*$/, "comment"], [/^(\s*(?:export\s+)?[\w.-]+)(\s*=)(.*)$/, ["keyword", "delimiter", "string"]],
      ] } });
    }
    return id;
  }
  return api.languages.getLanguages().find((language) => language.filenames?.some((file) => file.toLowerCase() === name))?.id ??
    api.languages.getLanguages().find((language) => language.extensions?.some((extension) => name.endsWith(extension)))?.id ?? "plaintext";
}

export function GitMergeEditors({ path, source, labels, text, parsed, highlight, busy, remove, handleRef, onChange, onReady }: Props) {
  const { t } = useI18n();
  const { isDark } = useTheme();
  const hosts = useRef<(HTMLDivElement | null)[]>([]);
  const tracks = useRef<(HTMLDivElement | null)[]>([]);
  const zones = useRef<string[][]>([[], [], []]);
  const callbacks = useRef({ onChange, onReady });
  callbacks.current = { onChange, onReady };
  const [editors, setEditors] = useState<Editors>();
  const [failed, setFailed] = useState(false);
  const local = useMemo(() => normalizeMergeText(source.local ?? ""), [source.local]);
  const incoming = useMemo(() => normalizeMergeText(source.incoming ?? ""), [source.incoming]);
  const draftLines = useMemo(() => text.split("\n"), [text]);
  const localLines = useMemo(() => local.split("\n"), [local]);
  const incomingLines = useMemo(() => incoming.split("\n"), [incoming]);
  const alignment = useMemo(() => alignMergeRows(draftLines, localLines, incomingLines, parsed.blocks), [draftLines, localLines, incomingLines, parsed.blocks]);
  const decorations = useRef<Monaco.editor.IEditorDecorationsCollection[]>([]);
  const monaco = useRef<typeof Monaco | null>(null);
  const theme = isDark ? "vs-dark" : "vs";
  const liveOptions = useRef({ labels, theme, busy, remove });
  liveOptions.current = { labels, theme, busy, remove };
  const relayout = useRef(false);

  useEffect(() => {
    let cancelled = false;
    let mounted: Editors | undefined;
    const disposables: Monaco.IDisposable[] = [];
    configureMergeWorkers();
    void import("monaco-editor").then((api) => {
      if (cancelled) return;
      const rootStyle = getComputedStyle(document.documentElement);
      const { labels, theme, busy, remove } = liveOptions.current;
      const language = languageForPath(api, path);
      monaco.current = api;
      const options: Monaco.editor.IStandaloneEditorConstructionOptions = {
        theme, automaticLayout: true, fontFamily: rootStyle.getPropertyValue("--font-mono"),
        fontSize: parseInt(rootStyle.getPropertyValue("--code-font-size"), 10) || 13, lineHeight: LINE_HEIGHT,
        minimap: { enabled: false }, folding: false, glyphMargin: false, lineNumbersMinChars: 3,
        lineDecorationsWidth: 8, stickyScroll: { enabled: false }, scrollBeyondLastLine: false,
        renderLineHighlight: "none", overviewRulerLanes: 0, wordWrap: "off", smoothScrolling: false,
        renderValidationDecorations: "off", matchBrackets: "never", quickSuggestions: false,
        parameterHints: { enabled: false }, contextmenu: false, unicodeHighlight: { ambiguousCharacters: false },
        scrollbar: { verticalScrollbarSize: 9, horizontalScrollbarSize: 9, alwaysConsumeMouseWheel: false },
      };
      const created = panes.map((pane, index) => {
        const value = pane === "local" ? local : pane === "incoming" ? incoming : normalizeMergeText(source.result ?? "");
        const model = api.editor.createModel(value, language);
        model.setEOL(api.editor.EndOfLineSequence.LF);
        disposables.push(model);
        const readOnly = pane !== "result" || busy || remove;
        const editor = api.editor.create(hosts.current[index]!, { ...options, model, readOnly, domReadOnly: readOnly, ariaLabel: labels[index] });
        disposables.push(editor);
        return editor;
      }) as Editors;
      mounted = created;
      zones.current = [[], [], []];
      decorations.current = created.map((editor) => editor.createDecorationsCollection());
      let syncing = false;
      for (const editor of created) disposables.push(editor.onDidScrollChange((event) => {
        if (!event.scrollTopChanged || relayout.current || syncing) return;
        syncing = true;
        for (const sibling of created) if (sibling !== editor) sibling.setScrollTop(event.scrollTop);
        for (const track of tracks.current) if (track) track.style.transform = `translateY(${-event.scrollTop}px)`;
        syncing = false;
      }));
      const result = created[1];
      disposables.push(result.onDidChangeModelContent(() => callbacks.current.onChange(result.getValue({ preserveBOM: true, lineEnding: "\n" }))));
      const isEditable = () => !liveOptions.current.busy && !liveOptions.current.remove;
      const panel = result.getDomNode()?.closest("[data-git-conflict-editor]");
      const ownsFocus = (target: EventTarget | null) => target instanceof Node && panel?.contains(target) && panel.getClientRects().length > 0;
      const performHistory = (action: EditorHistoryAction) => {
        if (!isEditable()) return false;
        const model = result.getModel();
        if (!model) return false;
        if (action === "undo") model.undo();
        else model.redo();
        return true;
      };
      const handleUndoRedo = (event: KeyboardEvent) => {
        if (event.defaultPrevented || event.isComposing || event.altKey || !(event.metaKey || event.ctrlKey)) return;
        const key = event.key.toLowerCase();
        if (key !== "z" && !(key === "y" && event.ctrlKey && !event.metaKey && !event.shiftKey)) return;
        if (!ownsFocus(event.target)) return;
        if (!performHistory(key === "z" && !event.shiftKey ? "undo" : "redo")) return;
        event.preventDefault();
        event.stopPropagation();
      };
      const handlePaste = (event: Event) => {
        if (isEditable() && event.target instanceof Node && result.getDomNode()?.contains(event.target)) result.pushUndoStop();
      };
      document.addEventListener("keydown", handleUndoRedo, true);
      document.addEventListener("paste", handlePaste, true);
      const removeNativeHistoryListener = listenForNativeEditorHistory((action) => Boolean(ownsFocus(document.activeElement)) && performHistory(action));
      disposables.push({ dispose() {
        document.removeEventListener("keydown", handleUndoRedo, true);
        document.removeEventListener("paste", handlePaste, true);
        removeNativeHistoryListener();
      } });
      setEditors(created);
      callbacks.current.onReady();
    }).catch(() => { if (!cancelled) setFailed(true); });
    return () => {
      cancelled = true;
      for (const editor of mounted ?? []) editor.setModel(null);
      for (const disposable of disposables.reverse()) disposable.dispose();
    };
  }, [path, source, local, incoming]);

  useLayoutEffect(() => {
    if (!editors) return;
    monaco.current?.editor.setTheme(theme);
    editors.forEach((editor, index) => {
      const readOnly = index !== 1 || busy || remove;
      editor.updateOptions({ readOnly, domReadOnly: readOnly, ariaLabel: labels[index] });
    });
  }, [editors, theme, labels, busy, remove]);

  const highlightRows = useMemo(() => {
    const result: Monaco.editor.IModelDeltaDecoration[][] = [[], [], []];
    if (highlight === "off") return result;
    const blockStarts = new Set(parsed.blocks.map((block) => block.startLine));
    const endLines = new Set(parsed.blocks.map((block) => block.endLine));
    const markerLines = new Set(parsed.markers.map((marker) => marker.line));
    const lines = [localLines, draftLines, incomingLines];
    let conflicted = false;
    for (const [left, middle, right] of alignment?.rows ?? draftLines.map((_, i) => [null, i + 1, null] as const)) {
      if (middle !== null && blockStarts.has(middle)) conflicted = true;
      const row = [left, middle, right];
      const marker = middle !== null && markerLines.has(middle);
      const values = row.map((line, index) => line === null ? undefined : lines[index][line - 1]);
      // 正好在结束标记后的侧栏占位不应染色；中栏下一条真实行才退出冲突区域。
      const changed = conflicted || marker || (!!alignment && (values[0] !== values[1] || values[1] !== values[2]));
      for (const [index, line] of row.entries()) {
        if (line === null || !changed) continue;
        result[index].push({ range: { startLineNumber: line, endLineNumber: line, startColumn: 1, endColumn: 1 },
          options: { isWholeLine: true, className: conflicted || marker ? styles.conflictLine : styles.changedLine } });
        const value = values[index];
        const comparison = conflicted ? values[2 - index] : index === 1 ? values[0] ?? values[2] : values[1];
        if (highlight === "words" && !(conflicted && index === 1) && value !== undefined && comparison !== undefined && value !== comparison) {
          const [startColumn, endColumn] = changedMergeColumns(value, comparison);
          if (startColumn < endColumn) result[index].push({ range: { startLineNumber: line, endLineNumber: line, startColumn, endColumn }, options: { inlineClassName: styles.changedWord } });
        }
      }
      if (middle !== null && endLines.has(middle)) conflicted = false;
    }
    return result;
  }, [alignment, draftLines, localLines, incomingLines, parsed, highlight]);

  useLayoutEffect(() => {
    if (!editors) return;
    const top = editors[1].getScrollTop();
    relayout.current = true;
    editors.forEach((editor, index) => {
      editor.changeViewZones((accessor) => {
        for (const id of zones.current[index]) accessor.removeZone(id);
        const next: string[] = [];
        let afterLineNumber = 0, heightInLines = 0;
        function flush() {
          if (!heightInLines) return;
          const domNode = document.createElement("div");
          domNode.className = styles.lineGap;
          next.push(accessor.addZone({ afterLineNumber, heightInLines, domNode }));
          heightInLines = 0;
        }
        for (const row of alignment?.rows ?? []) {
          const line = row[index];
          if (line === null) heightInLines++;
          else { flush(); afterLineNumber = line; }
        }
        flush();
        zones.current[index] = next;
      });
    });
    editors.forEach((editor) => editor.setScrollTop(top));
    for (const track of tracks.current) if (track) track.style.transform = `translateY(${-editors[1].getScrollTop()}px)`;
    relayout.current = false;
  }, [alignment, editors]);

  useLayoutEffect(() => {
    if (editors) highlightRows.forEach((rows, index) => decorations.current[index].set(rows));
  }, [editors, highlightRows]);

  function reveal(line: number) { editors?.[1].revealLineInCenter(line); }

  function accept(side: MergeSide, line?: number) {
    if (!editors || busy || remove || source[side] === null) return;
    const editor = editors[1];
    const model = editor.getModel();
    if (!model) return;
    const draft = model.getValue();
    const blocks = parseMergeDraft(draft, source.markerSize).blocks.filter((block) => line === undefined || block.startLine === line);
    applyEdits(editor, model, blocks.map((block) => ({ from: block.from, to: block.to,
      text: mergeBlockText(block, side, draft, side === "local" ? local : incoming) })), line);
  }

  function mergeSimple(plans: SimpleMergePlans) {
    if (!editors || busy || remove) return;
    const editor = editors[1];
    const model = editor.getModel();
    if (!model) return;
    const draft = model.getValue();
    applyEdits(editor, model, simpleMergeEdits(draft, parseMergeDraft(draft, source.markerSize).blocks, plans));
  }

  function applyEdits(editor: Editor, model: Monaco.editor.ITextModel, edits: { from: number; to: number; text: string }[], line?: number) {
    if (!edits.length) return;
    editor.pushUndoStop();
    editor.executeEdits("toiler-merge", edits.map(({ from, to, text }) => {
      const start = model.getPositionAt(from), end = model.getPositionAt(to);
      return { range: { startLineNumber: start.lineNumber, startColumn: start.column, endLineNumber: end.lineNumber, endColumn: end.column }, text };
    }));
    editor.pushUndoStop();
    if (line !== undefined) editor.setPosition({ lineNumber: Math.min(line, model.getLineCount()), column: 1 });
    editor.focus();
  }

  useImperativeHandle(handleRef, () => ({ reveal, accept, mergeSimple }));

  return <div className={styles.editorGrid}>
    {panes.map((pane, index) => <section className={styles.column} key={pane} data-pane={pane}>
      <h3 title={labels[index]}>{pane !== "result" && <LockKeyhole size={13} aria-hidden="true" />}<span>{labels[index]}</span></h3>
      <div className={styles.codeHost} ref={(node) => { hosts.current[index] = node; }} />
      {pane !== "result" && source[pane] === null && <p className={styles.paneMessage}>{t("gitConflict.missing")}</p>}
      {pane === "result" && (remove || !editors) && <p className={styles.paneMessage} role={failed ? "alert" : "status"}>
        {failed ? t("gitConflict.editorFailed") : remove ? t("gitConflict.deleteHint") : t("gitHistory.loading")}
      </p>}
    </section>)}
    {sides.map((side, index) => <div key={side} className={styles.rail} data-side={side}>
      <div className={styles.track} ref={(node) => { tracks.current[index] = node; }}>
        {editors && !remove && source[side] !== null && parsed.blocks.map((block) => {
          const acceptLabel = t(side === "local" ? "gitConflict.acceptLocal" : "gitConflict.acceptIncoming");
          const skipLabel = t(side === "local" ? "gitConflict.skipLocal" : "gitConflict.skipIncoming");
          const Arrow = side === "local" ? ChevronRight : ChevronLeft;
          return <div key={block.startLine} className={styles.gutterActions} style={{ top: (alignment?.conflictRows.get(block.startLine) ?? block.startLine - 1) * LINE_HEIGHT }}
            onFocus={(event) => { if (event.target.matches(":focus-visible")) reveal(block.startLine); }}>
            {source[otherSide(side)] !== null && <button type="button" disabled={busy} title={skipLabel} aria-label={skipLabel} onClick={() => accept(otherSide(side), block.startLine)}><X size={14} aria-hidden="true" /></button>}
            <button type="button" disabled={busy} title={acceptLabel} aria-label={acceptLabel} onClick={() => accept(side, block.startLine)}><Arrow size={17} aria-hidden="true" /></button>
          </div>;
        })}
      </div>
    </div>)}
  </div>;
}
