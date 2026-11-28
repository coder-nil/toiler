"use client";

import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type RefObject } from "react";
import { ArrowLeft, GitBranch, GitCompareArrows, PanelLeft } from "lucide-react";
import { useI18n } from "@/hooks/useI18n";
import { useResizablePanel } from "@/hooks/useResizablePanel";
import { readApiJson } from "@/lib/api-json";
import { getFileName } from "@/lib/file-paths";
import { workspaceDiffUrl, type GitDiffSelection } from "@/lib/git-workspace";
import { CHAT_TOP_BAR_HEIGHT, getGitWorkspaceSplitBounds } from "@/lib/panel-layout";
import type { GitConflictEditorHandle } from "./GitConflictEditor";
import { DiffView } from "./FileViewer";
import { GitHistoryPanel } from "./GitHistoryPanel";
import { GitPanel } from "./GitPanel";
import styles from "./GitWorkspace.module.css";

const PANES = ["changes", "diff", "history"] as const;
type Pane = typeof PANES[number];

interface Props {
  cwd: string;
  sessionId: string | null;
  sidebarOpen: boolean;
  refreshKey: number;
  conflictEditorRef: RefObject<GitConflictEditorHandle | null>;
  onToggleSidebar: () => void;
  onClose: () => void;
  onChanged: () => void;
}

