"use client";

import { useEffect, useId, useImperativeHandle, useLayoutEffect, useMemo, useRef, useState, type Ref } from "react";
import { ArrowDown, ArrowUp, ChevronsLeft, ChevronsRight, GitMerge, WandSparkles, X } from "lucide-react";
import { useI18n } from "@/hooks/useI18n";
import { readApiJson } from "@/lib/api-json";
import { mergeTextForSave, normalizeMergeText, parseMergeDraft } from "@/lib/git-conflicts";
import { planSimpleMerge, simpleMergeEdits } from "@/lib/git-simple-merge";
import { GitMergeEditors, type MergeEditorHandle, type MergeHighlight } from "./GitMergeEditors";
import styles from "./GitConflictEditor.module.css";

interface ConflictFile { local: string | null; incoming: string | null; result: string | null; token: string; markerSize: number; diff3?: string | null }

export interface GitConflictEditorHandle {
  requestLeave: (next: () => void) => void;
}

interface Props {
  cwd: string;
  path: string;
  branch: string | null;
  remaining: number;
  active: boolean;
  handleRef: Ref<GitConflictEditorHandle>;
  onReveal: () => void;
  onClose: () => void;
  onResolved: (response: Response) => Promise<void>;
}

export function GitConflictEditor({ cwd, path, branch, remaining, active, handleRef, onReveal, onClose, onResolved }: Props) {
  const { t } = useI18n();
  const title = useId();
  const keepEditing = useRef<HTMLButtonElement>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const editor = useRef<MergeEditorHandle>(null);
  const lastTarget = useRef(0);
  const [file, setFile] = useState<ConflictFile>();
  const [text, setText] = useState("");
  const [remove, setRemove] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [ready, setReady] = useState(false);
  const [highlight, setHighlight] = useState<MergeHighlight>("words");
  const [nextAction, setNextAction] = useState<(() => void) | null>(null);
  const confirmClosing = nextAction !== null;
  const [retry, setRetry] = useState(0);
  const parsed = useMemo(() => parseMergeDraft(text, file?.markerSize ?? 7), [file, text]);
  const simplePlans = useMemo(() => file?.diff3 && file.result !== null
    ? planSimpleMerge(normalizeMergeText(file.result), normalizeMergeText(file.diff3), file.markerSize,
      !!file.local?.endsWith("\n") && !!file.incoming?.endsWith("\n"))
    : new Map<string, string>(), [file]);
  const simpleCount = useMemo(() => simpleMergeEdits(text, parsed.blocks, simplePlans).length, [text, parsed, simplePlans]);
  const targets = useMemo(() => [...parsed.blocks.map((block) => block.startLine), ...parsed.looseMarkers.map((marker) => marker.line)].sort((a, b) => a - b), [parsed]);
  const dirty = !!file && (remove || text !== normalizeMergeText(file.result ?? ""));
  const incomingLabel = useMemo(() => {
    if (!file) return "";
    const labels = new Set(parseMergeDraft(normalizeMergeText(file.result ?? ""), file.markerSize).blocks.map((block) => block.incomingLabel).filter(Boolean));
    if (labels.size !== 1) return "";
    const ref = [...labels][0];
    return /^[a-f\d]{40,64}$/i.test(ref) ? ref.slice(0, 12) : ref;
  }, [file]);
  const labels = useMemo<[string, string, string]>(() => [
    branch ? t("gitConflict.fromRef", { ref: branch }) : t("gitConflict.local"),
    `${t("gitConflict.result")} · ${path.split("/").at(-1)}`,
    incomingLabel ? t("gitConflict.fromRef", { ref: incomingLabel }) : t("gitConflict.incoming"),
  ], [branch, incomingLabel, path, t]);

  useLayoutEffect(() => {
    if (active) (confirmClosing ? keepEditing.current : heading.current)?.focus({ preventScroll: true });
  }, [active, confirmClosing]);

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

  function requestLeave(next: () => void) {
    if (busy) return;
    if (!dirty) { next(); return; }
    setNextAction(() => next);
    onReveal();
  }

  useImperativeHandle(handleRef, () => ({ requestLeave }));
  const close = () => requestLeave(onClose);

  function navigate(forward: boolean) {
    if (!editor.current || !targets.length) return;
    const line = forward ? targets.find((line) => line > lastTarget.current) ?? targets[0]
      : targets.slice().reverse().find((line) => line < lastTarget.current) ?? targets.at(-1)!;
    lastTarget.current = line;
    editor.current.reveal(line);
  }

  async function resolve() {
    if (!file || busy || (!remove && (!ready || parsed.markers.length > 0))) return;
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

  const navDisabled = !ready || busy || remove || !targets.length;
  const batchDisabled = !ready || busy || remove || !parsed.blocks.length;
  const prev = t("gitConflict.previousBlock");
  const next = t("gitConflict.nextBlock");
  return <section data-git-conflict-editor className={styles.editor} aria-labelledby={title}
    onKeyDown={(event) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      event.preventDefault();
      event.stopPropagation();
      if (confirmClosing) setNextAction(null);
      else close();
    }}>
      <header className={styles.header}>
        <GitMerge size={17} aria-hidden="true" />
        <h2 id={title} ref={heading} tabIndex={-1}>{t("gitConflict.title")}</h2>
        <span className={styles.path} title={path}>{path}</span>
        <button type="button" className={styles.icon} disabled={busy} onClick={close} aria-label={t("gitConflict.close")}><X size={18} aria-hidden="true" /></button>
      </header>
      <div className={styles.toolbar}>
        <div className={styles.toolGroup}>
          <button type="button" className={styles.icon} disabled={navDisabled} title={prev} aria-label={prev} onClick={() => navigate(false)}><ArrowUp size={16} aria-hidden="true" /></button>
          <button type="button" className={styles.icon} disabled={navDisabled} title={next} aria-label={next} onClick={() => navigate(true)}><ArrowDown size={16} aria-hidden="true" /></button>
        </div>
        <button type="button" title={t(simpleCount ? "gitConflict.simpleHelp" : "gitConflict.noSimpleChanges")}
          disabled={!ready || busy || confirmClosing || remove || simpleCount === 0} onClick={() => editor.current?.mergeSimple(simplePlans)}>
          <WandSparkles size={15} aria-hidden="true" />{t("gitConflict.mergeSimple")}{simpleCount > 0 && ` (${simpleCount})`}
        </button>
        <label className={styles.highlight}>{t("gitConflict.highlight")}
          <select value={highlight} onChange={(event) => setHighlight(event.target.value as MergeHighlight)}>
            <option value="words">{t("gitConflict.highlightWords")}</option>
            <option value="lines">{t("gitConflict.highlightLines")}</option>
            <option value="off">{t("gitConflict.highlightOff")}</option>
          </select>
        </label>
        <div className={styles.status} aria-live="polite">
          {file && <><span>{t(dirty ? "gitConflict.draftChanged" : "gitConflict.draftUnchanged")}</span>
            <span>{t("gitConflict.blockCount", { count: remove ? 0 : parsed.blocks.length })}</span></>}
          <span>{t("gitConflict.remaining", { count: remaining })}</span>
        </div>
      </div>
      {file ? <div className={styles.viewport}>
        <GitMergeEditors path={path} source={file} labels={labels} text={text} parsed={parsed} highlight={highlight} busy={busy || confirmClosing} remove={remove}
          handleRef={editor} onChange={setText} onReady={() => setReady(true)} />
      </div> : <div className={styles.hint}>{error ? <><p role="alert">{error}</p><button type="button" onClick={() => { setError(""); setRetry((value) => value + 1); }}>{t("gitHistory.retry")}</button></> : <p role="status">{t("gitHistory.loading")}</p>}</div>}
      <footer className={styles.footer}>
        {confirmClosing ? <><span>{t("gitConflict.discardDraft")}</span><div className={styles.actions}>
          <button ref={keepEditing} type="button" onClick={() => setNextAction(null)}>{t("gitConflict.keepEditing")}</button>
          <button type="button" onClick={() => { setNextAction(null); nextAction?.(); }}>{t("gitConflict.discardAndClose")}</button>
        </div></> : <>
          <div className={styles.batch}>
            <button type="button" disabled={batchDisabled || file?.local == null} onClick={() => editor.current?.accept("local")}><ChevronsRight size={16} aria-hidden="true" />{t("gitConflict.acceptAllLocal")}</button>
            <button type="button" disabled={batchDisabled || file?.incoming == null} onClick={() => editor.current?.accept("incoming")}><ChevronsLeft size={16} aria-hidden="true" />{t("gitConflict.acceptAllIncoming")}</button>
          </div>
          <div className={styles.notes}>
            {file && error ? <span role="alert">{error}</span> : file && <span>{remove ? t("gitConflict.deleteHint") : parsed.looseMarkers.length ? t("gitConflict.looseMarkers") : parsed.markers.length ? t("gitConflict.markersRemain") : t("gitConflict.saveHint")}</span>}
            {file && (file.local === null || file.incoming === null) && <label><input type="checkbox" checked={remove} disabled={busy} onChange={(event) => setRemove(event.target.checked)} />{t("gitConflict.deleteFile")}</label>}
          </div>
          <div className={styles.actions}><button type="button" disabled={busy} onClick={close}>{t("gitConflict.cancel")}</button>
            <button className={styles.primary} type="button" disabled={!file || busy || (!remove && (!ready || parsed.markers.length > 0))} onClick={() => void resolve()}>{busy ? t("gitConflict.saving") : t("gitConflict.apply")}</button></div>
        </>}
      </footer>
  </section>;
}
