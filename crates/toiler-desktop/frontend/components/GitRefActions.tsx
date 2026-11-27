"use client";

import { useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useI18n } from "@/hooks/useI18n";
import { readApiJson } from "@/lib/api-json";
import { REF_ACTIONS, historyRefKind, shortRef, type GitHistoryRef, type GitHistoryRefs, type RefAction } from "@/lib/git-history";
import { HistoryActionMenu, type HistoryActionAnchor } from "./GitHistoryActions";
import styles from "./GitHistoryActions.module.css";
import panelStyles from "./GitHistoryPanel.module.css";

export interface RefActionAnchor extends HistoryActionAnchor {
  item: GitHistoryRef;
}

type RequestAction =
  | { action: "checkout" | "update" | "push_tag" }
  | { action: "delete"; force?: boolean }
  | { action: "merge"; expectedBranch: string }
  | { action: "create_branch" | "rename"; newBranch: string }
  | { action: "push"; remote: string; targetBranch: string };

type Confirmation = { ref: string; action: "checkout" | "create_branch" | "rename" | "delete" | "push"; remotes: string[]; upstream?: GitHistoryRefs["upstreams"][string] };

export async function requestGitRefAction(cwd: string, ref: string, command: RequestAction) {
  const response = await fetch("/api/git/ref-action", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ cwd, ref, ...command }),
  });
  await readApiJson<{ ok: true }>(response);
}

export function GitRefActions({ cwd, data, anchor, onClose, onChanged, onSelection, restoreFocus }: {
  cwd: string;
  data?: GitHistoryRefs;
  anchor: RefActionAnchor | null;
  onClose: (restore: boolean) => void;
  onChanged: () => void;
  onSelection: (reference: string, next: string) => void;
  restoreFocus: () => void;
}) {
  const { t } = useI18n();
  const [confirmation, setConfirmation] = useState<Confirmation | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const pending = useRef(false);
  const present = useRef<HTMLDivElement>(null);

  async function run(ref: string, command: RequestAction) {
    if (pending.current) return;
    pending.current = true;
    setBusy(true);
    setError(null);
    try {
      await requestGitRefAction(cwd, ref, command);
      if (command.action === "checkout" || command.action === "create_branch") onSelection(ref, "HEAD");
      if (command.action === "rename") onSelection(ref, `refs/heads/${command.newBranch}`);
      setConfirmation(null);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      pending.current = false;
      setBusy(false);
      onChanged();
      requestAnimationFrame(() => {
        if (present.current?.isConnected && !document.querySelector("dialog[open]")) restoreFocus();
      });
    }
  }

  function choose(action: RefAction) {
    if (!anchor || !data || pending.current) return;
    const ref = anchor.item.name;
    const kind = historyRefKind(ref);
    setError(null);
    onClose(false);
    if (action === "create_branch" || action === "rename" || action === "push" || action === "delete" || (action === "checkout" && kind === "tag")) {
      setConfirmation({ ref, action, remotes: data.remotes, upstream: data.upstreams[ref] });
    } else if (action === "merge") {
      if (data.headBranch) void run(ref, { action, expectedBranch: data.headBranch });
    } else {
      void run(ref, { action });
    }
  }

  const ref = anchor?.item.name;
  const kind = ref ? historyRefKind(ref) : "local";
  const current = ref === data?.headBranch;
  const branch = shortRef(ref ?? "");
  const into = data?.headBranch ? shortRef(data.headBranch) : "";

  return <>
    <div ref={present} hidden={Boolean(confirmation) || (!error && !busy)}>
      {busy && <p className={panelStyles.actionStatus} role="status">{t("gitRefs.working")}</p>}
      {error && <p className={panelStyles.actionError} role="alert">{error}</p>}
    </div>
    {anchor && ref && data && !busy && !confirmation && <HistoryActionMenu anchor={anchor} label={t("gitRefs.actions", { branch })} onClose={onClose} wide>
      {REF_ACTIONS[kind].map((action) => {
        const disabled = (action === "merge" && (!into || current)) || (current && (action === "checkout" || action === "delete"))
          || (action === "update" && !data.upstreams[ref]) || (action === "push" && !data.remotes.length)
          || (action === "push_tag" && !data.remotes.includes("origin"));
        return <button key={action} type="button" role="menuitem" tabIndex={-1} disabled={disabled}
          data-danger={action === "delete"} onClick={() => choose(action)}
          title={action === "update" && !data.upstreams[ref] ? t("git.noUpstream") : undefined}>
          {t(action === "delete" ? deleteLabel(kind) : `gitRefs.${action}`, { branch, into: into || t("gitRefs.currentBranch") })}
        </button>;
      })}
    </HistoryActionMenu>}
    {confirmation && <RefActionDialog target={confirmation} busy={busy} error={error}
      onCancel={() => { setConfirmation(null); setError(null); requestAnimationFrame(restoreFocus); }}
      onConfirm={(command) => void run(confirmation.ref, command)} />}
  </>;
}