export function GitWorkspace({ cwd, sessionId, sidebarOpen, refreshKey, conflictEditorRef, onToggleSidebar, onClose, onChanged }: Props) {
  const { t } = useI18n();
  const [selection, setSelection] = useState<GitDiffSelection | null>(null);
  const [pane, setPane] = useState<Pane>("changes");
  const [refreshTarget, setRefreshTarget] = useState<HTMLDivElement | null>(null);
  const [historyMutation, setHistoryMutation] = useState(0);
  const [conflictTarget, setConflictTarget] = useState<HTMLDivElement | null>(null);
  const [showingConflict, setShowingConflict] = useState(false);
  const [regionHeight, setRegionHeight] = useState(0);
  const diffId = useId();
  const body = useRef<HTMLDivElement>(null);
  const diffHeight = useRef(getGitWorkspaceSplitBounds(680).initial);
  const bounds = getGitWorkspaceSplitBounds(regionHeight || 680);
  const getMaxDiffHeight = useCallback(() => bounds.max, [bounds.max]);
  const getDefaultDiffHeight = useCallback(() => getGitWorkspaceSplitBounds(regionHeight || body.current?.clientHeight || 680).initial, [regionHeight]);
  const sizing = useResizablePanel({
    ariaLabel: t("gitWorkspace.resizeDiff"), cssVariable: "--git-workspace-diff-height",
    defaultWidth: bounds.initial, getDefaultWidth: getDefaultDiffHeight, getMaxWidth: getMaxDiffHeight, minWidth: bounds.min,
    maxWidth: bounds.max, growthDirection: "down", widthRef: diffHeight,
    storageKey: "toiler-git-workspace-diff-height",
  });
  const regions = sizing.panelRef;
  useLayoutEffect(() => {
    const element = regions.current;
    if (!element) return;
    const measure = () => setRegionHeight(element.clientHeight);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [regions]);
  const back = useRef<HTMLButtonElement>(null);
  useEffect(() => { back.current?.focus({ preventScroll: true }); }, []);

  function showConflict() {
    setShowingConflict(true);
    setPane("diff");
  }

  function selectFile(file: GitDiffSelection) {
    setShowingConflict(false);
    setSelection(file);
    setPane("diff");
    const tab = body.current?.querySelector<HTMLButtonElement>('[data-workspace-tab="diff"]');
    if (tab?.getClientRects().length) tab.focus({ preventScroll: true });
  }

  return <section className={styles.workspace} aria-label={t("gitWorkspace.title")} onKeyDown={(event) => {
    if (event.key !== "Escape" || event.defaultPrevented) return;
    event.preventDefault();
    event.stopPropagation();
    onClose();
  }}>
    <header className={`app-chrome app-topbar ${styles.topBar}`} data-sidebar-open={sidebarOpen ? "true" : "false"}
      data-tauri-drag-region="deep" style={{ height: `calc(${CHAT_TOP_BAR_HEIGHT}px + env(safe-area-inset-top))` }}>
      {!sidebarOpen && <button type="button" className="app-toolbar-btn" onClick={onToggleSidebar} aria-label={t("sidebar.show")} title={t("sidebar.show")}>
        <PanelLeft size={16} aria-hidden="true" />
      </button>}
      <GitBranch size={16} aria-hidden="true" />
      <h1>{t("gitWorkspace.title")}</h1>
      <span className={styles.repository} title={cwd}>{getFileName(cwd) || cwd}</span>
      <button ref={back} type="button" className={`app-toolbar-btn ${styles.back}`} onClick={onClose}>
        <ArrowLeft size={15} aria-hidden="true" />{t("gitWorkspace.back")}
      </button>
    </header>
    <div ref={body} className={styles.body} data-pane={pane}>
      <nav className={styles.navigation} aria-label={t("gitWorkspace.title")}>
        {PANES.map((name) => <button type="button" key={name} data-workspace-tab={name} aria-pressed={name === pane} onClick={() => setPane(name)}>
          {t(`gitWorkspace.${name}`)}
        </button>)}
      </nav>
      <div ref={regions} className={styles.regions} data-resizing={sizing.isResizing || undefined}>
        <section className={styles.changes} aria-label={t("gitWorkspace.changes")}>
          <header className={styles.paneBar}><h2>{t("gitWorkspace.changes")}</h2><div ref={setRefreshTarget} /></header>
          <GitPanel cwd={cwd} sessionId={sessionId} refreshTarget={refreshTarget} refreshButtonClassName={styles.iconButton}
            historyMutation={historyMutation} onClose={onClose} onChanged={onChanged} onSelectFile={selectFile} selectedFile={selection}
            conflictTarget={conflictTarget} conflictActive={showingConflict} conflictEditorRef={conflictEditorRef}
            onShowConflict={showConflict} onHideConflict={() => setShowingConflict(false)} />
        </section>
        <div id={diffId} className={styles.diffRegion}>
          <div hidden={showingConflict} className={styles.diffView}><WorkspaceDiff cwd={cwd} selection={selection} refreshKey={refreshKey} /></div>
          {/* 隐藏而不卸载，返回冲突时 Monaco 保留草稿和撤销栈。 */}
          <div ref={setConflictTarget} hidden={!showingConflict} className={styles.diffView} />
        </div>
        <div {...sizing.separatorProps} className={styles.separator} aria-controls={diffId} title={t("gitWorkspace.resizeDiff")} />
        <div className={styles.history}>
          <GitHistoryPanel cwd={cwd} refreshKey={refreshKey} onSelectFile={selectFile} selectedFile={selection}
            onChanged={() => { setHistoryMutation((value) => value + 1); onChanged(); }} />
        </div>
      </div>
    </div>
  </section>;
}

interface DiffResponse { patch: string; oversized: boolean }

function WorkspaceDiff({ cwd, selection, refreshKey }: { cwd: string; selection: GitDiffSelection | null; refreshKey: number }) {
  const { t } = useI18n();
  const [retry, setRetry] = useState(0);
  const [result, setResult] = useState<{ key: string; data?: DiffResponse; error?: string }>();
  const url = selection ? workspaceDiffUrl(cwd, selection) : null;
  const key = url ? `${retry}:${selection?.scope === "commit" ? 0 : refreshKey}:${url}` : null;
  const scroller = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!url || !key) return;
    const controller = new AbortController();
    scroller.current?.scrollTo(0, 0);
    void fetch(url, { cache: "no-store", signal: controller.signal })
      .then((response) => readApiJson<DiffResponse>(response))
      .then((data) => {
        if (!controller.signal.aborted) setResult({ key, data });
      }, (error: unknown) => {
        if (!controller.signal.aborted) setResult({ key, error: error instanceof Error ? error.message : String(error) });
      });
    return () => controller.abort();
  }, [key, url]);

  const current = result?.key === key ? result : undefined;
  const patch = current?.data?.patch;
  return <section className={styles.diff} aria-label={t("gitWorkspace.diff")}>
    <header className={styles.paneBar}>
      <GitCompareArrows size={15} aria-hidden="true" />
      <h2>{t("gitWorkspace.diff")}</h2>
      {selection && <>
        <span className={styles.fileName} title={selection.path}>{selection.scope === "commit" ? selection.path : getFileName(selection.path)}</span>
        <span className={styles.scope} title={selection.scope === "commit" ? selection.hash : undefined}>
          {selection.scope === "commit" ? selection.hash.slice(0, 8) : t(`gitWorkspace.${selection.scope}`)}
        </span>
      </>}
    </header>
    <div ref={scroller} className={styles.diffScroll} tabIndex={0} role="region" aria-label={t("gitWorkspace.diff")}>
      {!selection ? <p className={styles.hint}>{t("gitWorkspace.pickFile")}</p> :
        current?.error ? <div className={styles.hint} role="alert">
          <p>{current.error}</p><button type="button" className={styles.retry} onClick={() => setRetry((value) => value + 1)}>{t("gitHistory.retry")}</button>
        </div> : !current?.data ? <p className={styles.hint} role="status">{t("gitHistory.loading")}</p> :
          current.data.oversized ? <p className={styles.hint}>{t("gitWorkspace.oversized")}</p> :
            !patch ? <p className={styles.hint}>{t("gitWorkspace.noTextDiff")}</p> :
              /^@@ /m.test(patch) ? <DiffView patch={patch} /> : <pre className={styles.patchMeta}>{patch}</pre>}
    </div>
  </section>;
}
