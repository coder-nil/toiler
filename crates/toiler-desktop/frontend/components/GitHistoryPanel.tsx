"use client";

import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import { ChevronRight, FileText, GitBranch, GitCommitHorizontal, GitMerge, History, RefreshCw, Search, Tag } from "lucide-react";
import type { GitDiffSelection } from "@/lib/git-workspace";
import { useI18n } from "@/hooks/useI18n";
import { readApiJson } from "@/lib/api-json";
import { layoutGitHistory, shortRef, type GitHistoryBranch, type GitHistoryCommit, type GitHistoryPage, type GitHistoryRef, type GitHistoryRefs } from "@/lib/git-history";
import styles from "./GitHistoryPanel.module.css";

type Pane = "refs" | "commits" | "details";
const PANES = ["refs", "commits", "details"] as const;
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

export function GitHistoryPanel({ cwd, refreshKey, onSelectFile, selectedFile }: { cwd: string; refreshKey: number; onSelectFile: (file: GitDiffSelection) => void; selectedFile: GitDiffSelection | null }) {
  const { t } = useI18n();
  const panel = useRef<HTMLElement>(null);
  const [pane, setPane] = useState<Pane>("commits");
  const [reference, setReference] = useState("HEAD");
  const [filter, setFilter] = useState("");
  const [reload, setReload] = useState(0);
  const version = `${refreshKey}:${reload}`;
  const resource = useResource<GitHistoryRefs>(url("refs", cwd, { reload: version }));
  const { data } = resource;
  const selectedRef = data?.refs.some((item) => item.name === reference) ? reference : "HEAD";

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

  return (
    <section ref={panel} className={styles.panel} aria-label={t("gitHistory.title")} data-pane={pane}>
      <header className={styles.toolbar}>
        <History size={15} aria-hidden="true" />
        <h2>{t("gitHistory.title")}</h2>
        <button type="button" className={styles.iconButton} onClick={() => setReload((value) => value + 1)}
          aria-label={t("gitHistory.refresh")} title={t("gitHistory.refresh")}><RefreshCw size={15} aria-hidden="true" /></button>
      </header>
      <nav className={styles.navigation} aria-label={t("gitHistory.title")}>
        {PANES.map((name) => <button type="button" key={name} data-navigation={name} aria-pressed={name === pane} onClick={() => showPane(name)}>
          {t(`gitHistory.${name}`)}
        </button>)}
      </nav>
      {!data ? <LoadState error={resource.error} retry={resource.retry} /> :
        <div className={styles.columns}>
          <section className={`${styles.column} ${styles.refs}`} aria-label={t("gitHistory.refs")}>
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
                  {group.map((item) => <button type="button" key={item.name} className={styles.refRow}
                    aria-pressed={selectedRef === item.name} onClick={() => selectRef(item.name)} title={item.name}>
                    <Icon size={14} aria-hidden="true" />
                    <span className={styles.ellipsis}>{shortRef(item.name)}</span>
                    {item.name === data.headBranch && <span className={styles.current}>HEAD</span>}
                  </button>)}
                </details>;
              })}
            </div>
          </section>
          <CommitHistory key={`${version}:${selectedRef}`} cwd={cwd} reference={selectedRef} refs={data.refs} onOpenDetails={() => showPane("details")} onSelectFile={onSelectFile} selectedFile={selectedFile} />
        </div>}
    </section>
  );
}

function CommitHistory({ cwd, reference, refs, onOpenDetails, onSelectFile, selectedFile }: { cwd: string; reference: string; refs: GitHistoryRef[]; onOpenDetails: () => void; onSelectFile: (file: GitDiffSelection) => void; selectedFile: GitDiffSelection | null }) {
  const { locale, t } = useI18n();
  const initial = useResource<GitHistoryPage>(url("history", cwd, { ref: reference }));
  const [pages, setPages] = useState<GitHistoryPage[]>([]);
  const [selection, setSelection] = useState<string | null>(null);
  const [moreBusy, setMoreBusy] = useState(false);
  const [moreError, setMoreError] = useState<string>();
  const continuation = useRef<AbortController | null>(null);
  useEffect(() => () => continuation.current?.abort(), []);

  const commits = useMemo(() => initial.data ? [initial.data, ...pages].flatMap((page) => page.commits) : [], [initial.data, pages]);
  const graph = useMemo(() => layoutGitHistory(commits), [commits]);
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
  const grid = { "--graph-width": `${graph.width * 13 + 11}px` } as CSSProperties;

  async function loadMore() {
    const start = initial.data?.start;
    if (!start || next == null || continuation.current) return;
    const controller = new AbortController();
    continuation.current = controller;
    setMoreBusy(true);
    setMoreError(undefined);
    try {
      const page = await request<GitHistoryPage>(url("history", cwd, { start, offset: String(next) }), controller.signal);
      if (!controller.signal.aborted) setPages((loaded) => [...loaded, page]);
    } catch (error) {
      if (!controller.signal.aborted) setMoreError(errorMessage(error));
    } finally {
      continuation.current = null;
      if (!controller.signal.aborted) setMoreBusy(false);
    }
  }

  return <>
    <section className={`${styles.column} ${styles.commits}`} aria-label={t("gitHistory.commits")} style={grid}>
      <div className={styles.columnBar}>
        <h3>{t("gitHistory.commits")}</h3>
        <span className={`${styles.scope} ${styles.ellipsis}`} title={reference}>{shortRef(reference)}</span>
      </div>
      <div className={styles.scroll}>
        <div className={`${styles.commitGrid} ${styles.commitHead}`} aria-hidden="true">
          <span /><span>{t("gitHistory.subject")}</span><span>{t("gitHistory.author")}</span><span>{t("gitHistory.time")}</span>
        </div>
        {!initial.data && <LoadState error={initial.error} retry={initial.retry} />}
        {initial.data && commits.length === 0 && <p className={styles.hint}>{t("gitHistory.noCommits")}</p>}
        <ul className={styles.list}>
          {commits.map((commit, index) => {
            const labels = decorations.get(commit.hash);
            const labelTitle = labels?.map((item) => shortRef(item.name)).join(", ");
            return <li key={commit.hash}>
              <button type="button" className={styles.commitGrid} aria-pressed={selectedHash === commit.hash} onClick={() => {
                setSelection(commit.hash);
                onOpenDetails();
              }}>
                <CommitGraph row={graph.rows[index]} width={graph.width} merge={commit.parents.length > 1} />
                <span className={styles.subject}>
                  <span className={styles.ellipsis} title={`${commit.subject} (${commit.hash.slice(0, 8)})`}>{commit.subject || t("gitHistory.noSubject")}</span>
                  {labels && <span className={styles.decoration} title={labelTitle}>
                    {labels[0].name.startsWith("refs/tags/") ? <Tag size={11} aria-hidden="true" /> : <GitBranch size={11} aria-hidden="true" />}
                    <span className={styles.ellipsis}>{shortRef(labels[0].name)}</span>
                    {labels.length > 1 && <span>+{labels.length - 1}</span>}
                  </span>}
                </span>
                <span className={`${styles.ellipsis} ${styles.author}`} title={commit.author}>{commit.author}</span>
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
