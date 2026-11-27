"use client";

import { useCallback, useEffect, useId, useMemo, useRef, useState, type CSSProperties } from "react";
import { ChevronRight, FileText, GitBranch, GitCommitHorizontal, GitMerge, History, MoreHorizontal, RefreshCw, Search, Tag } from "lucide-react";
import type { GitDiffSelection } from "@/lib/git-workspace";
import { useI18n } from "@/hooks/useI18n";
import { useResizablePanel } from "@/hooks/useResizablePanel";
import { readApiJson } from "@/lib/api-json";
import { GIT_HISTORY_REFS_DEFAULT_WIDTH, GIT_HISTORY_REFS_MAX_WIDTH, GIT_HISTORY_REFS_MIN_WIDTH, getGitHistoryRefsDefaultWidth, getGitHistoryRefsMaxWidth } from "@/lib/panel-layout";
import { historyFilterParams, layoutGitHistory, shortRef, type GitHistoryCriteria, type GitHistoryBranch, type GitHistoryCommit, type GitHistoryPage, type GitHistoryRef, type GitHistoryRefs } from "@/lib/git-history";
import { CommitActionMenu, CommitResetDialog, type CommitActionAnchor, type CommitActionTarget, type GitResetMode } from "./GitHistoryActions";
import { GitRefActions, type RefActionAnchor } from "./GitRefActions";
import styles from "./GitHistoryPanel.module.css";

type Pane = "refs" | "commits" | "details";
const PANES = ["refs", "commits", "details"] as const;
const EMPTY_CRITERIA: GitHistoryCriteria = { commitId: "", subject: "", author: "", fromDate: "", toDate: "" };
const REF_GROUPS = [
  ["refs/heads/", "sidebar.localBranches", GitBranch],
  ["refs/remotes/", "sidebar.remoteBranches", GitBranch],
  ["refs/tags/", "git.tags", Tag],
] as const;

function url(endpoint: string, cwd: string, params: Record<string, string> = {}) {
  return `/api/git/${endpoint}?${new URLSearchParams({ cwd, ...params })}`;
}

