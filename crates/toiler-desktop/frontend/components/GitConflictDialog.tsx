"use client";

import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { GitMerge, X } from "lucide-react";
import { useI18n } from "@/hooks/useI18n";
import { readApiJson } from "@/lib/api-json";
import { mergeMarkerOffsets, mergeTextForSave, normalizeMergeText } from "@/lib/git-conflicts";
import styles from "./GitConflictDialog.module.css";

interface ConflictFile { local: string | null; incoming: string | null; result: string | null; token: string; markerSize: number }

interface Props {
  cwd: string;
  path: string;
  remaining: number;
  onClose: () => void;
  onResolved: (response: Response) => Promise<void>;
}

export function GitConflictDialog({ cwd, path, remaining, onClose, onResolved }: Props) {
  const { t } = useI18n();
  const title = useId();
  const dialog = useRef<HTMLDialogElement>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const editor = useRef<HTMLTextAreaElement>(null);
  const [file, setFile] = useState<ConflictFile>();
  const [text, setText] = useState("");
  const [remove, setRemove] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [confirmClosing, setConfirmClosing] = useState(false);
  const [retry, setRetry] = useState(0);
  const markers = useMemo(() => file ? mergeMarkerOffsets(text, file.markerSize) : [], [file, text]);
  const dirty = !!file && (remove || text !== normalizeMergeText(file.result ?? ""));

  useLayoutEffect(() => {
    const node = dialog.current;
    node?.showModal();
    heading.current?.focus();
    return () => node?.close();
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    void fetch(`/api/git/conflict?${new URLSearchParams({ cwd, path })}`, { signal: controller.signal, cache: "no-store" })
      .then((response) => readApiJson<ConflictFile>(response))
      .then((data) => {
        if (!controller.signal.aborted) {
          setFile(data);
          setText(normalizeMergeText(data.result ?? ""));
        }
      }, (reason: unknown) => {
        if (!controller.signal.aborted) setError(reason instanceof Error ? reason.message : String(reason));
      });
    return () => controller.abort();
  }, [cwd, path, retry]);

  function close() {
    if (busy) return;
    if (dirty) setConfirmClosing(true);
    else onClose();
  }

  function nextMarker() {
    const node = editor.current;
    if (!node || !markers.length) return;
    const from = node.selectionStart === node.selectionEnd ? node.selectionStart : node.selectionEnd;
    const offset = markers.find((position) => position >= from) ?? markers[0];
    const end = node.value.indexOf("\n", offset);
    node.focus();
    node.setSelectionRange(offset, end < 0 ? node.value.length : end);
    node.scrollTop = Math.max(0, (node.value.slice(0, offset).split("\n").length - 3) * parseFloat(getComputedStyle(node).lineHeight));
  }

  async function resolve() {
    if (!file || busy || (!remove && markers.length > 0)) return;
    setBusy(true);
    setError("");
    try {
      const resolution = remove ? { kind: "delete" } : { kind: "text", content: mergeTextForSave(text, file.result) };
      const response = await fetch("/api/git/conflict", {
        method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ cwd, path, token: file.token, resolution }),
      });
      await onResolved(response);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      setBusy(false);
    }
  }

  return createPortal(<dialog ref={dialog} className={styles.dialog} aria-labelledby={title}
    onCancel={(event) => { event.preventDefault(); event.stopPropagation(); close(); }}
    onKeyDown={(event) => { if (event.key === "Escape") event.stopPropagation(); }}>
    <div className={styles.layout}>
      <header className={styles.header}>
        <div><h2 id={title} ref={heading} tabIndex={-1}><GitMerge size={16} aria-hidden="true" />{t("gitConflict.title")}</h2>
          <p title={path}>{path}</p></div>
        <span className={styles.count}>{t("gitConflict.remaining", { count: remaining })}</span>
        <button type="button" className={styles.close} disabled={busy} onClick={close} aria-label={t("gitConflict.close")}><X size={19} aria-hidden="true" /></button>
      </header>
      {file ? <div className={styles.viewport}>
        <div className={styles.columns}>
          <SourceColumn label={t("gitConflict.local")} content={file.local} missing={t("gitConflict.missing")} />
          <section className={styles.column} data-result>
            <h3>{t("gitConflict.result")}{markers.length > 0 && !remove && <button type="button" disabled={busy} onClick={nextMarker}>{t("gitConflict.nextMarker")}</button>}</h3>
            {remove ? <p className={styles.hint}>{t("gitConflict.deleteHint")}</p> :
              <textarea ref={editor} value={text} onChange={(event) => setText(event.target.value)} disabled={busy} aria-label={t("gitConflict.result")} wrap="off" spellCheck={false} autoCapitalize="off" autoCorrect="off" />}
          </section>
          <SourceColumn label={t("gitConflict.incoming")} content={file.incoming} missing={t("gitConflict.missing")} />
        </div>
      </div> : <div className={styles.hint}>{error ? <><p role="alert">{error}</p><button type="button" onClick={() => { setError(""); setRetry((value) => value + 1); }}>{t("gitHistory.retry")}</button></> : <p role="status">{t("gitHistory.loading")}</p>}</div>}
      <footer className={styles.footer}>
        {confirmClosing ? <><span>{t("gitConflict.discardDraft")}</span><div className={styles.actions}>
          <button type="button" onClick={() => setConfirmClosing(false)}>{t("gitConflict.keepEditing")}</button>
          <button type="button" onClick={onClose}>{t("gitConflict.discardAndClose")}</button>
        </div></> : <>
          <div className={styles.notes}>
            {file && error ? <span role="alert">{error}</span> : file && <span>{remove ? t("gitConflict.deleteHint") : markers.length ? t("gitConflict.markersRemain") : t("gitConflict.saveHint")}</span>}
            {file && (file.local === null || file.incoming === null) && <label><input type="checkbox" checked={remove} disabled={busy} onChange={(event) => setRemove(event.target.checked)} />{t("gitConflict.deleteFile")}</label>}
          </div>
          <div className={styles.actions}><button type="button" disabled={busy} onClick={close}>{t("gitConflict.close")}</button>
            <button className={styles.primary} type="button" disabled={!file || busy || (!remove && markers.length > 0)} onClick={() => void resolve()}>{busy ? t("gitConflict.saving") : t("gitConflict.resolved")}</button></div>
        </>}
      </footer>
    </div>
  </dialog>, document.body);
}

function SourceColumn({ label, content, missing }: { label: string; content: string | null; missing: string }) {
  return <section className={styles.column}><h3>{label}</h3>{content === null ? <p className={styles.hint}>{missing}</p> :
    <pre aria-label={label} tabIndex={0}>{content}</pre>}</section>;
}