function deleteLabel(kind: ReturnType<typeof historyRefKind>) {
  return kind === "local" ? "sidebar.deleteLocalBranch" : kind === "remote" ? "sidebar.deleteRemoteBranch" : "git.deleteTag";
}

export function DeleteBranchDialog({ reference, busy, error, onCancel, onConfirm }: {
  reference: string;
  busy: boolean;
  error: string | null;
  onCancel: () => void;
  onConfirm: (force: boolean) => void;
}) {
  return <RefActionDialog key={reference} target={{ ref: reference, action: "delete", remotes: [] }} busy={busy} error={error}
    onCancel={onCancel} onConfirm={(command) => { if (command.action === "delete") onConfirm(command.force === true); }} />;
}

function RefActionDialog({ target, busy, error, onCancel, onConfirm }: {
  target: Confirmation;
  busy: boolean;
  error: string | null;
  onCancel: () => void;
  onConfirm: (command: RequestAction) => void;
}) {
  const { t } = useI18n();
  const titleId = useId();
  const dialog = useRef<HTMLDialogElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const source = shortRef(target.ref);
  const kind = historyRefKind(target.ref);
  const isPush = target.action === "push";
  const isDelete = target.action === "delete";
  const canForceDelete = isDelete && kind === "local";
  const [forceDelete, setForceDelete] = useState(false);
  const title = isDelete ? deleteLabel(kind) : `gitRefs.title.${target.action}`;
  const hasName = isPush || target.action === "create_branch" || target.action === "rename";
  const defaultRemote = target.remotes.includes(target.upstream?.remote ?? "") ? target.upstream!.remote
    : target.remotes.includes("origin") ? "origin" : target.remotes[0] ?? "";
  const [remote, setRemote] = useState(defaultRemote);
  const [name, setName] = useState(isPush && target.upstream?.remote === defaultRemote ? target.upstream.branch : target.action === "create_branch" ? "" : source);
  const description = isDelete ? `gitRefs.delete.${kind}` : isPush ? "gitRefs.pushHint" : target.action === "checkout" ? "gitRefs.detachHint" : undefined;
  useLayoutEffect(() => {
    const element = dialog.current;
    element?.showModal();
    (input.current ?? cancel.current)?.focus({ preventScroll: true });
    return () => { if (element?.open) element.close(); };
  }, []);

  return createPortal(<dialog ref={dialog} className={styles.dialog} data-git-history-action aria-labelledby={titleId}
    onKeyDown={(event) => { if (event.key === "Escape") event.stopPropagation(); }}
    onCancel={(event) => { event.preventDefault(); if (!busy) onCancel(); }}>
    <form onSubmit={(event) => {
      event.preventDefault();
      if (busy || (hasName && !name.trim())) return;
      if (target.action === "push") onConfirm({ action: "push", remote, targetBranch: name.trim() });
      else if (target.action === "create_branch" || target.action === "rename") onConfirm({ action: target.action, newBranch: name.trim() });
      else if (target.action === "delete") onConfirm(canForceDelete && forceDelete ? { action: "delete", force: true } : { action: "delete" });
      else onConfirm({ action: target.action });
    }}>
      <h2 id={titleId}>{t(title)}</h2>
      <p className={styles.target}><code>{source}</code></p>
      {description && <p className={styles.muted}>{t(description)}</p>}
      {isPush && <label className={styles.refField}>{t("gitRefs.remoteLabel")}
        <select disabled={busy} value={remote} onChange={(event) => setRemote(event.target.value)}>
          {target.remotes.map((item) => <option key={item} value={item}>{item}</option>)}
        </select>
      </label>}
      {hasName && <label className={styles.refField}>{t(isPush ? "gitRefs.targetName" : "gitRefs.branchName")}
        <input ref={input} value={name} onChange={(event) => setName(event.target.value)} required disabled={busy} spellCheck={false} autoComplete="off" />
      </label>}
      {canForceDelete && <label className={styles.mode}>
        <input type="checkbox" checked={forceDelete} disabled={busy} onChange={(event) => setForceDelete(event.target.checked)} />
        <span><strong>{t("gitRefs.forceDelete")}</strong><small>{t("gitRefs.forceDeleteHint")}</small></span>
      </label>}
      {error && <p className={styles.error} role="alert">{error}</p>}
      <div className={styles.actions}>
        <button ref={cancel} type="button" disabled={busy} onClick={onCancel}>{t("sidebar.cancel")}</button>
        <button type="submit" data-primary data-danger={isDelete} disabled={busy || (hasName && !name.trim()) || (isPush && !remote)}>
          {t(busy ? "gitRefs.working" : canForceDelete && forceDelete ? "gitRefs.forceDelete" : title)}
        </button>
      </div>
    </form>
  </dialog>, document.body);
}