async function request<T>(uri: string, signal: AbortSignal) {
  return readApiJson<T>(await fetch(uri, { signal, cache: "no-store" }));
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function useResource<T>(uri: string | null) {
  const [attempt, setAttempt] = useState(0);
  const [result, setResult] = useState<{ uri: string; attempt: number; data?: T; error?: string }>();
  useEffect(() => {
    if (!uri) return;
    const controller = new AbortController();
    void request<T>(uri, controller.signal).then(
      (data) => { if (!controller.signal.aborted) setResult({ uri, attempt, data }); },
      (error: unknown) => { if (!controller.signal.aborted) setResult({ uri, attempt, error: errorMessage(error) }); },
    );
    return () => controller.abort();
  }, [uri, attempt]);
  const current = uri === result?.uri && attempt === result.attempt ? result : undefined;
  return { data: current?.data, error: current?.error, retry: () => setAttempt((value) => value + 1) };
}

function LoadState({ error, retry }: { error?: string; retry: () => void }) {
  const { t } = useI18n();
  if (!error) return <p className={styles.hint} role="status">{t("gitHistory.loading")}</p>;
  return <div className={styles.hint} role="alert">
    <strong>{t("gitHistory.loadError")}</strong>
    <p>{error}</p>
    <button className={styles.secondaryButton} type="button" onClick={retry}>{t("gitHistory.retry")}</button>
  </div>;
}

export function GitHistoryPanel({ cwd, refreshKey, onSelectFile, selectedFile, onChanged }: { cwd: string; refreshKey: number; onSelectFile: (file: GitDiffSelection) => void; selectedFile: GitDiffSelection | null; onChanged: () => void }) {
  const { t } = useI18n();
  const panel = useRef<HTMLElement>(null);
  const refsId = useId();
  const refsWidth = useRef(GIT_HISTORY_REFS_DEFAULT_WIDTH);
  const [panelWidth, setPanelWidth] = useState(0);
  const getDefaultRefsWidth = useCallback(
    () => getGitHistoryRefsDefaultWidth(panelWidth || panel.current?.clientWidth || 0),
    [panelWidth],
  );
  const getMaxRefsWidth = useCallback(
    () => getGitHistoryRefsMaxWidth(panelWidth || panel.current?.clientWidth || 0),
    [panelWidth],
  );
  const refsResizer = useResizablePanel({
    ariaLabel: t("gitHistory.resizeRefs"),
    cssVariable: "--git-history-refs-width",
    defaultWidth: GIT_HISTORY_REFS_DEFAULT_WIDTH,
    getDefaultWidth: getDefaultRefsWidth,
    getMaxWidth: getMaxRefsWidth,
    growthDirection: "right",
    maxWidth: GIT_HISTORY_REFS_MAX_WIDTH,
    minWidth: GIT_HISTORY_REFS_MIN_WIDTH,
    storageKey: "toiler-git-history-refs-width",
    widthRef: refsWidth,
  });
  useEffect(() => {
    const element = panel.current;
    if (!element) return;
    // 打开主侧栏也会改变历史区宽度，此时窗口本身不会触发 resize。
    const observer = new ResizeObserver(([entry]) => setPanelWidth(entry.contentRect.width));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  const searchButton = useRef<HTMLButtonElement>(null);
  const formId = useId();
  const [showFilters, setShowFilters] = useState(false);
  const [draft, setDraft] = useState<GitHistoryCriteria>(EMPTY_CRITERIA);
  const [criteria, setCriteria] = useState<GitHistoryCriteria>(EMPTY_CRITERIA);
  const params = useMemo(() => historyFilterParams(criteria), [criteria]);
  const searchKey = JSON.stringify(params);
  const filtered = searchKey !== "{}";
  const [pane, setPane] = useState<Pane>("commits");
  const [reference, setReference] = useState("HEAD");
  const [filter, setFilter] = useState("");
  const [reload, setReload] = useState(0);
  const [menu, setMenu] = useState<CommitActionAnchor | null>(null);
  const [refMenu, setRefMenu] = useState<RefActionAnchor | null>(null);
  const [resetTarget, setResetTarget] = useState<CommitActionTarget | null>(null);
  const actionTrigger = useRef<HTMLButtonElement | null>(null);
  const actionPending = useRef(false);
  const [actionBusy, setActionBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const version = `${refreshKey}:${reload}`;
  const resource = useResource<GitHistoryRefs>(url("refs", cwd, { reload: version }));
  const { data } = resource;
  const selectedRef = data?.refs.some((item) => item.name === reference) ? reference : "HEAD";
  const checkedOut = data?.refs.find((item) => item.name === data.headBranch);
  const canWrite = Boolean(checkedOut?.commit);

  const restoreActionFocus = useCallback(() => {
    const trigger = actionTrigger.current;
    (trigger?.isConnected ? trigger : searchButton.current)?.focus({ preventScroll: true });
  }, []);

  const closeMenu = useCallback((restoreFocus: boolean) => {
    setMenu(null);
    if (restoreFocus) restoreActionFocus();
  }, [restoreActionFocus]);

  const closeRefMenu = useCallback((restoreFocus: boolean) => {
    setRefMenu(null);
    if (restoreFocus) restoreActionFocus();
  }, [restoreActionFocus]);

  function openRefMenu(anchor: RefActionAnchor) {
    if (actionPending.current) return;
    actionTrigger.current = anchor.trigger;
    setMenu(null);
    setRefMenu(anchor);
  }

  function openMenu(anchor: CommitActionAnchor) {
    if (actionPending.current) return;
    setRefMenu(null);
    actionTrigger.current = anchor.trigger;
    setActionError(null);
    setMenu(anchor);
  }

  function menuTarget(): CommitActionTarget | null {
    if (!menu || !checkedOut?.commit) return null;
    return { commit: menu.commit, branch: checkedOut.name, head: checkedOut.commit };
  }

  async function runAction(target: CommitActionTarget, mode?: GitResetMode) {
    if (actionPending.current) return;
    actionPending.current = true;
    setActionBusy(true);
    setActionError(null);
    try {
      const response = await fetch("/api/git/history-action", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ cwd, hash: target.commit.hash, expectedBranch: target.branch, expectedHead: target.head,
          ...(mode ? { action: "reset", mode } : { action: "cherry_pick" }) }),
      });
      await readApiJson<{ ok: true }>(response);
      setResetTarget(null);
    } catch (error) {
      setActionError(errorMessage(error));
    } finally {
      actionPending.current = false;
      setActionBusy(false);
      // 优选失败也可能留下待处理冲突，必须同步 Git 操作区和工作树状态。
      onChanged();
      requestAnimationFrame(() => {
        if (!panel.current?.isConnected || panel.current.ownerDocument.querySelector("dialog[open]")) return;
        restoreActionFocus();
      });
    }
  }

  function showPane(next: Pane) {
    setPane(next);
    const tab = panel.current?.querySelector<HTMLButtonElement>(`[data-navigation="${next}"]`);
    if (tab?.getClientRects().length) tab.focus({ preventScroll: true });
  }

  const query = filter.trim().toLowerCase();
  const visibleRefs = data?.refs.filter((item) => shortRef(item.name).toLowerCase().includes(query));
  function selectRef(value: string) {
    setReference(value);
    showPane("commits");
  }

  function updateFilter(field: keyof GitHistoryCriteria, value: string) {
    setDraft((previous) => ({ ...previous, [field]: value }));
  }

  function applySearch(next: GitHistoryCriteria) {
    setCriteria(next);
    setShowFilters(false);
    showPane("commits");
    searchButton.current?.focus({ preventScroll: true });
  }

  return (
    <section ref={panel} className={styles.panel} aria-label={t("gitHistory.title")} data-pane={pane}>
      <header className={styles.toolbar}>
        <History size={15} aria-hidden="true" />
        <h2>{t("gitHistory.title")}</h2>
        <button ref={searchButton} type="button" className={styles.queryButton} data-active={filtered}
          aria-expanded={showFilters} aria-controls={formId} onClick={() => setShowFilters((value) => !value)}>
          <Search size={14} aria-hidden="true" />{t(filtered ? "gitHistory.filtersActive" : "gitHistory.search")}
        </button>
        <button type="button" className={styles.iconButton} onClick={() => setReload((value) => value + 1)}
          aria-label={t("gitHistory.refresh")} title={t("gitHistory.refresh")}><RefreshCw size={15} aria-hidden="true" /></button>
      </header>
      <GitRefActions cwd={cwd} data={data} anchor={refMenu} onClose={closeRefMenu} onChanged={onChanged} restoreFocus={restoreActionFocus}
        onSelection={(ref, next) => setReference((value) => next === "HEAD" || value === ref ? next : value)} />
      {!resetTarget && actionError && <p className={styles.actionError} role="alert">{actionError}</p>}
      {!resetTarget && actionBusy && <p className={styles.actionStatus} role="status">{t("gitHistory.cherryPickBusy")}</p>}
      <form id={formId} hidden={!showFilters} className={styles.queryForm} aria-label={t("gitHistory.search")}
        onSubmit={(event) => { event.preventDefault(); applySearch(draft); }}>
        <label>{t("gitHistory.shortHash")}
          <input type="text" autoComplete="off" spellCheck={false} maxLength={8} pattern="[0-9A-Fa-f]{8}" value={draft.commitId}
            onChange={(event) => updateFilter("commitId", event.target.value)} />
        </label>
        <label>{t("gitHistory.subject")}
          <input type="search" value={draft.subject} onChange={(event) => updateFilter("subject", event.target.value)} />
        </label>
        <label>{t("gitHistory.author")}
          <input type="search" value={draft.author} onChange={(event) => updateFilter("author", event.target.value)} />
        </label>
        <label>{t("gitHistory.fromDate")}
          <input type="date" value={draft.fromDate} max={draft.toDate || undefined} onChange={(event) => updateFilter("fromDate", event.target.value)} />
        </label>
        <label>{t("gitHistory.toDate")}
          <input type="date" value={draft.toDate} min={draft.fromDate || undefined} onChange={(event) => updateFilter("toDate", event.target.value)} />
        </label>
        <div className={styles.queryActions}>
          <button type="button" className={styles.secondaryButton} onClick={() => { setDraft(EMPTY_CRITERIA); applySearch(EMPTY_CRITERIA); }}>
            {t("gitHistory.clearSearch")}
          </button>
          <button type="submit" className={`${styles.secondaryButton} ${styles.querySubmit}`}>{t("gitHistory.search")}</button>
        </div>
      </form>
      <nav className={styles.navigation} aria-label={t("gitHistory.title")}>
        {PANES.map((name) => <button type="button" key={name} data-navigation={name} aria-pressed={name === pane} onClick={() => showPane(name)}>
          {t(`gitHistory.${name}`)}
        </button>)}
      </nav>
      <div ref={refsResizer.panelRef} className={data ? styles.columns : undefined}>
        {!data ? <LoadState error={resource.error} retry={resource.retry} /> : <>
          <section id={refsId} className={`${styles.column} ${styles.refs}`} aria-label={t("gitHistory.refs")}>
            <div className={styles.refToolbar}>
              <label className={styles.search}>
                <Search size={14} aria-hidden="true" />
                <input type="search" value={filter} onChange={(event) => setFilter(event.target.value)}
                  placeholder={t("gitHistory.filter")} aria-label={t("gitHistory.filter")} />
              </label>
            </div>
            <div className={styles.refScroll}>
              <button type="button" className={styles.refRow} aria-pressed={selectedRef === "HEAD"} onClick={() => selectRef("HEAD")}>
                <GitCommitHorizontal size={15} aria-hidden="true" />
                <span className={styles.ellipsis}>HEAD</span>
                <span className={`${styles.refSuffix} ${styles.ellipsis}`} title={data.headBranch ? shortRef(data.headBranch) : undefined}>
                  {data.headBranch ? shortRef(data.headBranch) : t("git.detachedHead")}
                </span>
              </button>
              {query && visibleRefs?.length === 0 && <p className={styles.hint}>{t("gitHistory.noMatches")}</p>}
              {REF_GROUPS.map(([prefix, label, Icon]) => {
                const group = visibleRefs?.filter((item) => item.name.startsWith(prefix)) ?? [];
                if (query && group.length === 0) return null;
                return <details className={styles.refGroup} open key={prefix}>
                  <summary><ChevronRight size={13} aria-hidden="true" />{t(label)}<span>{group.length}</span></summary>
                  {group.map((item) => <div key={item.name} className={styles.refItem} data-active={refMenu?.item.name === item.name}>
                    <button type="button" className={styles.refRow} aria-pressed={selectedRef === item.name}
                      onClick={() => selectRef(item.name)} title={item.name} aria-haspopup="menu"
                      onContextMenu={(event) => {
                        event.preventDefault();
                        openRefMenu({ item, trigger: event.currentTarget, x: event.clientX, y: event.clientY });
                      }}
                      onKeyDown={(event) => {
                        if (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
                        event.preventDefault();
                        const { left, bottom } = event.currentTarget.getBoundingClientRect();
                        openRefMenu({ item, trigger: event.currentTarget, x: left, y: bottom });
                      }}>
                      <Icon size={14} aria-hidden="true" />
                      <span className={styles.ellipsis}>{shortRef(item.name)}</span>
                      {item.name === data.headBranch && <span className={styles.current}>HEAD</span>}
                    </button>
                    <button type="button" className={styles.refMore} aria-haspopup="menu" aria-expanded={refMenu?.item.name === item.name}
                      aria-label={t("gitRefs.actions", { branch: shortRef(item.name) })} onClick={(event) => {
                        const { left, bottom } = event.currentTarget.getBoundingClientRect();
                        openRefMenu({ item, trigger: event.currentTarget, x: left, y: bottom });
                      }}>
                      <MoreHorizontal size={14} aria-hidden="true" />
                    </button>
                  </div>)}
                </details>;
              })}
            </div>
          </section>
          <div {...refsResizer.separatorProps} aria-controls={refsId} className={styles.refsDivider}
            data-resizing={refsResizer.isResizing} title={t("layout.resizeHint")} />
          <CommitHistory key={`${version}:${selectedRef}:${searchKey}`} cwd={cwd} reference={selectedRef} params={params} filtered={filtered} refs={data.refs} onOpenDetails={() => showPane("details")} onSelectFile={onSelectFile} selectedFile={selectedFile} onCommitMenu={openMenu} />
        </>}
      </div>
      {menu && <CommitActionMenu anchor={menu} canWrite={canWrite} onClose={closeMenu}
        onCherryPick={() => { const target = menuTarget(); closeMenu(true); if (target) void runAction(target); }}
        onReset={() => { const target = menuTarget(); closeMenu(false); if (target) setResetTarget(target); }} />}
      {resetTarget && <CommitResetDialog target={resetTarget} busy={actionBusy} error={actionError}
        onCancel={() => { setResetTarget(null); setActionError(null); requestAnimationFrame(restoreActionFocus); }}
        onConfirm={(mode) => void runAction(resetTarget, mode)} />}
    </section>
  );
}

function CommitHistory({ cwd, reference, params, filtered, refs, onOpenDetails, onSelectFile, selectedFile, onCommitMenu }: { cwd: string; reference: string; params: Record<string, string>; filtered: boolean; refs: GitHistoryRef[]; onOpenDetails: () => void; onSelectFile: (file: GitDiffSelection) => void; selectedFile: GitDiffSelection | null; onCommitMenu: (anchor: CommitActionAnchor) => void }) {
  const { locale, t } = useI18n();
  const initial = useResource<GitHistoryPage>(url("history", cwd, { ref: reference, ...params }));
  const [pages, setPages] = useState<GitHistoryPage[]>([]);
  const [selection, setSelection] = useState<string | null>(null);
  const [moreBusy, setMoreBusy] = useState(false);
  const [moreError, setMoreError] = useState<string>();
  const continuation = useRef<AbortController | null>(null);
  useEffect(() => () => continuation.current?.abort(), []);

  const commits = useMemo(() => initial.data ? [initial.data, ...pages].flatMap((page) => page.commits) : [], [initial.data, pages]);
  // 筛选会跳过祖先，不能把相邻结果误画成连续的提交图。
  const graph = useMemo(() => filtered ? null : layoutGitHistory(commits), [filtered, commits]);
  const decorations = useMemo(() => {
    const byCommit = new Map<string, GitHistoryRef[]>();
    for (const item of refs) {
      if (!item.commit) continue;
      const current = byCommit.get(item.commit) ?? [];
      current.push(item);
      byCommit.set(item.commit, current);
    }
    return byCommit;
  }, [refs]);
  const selectedHash = selection ?? commits[0]?.hash;
  const next = (pages.at(-1) ?? initial.data)?.nextOffset;
  const date = useMemo(() => new Intl.DateTimeFormat(locale, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }), [locale]);
  const grid = { "--graph-width": `${graph ? graph.width * 13 + 11 : 0}px` } as CSSProperties;

  async function loadMore() {
    const start = initial.data?.start;
    if (!start || next == null || continuation.current) return;
    const controller = new AbortController();
    continuation.current = controller;
    setMoreBusy(true);
    setMoreError(undefined);
    try {
      const page = await request<GitHistoryPage>(url("history", cwd, { start, offset: String(next), ...params }), controller.signal);
      if (!controller.signal.aborted) setPages((loaded) => [...loaded, page]);
    } catch (error) {
      if (!controller.signal.aborted) setMoreError(errorMessage(error));
    } finally {
      continuation.current = null;
      if (!controller.signal.aborted) setMoreBusy(false);
    }
  }

  return <>
    <section className={`${styles.column} ${styles.commits}`} aria-label={t("gitHistory.commits")} style={grid} data-filtered={filtered}>
      <div className={styles.columnBar}>
        <h3>{t("gitHistory.commits")}</h3>
        <span className={`${styles.scope} ${styles.ellipsis}`} title={reference}>{shortRef(reference)}</span>
      </div>
      <div className={styles.scroll}>
        <div className={`${styles.commitGrid} ${styles.commitHead}`} aria-hidden="true">
          {graph && <span />}<span>{t("gitHistory.hash")}</span><span>{t("gitHistory.subject")}</span><span>{t("gitHistory.author")}</span><span>{t("gitHistory.time")}</span>
        </div>
        {!initial.data && <LoadState error={initial.error} retry={initial.retry} />}
        {initial.data && commits.length === 0 && <p className={styles.hint} role="status">{t(filtered ? "gitHistory.noSearchResults" : "gitHistory.noCommits")}</p>}
        <ul className={styles.list}>
          {commits.map((commit, index) => {
            const merge = commit.parents.length > 1;
            const labels = decorations.get(commit.hash);
            const labelTitle = labels?.map((item) => shortRef(item.name)).join(", ");
            return <li key={commit.hash}>
              <button type="button" className={styles.commitGrid} data-merge={merge || undefined} aria-pressed={selectedHash === commit.hash} aria-haspopup="menu"
                onContextMenu={(event) => {
                  event.preventDefault();
                  onCommitMenu({ commit, trigger: event.currentTarget, x: event.clientX, y: event.clientY });
                }}
                onKeyDown={(event) => {
                  if (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
                  event.preventDefault();
                  const { left, bottom } = event.currentTarget.getBoundingClientRect();
                  onCommitMenu({ commit, trigger: event.currentTarget, x: left, y: bottom });
                }} onClick={() => {
                  setSelection(commit.hash);
                  onOpenDetails();
                }}>
                {graph && <CommitGraph row={graph.rows[index]} width={graph.width} merge={merge} />}
                <code className={styles.commitShortId} title={commit.hash}>{commit.hash.slice(0, 8)}</code>
                <span className={styles.subject}>
                  <span className={styles.ellipsis} title={`${commit.subject} (${commit.hash.slice(0, 8)})`}>{commit.subject || t("gitHistory.noSubject")}</span>
                  {labels && <span className={styles.decoration} title={labelTitle}>
                    {labels[0].name.startsWith("refs/tags/") ? <Tag size={11} aria-hidden="true" /> : <GitBranch size={11} aria-hidden="true" />}
                    <span className={styles.ellipsis}>{shortRef(labels[0].name)}</span>
                    {labels.length > 1 && <span>+{labels.length - 1}</span>}
                  </span>}
                </span>
                <span className={`${styles.ellipsis} ${styles.author}`} data-own={commit.isOwnAuthor || undefined} title={commit.author}>{commit.author}</span>
                <time className={styles.time} dateTime={commit.authoredAt} title={commit.authoredAt}>{formatDate(date, commit.authoredAt)}</time>
              </button>
            </li>;
          })}
        </ul>
        {moreError && <p className={styles.hint} role="alert">{moreError}</p>}
        {next != null && <div className={styles.pageControls}>
          <button type="button" className={styles.secondaryButton} disabled={moreBusy} onClick={() => void loadMore()}>
            {t(moreBusy ? "gitHistory.loading" : moreError ? "gitHistory.retry" : "gitHistory.loadMore")}
          </button>
        </div>}
      </div>
    </section>
    <CommitDetails key={selectedHash ?? "empty"} cwd={cwd} hash={selectedHash} onSelectFile={onSelectFile} selectedFile={selectedFile} />
  </>;
}

function formatDate(formatter: Intl.DateTimeFormat, value: string) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : formatter.format(date);
}

function CommitGraph({ row, width, merge }: { row: ReturnType<typeof layoutGitHistory>["rows"][number]; width: number; merge: boolean }) {
  const x = (lane: number) => 12 + lane * 13;
  const colour = (lane: number) => `var(--lane-${lane % 6})`;
  const line = (lane: number, y1: number, y2: number) =>
    <path key={lane} stroke={colour(lane)} d={`M${x(lane)} ${y1}V${y2}`} />;
  return <svg className={styles.graph} viewBox={`0 0 ${width * 13 + 11} 30`} preserveAspectRatio="none" aria-hidden="true">
    <g fill="none" strokeWidth="1.4">
      {row.through.map((lane) => line(lane, 0, 30))}
      {row.enters && line(row.lane, 0, 15)}
      {row.parents.map((lane) => <path key={lane} stroke={colour(lane)} d={`M${x(row.lane)} 15 C${x(row.lane)} 23 ${x(lane)} 21 ${x(lane)} 30`} />)}
    </g>
    <circle cx={x(row.lane)} cy="15" r={merge ? 4 : 3} fill={merge ? "var(--history-row)" : colour(row.lane)} stroke={colour(row.lane)} strokeWidth="1.45" />
  </svg>;
}

function CommitDetails({ cwd, hash, onSelectFile, selectedFile }: { cwd: string; hash?: string; onSelectFile: (file: GitDiffSelection) => void; selectedFile: GitDiffSelection | null }) {
  const { locale, t } = useI18n();
  const detail = useResource<GitHistoryCommit>(hash ? url("commit", cwd, { hash }) : null);
  const commit = detail.data;
  const date = useMemo(() => new Intl.DateTimeFormat(locale, { dateStyle: "medium", timeStyle: "long" }), [locale]);
  return <section className={`${styles.column} ${styles.details}`} aria-label={t("gitHistory.details")}>
    <div className={styles.columnBar}>
      <h3>{t("gitHistory.files")}</h3>
      {commit && <span className={styles.count}>{commit.files.length}</span>}
    </div>
    {!hash && <p className={styles.hint}>{t("gitHistory.selectCommit")}</p>}
    {hash && !commit && <LoadState error={detail.error} retry={detail.retry} />}
    {commit && <>
      <div className={styles.fileScroll}>
        {commit.parents.length > 1 && <p className={styles.mergeHint}><GitMerge size={13} aria-hidden="true" />{t("gitHistory.firstParent")}</p>}
        {commit.files.length === 0 && <p className={styles.hint}>{t("gitHistory.noFiles")}</p>}
        <ul className={styles.list}>
          {commit.files.map((file) => <li key={file.path}>
            <button type="button" className={styles.fileRow} title={file.oldPath ? `${file.oldPath} → ${file.path}` : file.path}
              aria-pressed={selectedFile?.scope === "commit" && selectedFile.hash === commit.hash && selectedFile.path === file.path}
              onClick={() => onSelectFile({ scope: "commit", hash: commit.hash, path: file.path, oldPath: file.oldPath })}>
              <span className={styles.fileStatus} data-status={file.status[0]}>{file.status[0]}</span>
              <FileText size={14} aria-hidden="true" />
              <span className={styles.filePath}>{file.path}{file.oldPath && <small>{t("gitHistory.oldPath", { path: file.oldPath })}</small>}</span>
            </button>
          </li>)}
        </ul>
      </div>
      <div className={styles.detailBody}>
        <h3>{t("gitHistory.details")}</h3>
        <dl className={`${styles.metadata} ${styles.commitId}`}>
          <dt>{t("gitHistory.hash")}</dt><dd><code>{commit.hash}</code></dd>
        </dl>
        <div className={styles.message}>{commit.message || t("gitHistory.noSubject")}</div>
        <dl className={styles.metadata}>
          <dt>{t("gitHistory.author")}</dt><dd>{commit.author}{commit.authorEmail && <span className={styles.email}>{`<${commit.authorEmail}>`}</span>}</dd>
          <dt>{t("gitHistory.time")}</dt><dd><time dateTime={commit.authoredAt}>{formatDate(date, commit.authoredAt)}</time></dd>
          {commit.parents.length > 0 && <><dt>{t("gitHistory.parents")}</dt><dd className={styles.parents}>
            {commit.parents.map((parent) => <code key={parent} title={parent}>{parent.slice(0, 8)}</code>)}
          </dd></>}
        </dl>
        <ContainingBranches branches={commit.containingBranches} />
      </div>
    </>}
  </section>;
}

function ContainingBranches({ branches }: { branches: GitHistoryBranch[] }) {
  const { t } = useI18n();
  return <div className={styles.containingBranches}>
    <h4>{t("gitHistory.containingBranches")}</h4>
    {branches.length === 0 && <p className={styles.branchEmpty}>{t("gitHistory.noContainingBranches")}</p>}
    {REF_GROUPS.slice(0, 2).map(([prefix, label]) => {
      const group = branches.filter((branch) => branch.name.startsWith(prefix));
      if (!group.length) return null;
      return <div key={prefix} className={styles.branchGroup}>
        <h5>{t(label)} <span className={styles.count}>{group.length}</span></h5>
        <ul className={styles.branchList}>
          {group.map((branch) => <li key={branch.name} className={styles.branchLabel} title={branch.name}>
            <GitBranch size={13} aria-hidden="true" />
            <code>{shortRef(branch.name)}</code>
            {branch.isTip && <span className={styles.branchTip}>{t("gitHistory.branchTip")}</span>}
          </li>)}
        </ul>
      </div>;
    })}
  </div>;
}
