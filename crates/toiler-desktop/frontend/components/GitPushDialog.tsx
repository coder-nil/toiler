"use client";

import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { ArrowRight, FileText } from "lucide-react";
import { useI18n } from "@/hooks/useI18n";
import { readApiJson } from "@/lib/api-json";
import { getFileName } from "@/lib/file-paths";
import type { GitHistoryCommit, GitHistorySummary } from "@/lib/git-history";
import styles from "./GitPushDialog.module.css";

export interface GitPushPreview {
  branch: string;
  remote: string;
  targetBranch: string;
  token: string;
  hasTrackingRef: boolean;
  usesPanelCredential: boolean;
  commits: GitHistorySummary[];
  nextOffset: number | null;
}

function load<T>(endpoint: string, cwd: string, params: Record<string, string>, signal: AbortSignal) {
  return fetch(`/api/git/${endpoint}?${new URLSearchParams({ cwd, ...params })}`, { signal, cache: "no-store" }).then((response) => readApiJson<T>(response));
}

export function GitPushDialog({ cwd, committed, onCancel, onPush }: {
  cwd: string;
  committed: boolean;
  onCancel: () => void;
  onPush: (preview: GitPushPreview) => Promise<string | null>;
}) {
  const { locale, t } = useI18n();
  const titleId = useId();
  const dialog = useRef<HTMLDialogElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const pageRequest = useRef<AbortController | null>(null);
  const [reload, setReload] = useState(0);
  const [preview, setPreview] = useState<GitPushPreview>();
  const [hash, setHash] = useState<string>();
  const [detail, setDetail] = useState<{ hash: string; data?: GitHistoryCommit; error?: string }>();
  const [error, setError] = useState<string>();
  const [pushError, setPushError] = useState<string>();
  const [paging, setPaging] = useState(false);
  const [pushing, setPushing] = useState(false);
  const selected = detail?.hash === hash ? detail : undefined;
  const commit = selected?.data;

  useLayoutEffect(() => {
    const element = dialog.current;
    element?.showModal();
    cancel.current?.focus({ preventScroll: true });
    return () => { if (element?.open) element.close(); };
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    void load<GitPushPreview>("push-preview", cwd, {}, controller.signal).then((data) => {
      if (controller.signal.aborted) return;
      setPreview(data);
      setHash(data.commits[0]?.hash);
    }, (reason: unknown) => { if (!controller.signal.aborted) setError(String(reason instanceof Error ? reason.message : reason)); });
    return () => { controller.abort(); pageRequest.current?.abort(); pageRequest.current = null; };
  }, [cwd, reload]);

  useEffect(() => {
    if (!hash) return;
    const controller = new AbortController();
    void load<GitHistoryCommit>("commit", cwd, { hash }, controller.signal).then((data) => {
      if (!controller.signal.aborted) setDetail({ hash, data });
    }, (reason: unknown) => { if (!controller.signal.aborted) setDetail({ hash, error: reason instanceof Error ? reason.message : String(reason) }); });
    return () => controller.abort();
  }, [cwd, hash]);

  function retry() {
    setPreview(undefined);
    setHash(undefined);
    setDetail(undefined);
    setError(undefined);
    setPushError(undefined);
    setPaging(false);
    setReload((value) => value + 1);
  }

  async function loadMore() {
    if (preview?.nextOffset == null || pageRequest.current) return;
    const { token, nextOffset } = preview;
    const controller = new AbortController();
    pageRequest.current = controller;
    setPaging(true);
    try {
      const page = await load<GitPushPreview>("push-preview", cwd, { token, offset: String(nextOffset) }, controller.signal);
      if (!controller.signal.aborted) setPreview((current) => current?.token === token ? { ...current, commits: [...current.commits, ...page.commits], nextOffset: page.nextOffset } : current);
    } catch (reason) {
      if (!controller.signal.aborted) setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      if (!controller.signal.aborted) {
        pageRequest.current = null;
        setPaging(false);
      }
    }
  }

  async function confirm() {
    if (!preview?.commits.length || pushing || paging || error || pushError) return;
    setPushing(true);
    const failure = await onPush(preview);
    if (failure !== null) {
      setPushError(failure);
      setPushing(false);
    }
  }

  return <dialog ref={dialog} className={styles.dialog} aria-labelledby={titleId}
    onKeyDown={(event) => { if (event.key === "Escape") event.stopPropagation(); }}
    onCancel={(event) => { event.preventDefault(); if (!pushing) onCancel(); }}>
    <div className={styles.layout}>
      <header className={styles.header}>
        <h2 id={titleId}>{t("gitPush.title", { name: getFileName(cwd) || cwd })}</h2>
        {preview && <div className={styles.route} aria-label={t("gitPush.route")}>
          <code title={preview.branch}>{preview.branch}</code><ArrowRight size={14} aria-hidden="true" />
          <code title={`${preview.remote}/${preview.targetBranch}`}>{preview.remote}/{preview.targetBranch}</code>
        </div>}
      </header>
      <div className={styles.panes}>
        <section className={styles.column} aria-label={t("gitPush.pending")}>
          <h3>{t("gitPush.pending")}</h3>
          <div className={styles.scroll}>
            {!preview && !error && <p className={styles.hint} role="status">{t("gitHistory.loading")}</p>}
            {preview?.commits.length === 0 && <p className={styles.hint}>{t("gitPush.empty")}</p>}
            {preview && !preview.hasTrackingRef && <p className={styles.hint}>{t("gitPush.noTracking")}</p>}
            <ul className={styles.list}>{preview?.commits.map((item) => <li key={item.hash}>
              <button type="button" className={styles.commitRow} aria-pressed={item.hash === hash} onClick={() => setHash(item.hash)}>
                <strong>{item.subject || t("gitHistory.noSubject")}</strong>
                <span>{item.author} <code>{item.hash.slice(0, 8)}</code></span>
              </button>
            </li>)}</ul>
            {preview?.nextOffset != null && !error && <button className={styles.more} type="button" disabled={paging || pushing} onClick={() => void loadMore()}>
              {t(paging ? "gitHistory.loading" : "gitPush.more")}
            </button>}
          </div>
        </section>
        <section className={styles.column} aria-label={t("gitHistory.details")}>
          <h3>{t("gitHistory.files")}{commit && <span className={styles.count}>{commit.files.length}</span>}</h3>
          <div className={styles.scroll}>
            {!hash && <p className={styles.hint}>{t("gitHistory.selectCommit")}</p>}
            {hash && !selected && <p className={styles.hint} role="status">{t("gitHistory.loading")}</p>}
            {selected?.error && <p className={styles.hint} role="alert">{selected.error}</p>}
            {commit && <>
              {commit.parents.length > 1 && <p className={styles.hint}>{t("gitHistory.firstParent")}</p>}
              {!commit.files.length && <p className={styles.hint}>{t("gitHistory.noFiles")}</p>}
              <ul className={styles.files}>{commit.files.map((file) => <li key={file.path}>
                <FileText size={14} aria-hidden="true" /><code>{file.status}</code>
                <span>{file.path}{file.oldPath && <small>{t("gitHistory.oldPath", { path: file.oldPath })}</small>}</span>
              </li>)}</ul>
              <div className={styles.detail}>
                <h4>{t("gitHistory.details")}</h4>
                <p className={styles.message}>{commit.message || t("gitHistory.noSubject")}</p>
                <dl>
                  <dt>{t("gitHistory.hash")}</dt><dd><code>{commit.hash}</code></dd>
                  <dt>{t("gitHistory.author")}</dt><dd>{commit.author}{commit.authorEmail ? ` <${commit.authorEmail}>` : ""}</dd>
                  <dt>{t("gitHistory.time")}</dt><dd><time dateTime={commit.authoredAt}>{new Date(commit.authoredAt).toLocaleString(locale)}</time></dd>
                </dl>
              </div>
            </>}
          </div>
        </section>
      </div>
      <footer className={styles.footer}>
        <div className={styles.notes}>
          {committed && <span>{t("gitPush.committed")}</span>}
          {(error || pushError) && <p role="alert">{error || pushError} <button type="button" onClick={retry}>{t("gitPush.reload")}</button></p>}
        </div>
        <div className={styles.buttons}>
          <button type="button" ref={cancel} disabled={pushing} onClick={onCancel}>{t("sidebar.cancel")}</button>
          <button type="button" className={styles.primary} disabled={!preview?.commits.length || pushing || paging || Boolean(error || pushError)} onClick={() => void confirm()}>
            {t(pushing ? "git.working" : "git.push")}
          </button>
        </div>
      </footer>
    </div>
  </dialog>;
}
