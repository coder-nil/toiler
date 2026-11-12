"use client";

import { useCallback, useEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { ArrowDown, ArrowUp } from "lucide-react";
import { useI18n } from "@/hooks/useI18n";
import { readApiJson } from "@/lib/api-json";
import { revealNext } from "@/lib/typewriter";
import { BranchPicker } from "./BranchPicker";

// MergeBranchPicker delegates to the shared picker, which renders the accessible
// role="combobox" / role="listbox" control and uses git.selectBranch as its fallback label.

type GitFileStatus = {
  filePath: string;
  status: "modified" | "added" | "deleted" | "renamed" | "untracked" | "conflict";
  code: string;
  indexStatus: string;
  worktreeStatus: string;
};

type GitSummary = {
  isGitRepository: boolean;
  repositoryRoot: string | null;
  branch: string | null;
  upstream: string | null;
  ahead: number;
  behind: number;
  remote: string | null;
  credentialKind: "https" | "ssh" | "none";
  hasSavedCredential: boolean;
  savedCredentialUsername: string | null;
  operation: "merge" | "rebase" | "cherry-pick" | "revert" | null;
  branches: string[];
  changes: { files: GitFileStatus[]; additions: number; deletions: number };
  untrackedPaths: string[];
};

type Action = "stage" | "unstage" | "discard" | "discard_all" | "commit" | "fetch" | "pull" | "push" | "merge" | "continue" | "abort" | "set_remote_url" | "summarize";

const SAVED_CREDENTIAL_MASK = "••••••••";

const STATUS_COLOR: Record<GitFileStatus["status"], string> = {
  modified: "#d6a84b", added: "#4ade80", deleted: "#f87171", renamed: "#60a5fa", untracked: "#4ade80", conflict: "#f87171",
};

function fileName(filePath: string): string {
  return filePath.split(/[\\/]/).pop() ?? filePath;
}

export function GitPanel({ cwd, sessionId, onClose, onChanged, onOpenFile }: { cwd: string; sessionId: string | null; onClose: () => void; onChanged: () => void; onOpenFile: (filePath: string, fileName: string, options?: { modeHint?: "diff" }) => void }) {
  const { locale, t } = useI18n();
  const [summary, setSummary] = useState<GitSummary | null>(null);
  const [message, setMessage] = useState("");
  const [mergeBranch, setMergeBranch] = useState("");
  const [pullRebase, setPullRebase] = useState(false);
  const [username, setUsername] = useState("");
  const [secret, setSecret] = useState("");
  const [rememberCredential, setRememberCredential] = useState(false);
  const [editingRemote, setEditingRemote] = useState(false);
  const [remoteDraft, setRemoteDraft] = useState("");
  const [busy, setBusy] = useState<Action | null>(null);
  const [confirmingDiscardAll, setConfirmingDiscardAll] = useState(false);
  const [confirmingDiscardFile, setConfirmingDiscardFile] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [checkedUpdate, setCheckedUpdate] = useState(false);

  // Prefill the credential form from the encrypted store so users can tell at a
  // glance whether they already saved a credential for this remote. The secret
  // itself is never sent to the browser — only the username and the remembered
  // checkbox state are restored.
  const credentialPrefillKey = summary?.isGitRepository ? `${summary.remote ?? ""}|${summary.hasSavedCredential ? 1 : 0}` : null;
  const prefilledKeyRef = useRef<string | null>(null);
  // Drives the typewriter reveal of the AI summary; see summarizeCommitMessage.
  const summaryRevealRef = useRef({ frame: 0, target: "", displayed: "" });
  useEffect(() => () => {
    const reveal = summaryRevealRef.current;
    if (reveal.frame) cancelAnimationFrame(reveal.frame);
    reveal.frame = 0;
    reveal.target = reveal.displayed;
  }, []);
  useEffect(() => {
    if (!credentialPrefillKey || prefilledKeyRef.current === credentialPrefillKey) return;
    prefilledKeyRef.current = credentialPrefillKey;
    setRememberCredential(Boolean(summary?.hasSavedCredential));
    if (summary?.hasSavedCredential && !username) setUsername(summary.savedCredentialUsername ?? "");
    // A project can hold several sub-project repositories whose remotes each
    // have their own saved credential. Without the reset below, the previous
    // repository leaves its mask in the field, the panel then submits no
    // credential at all (the mask is display-only) while the form still looks
    // filled in, and an SSH remote fails with "Permission denied (publickey)".
    if (summary?.hasSavedCredential) {
      setSecret(SAVED_CREDENTIAL_MASK);
    } else {
      setSecret("");
      setUsername("");
    }
  }, [credentialPrefillKey, summary?.hasSavedCredential, summary?.savedCredentialUsername, username]);

  const refresh = useCallback(async () => {
    const res = await fetch(`/api/git?${new URLSearchParams({ cwd }).toString()}`);
    setSummary(await readApiJson<GitSummary>(res, { hint: t("git.serverErrorHint") }));
  }, [cwd, t]);

  // 一个项目里可以有多个仓库（子项目），从文件浏览器的徽标切换仓库根时先丢掉上一个
  // 仓库的文件列表与错误，否则新目标会先闪一帧旧仓库的内容。
  useEffect(() => {
    setSummary(null);
    setError(null);
  }, [cwd]);

  useEffect(() => { void refresh().catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason))); }, [refresh]);

  // When the panel opens and there are remote updates (behind > 0), show a gentle reminder.
  useEffect(() => {
    if (!summary || !summary.upstream || summary.behind <= 0) {
      setCheckedUpdate(false);
      return;
    }
    // Only remind once per panel open session.
    if (checkedUpdate) return;
    setCheckedUpdate(true);
  }, [summary, checkedUpdate]);

  const run = useCallback(async (action: Action, extra: Record<string, unknown> = {}) => {
    setBusy(action);
    setError(null);
    try {
      const isRemoteAction = action === "fetch" || action === "pull" || action === "push";
      const credentialKind = summary?.credentialKind;
      const credential = isRemoteAction && secret && secret !== SAVED_CREDENTIAL_MASK && (credentialKind === "https" || credentialKind === "ssh")
        ? { kind: credentialKind, secret, ...(credentialKind === "https" ? { username } : {}) }
        : undefined;
      const res = await fetch("/api/git", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ cwd, action, ...extra, ...(credential ? { credential, rememberCredential } : {}) }) });
      const data = await readApiJson<GitSummary>(res, { hint: t("git.serverErrorHint") });
      setSummary(data);
      if (action === "commit") setMessage("");
      if (action === "merge") setMergeBranch("");
      if (action === "set_remote_url") {
        setEditingRemote(false);
        setRemoteDraft("");
        setSecret("");
        setUsername("");
        setRememberCredential(false);
        prefilledKeyRef.current = null;
      }
      if (isRemoteAction) setSecret(data.hasSavedCredential ? SAVED_CREDENTIAL_MASK : "");
      onChanged();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      void refresh().catch(() => undefined);
    } finally {
      setBusy(null);
    }
  }, [cwd, onChanged, refresh, rememberCredential, secret, summary?.credentialKind, t, username]);

  const summarizeCommitMessage = useCallback(async () => {
    setBusy("summarize");
    setError(null);
    // Reveal streamed text at a steady pace so a burst of tokens still reads as
    // a typewriter instead of appearing all at once.
    const reveal = summaryRevealRef.current;
    reveal.target = "";
    reveal.displayed = "";
    if (reveal.frame) cancelAnimationFrame(reveal.frame);
    reveal.frame = 0;
    const pump = () => {
      reveal.frame = 0;
      if (reveal.displayed !== reveal.target) {
        reveal.displayed = revealNext(reveal.displayed, reveal.target);
        setMessage(reveal.displayed);
      }
      if (reveal.displayed !== reveal.target) reveal.frame = requestAnimationFrame(pump);
    };
    const kick = () => {
      if (!reveal.frame && reveal.displayed !== reveal.target) reveal.frame = requestAnimationFrame(pump);
    };
    const pushTarget = (value: string) => {
      reveal.target = value;
      kick();
    };

    let failure: string | null = null;
    try {
      const res = await fetch("/api/git/commit-message", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ cwd, locale, ...(sessionId ? { sessionId } : {}) }),
      });
      if (!res.ok || !res.body) {
        throw await readApiJson<never>(res, { hint: t("git.serverErrorHint") });
      }
      // The route streams newline-delimited JSON: "update" events carry the
      // message so far, "done" the authoritative result.
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffered = "";
      let finalMessage: string | null = null;
      let streamError: string | null = null;
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buffered += decoder.decode(value, { stream: true });
        let newline = buffered.indexOf("\n");
        while (newline >= 0) {
          const line = buffered.slice(0, newline).trim();
          buffered = buffered.slice(newline + 1);
          newline = buffered.indexOf("\n");
          if (!line) continue;
          const event = JSON.parse(line) as { type?: string; message?: string; error?: string };
          if (event.type === "update" && event.message) pushTarget(event.message);
          else if (event.type === "done" && event.message) finalMessage = event.message;
          else if (event.type === "error") streamError = event.error ?? "The model did not return a commit message";
        }
      }
      if (streamError) throw new Error(streamError);
      if (!finalMessage) throw new Error("The model did not return a commit message");
      pushTarget(finalMessage);
    } catch (reason) {
      failure = reason instanceof Error ? reason.message : String(reason);
    } finally {
      kick();
      await new Promise<void>((resolve) => {
        const wait = () => (reveal.displayed === reveal.target ? resolve() : setTimeout(wait, 24));
        wait();
      });
      if (failure) setError(failure);
      setBusy(null);
    }
  }, [cwd, locale, sessionId, t]);

  const openDiff = useCallback((file: GitFileStatus) => {
    onOpenFile(file.filePath, fileName(file.filePath), { modeHint: "diff" });
  }, [onOpenFile]);

  const staged = summary?.changes.files.filter((file) => file.indexStatus !== " " && file.indexStatus !== "?") ?? [];
  const unstaged = summary?.changes.files.filter((file) => file.worktreeStatus !== " " || file.indexStatus === "?") ?? [];
  const untrackedPaths = summary?.untrackedPaths ?? [];
  // "Discard all" only restores tracked files from the index, so an untracked-only
  // worktree would leave the button visible but inert. Gate it on real worktree
  // changes (untracked files are never discarded and stay out of this set).
  const hasDiscardableChanges = unstaged.some((file) => file.status !== "untracked");
  const conflictCount = summary?.changes.files.filter((file) => file.status === "conflict").length ?? 0;
  const mergeBranches = summary?.branches.filter((branch) => branch !== summary.branch) ?? [];
  const disabled = busy !== null;
  const needsHttpsCredential = summary?.credentialKind === "https";
  const needsSshCredential = summary?.credentialKind === "ssh";
  // ssh reports every authentication failure — a passphrase-protected key that
  // the askpass broker could not unlock, a key the server refused, an agent
  // without identities — as the same "Permission denied (publickey)" line, so
  // the raw stderr alone never tells the user what to do next.
  const sshAuthFailed = needsSshCredential && /permission denied \(publickey\)/i.test(error ?? "");

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !disabled) {
        event.preventDefault();
        event.stopPropagation();
        if (confirmingDiscardAll) {
          setConfirmingDiscardAll(false);
        } else if (confirmingDiscardFile) {
          setConfirmingDiscardFile(null);
        } else if (editingRemote) {
          setEditingRemote(false);
          setRemoteDraft("");
        } else {
          onClose();
        }
      }
    };
    document.addEventListener("keydown", handleKeyDown, true);
    return () => document.removeEventListener("keydown", handleKeyDown, true);
  }, [confirmingDiscardAll, confirmingDiscardFile, disabled, editingRemote, onClose]);

  return (
    <div role="presentation" className="git-panel-overlay">
      <section role="dialog" aria-label={t("common.git")} className="git-panel-dialog" style={{ position: "relative", width: 460, maxWidth: "100vw", height: "100%", display: "flex", flexDirection: "column", background: "var(--bg)", borderLeft: "1px solid var(--border)", boxShadow: "-14px 0 32px rgba(0,0,0,.16)" }}>
        <header style={{ height: "calc(36px + env(safe-area-inset-top))", padding: "env(safe-area-inset-top) 14px 0", display: "flex", alignItems: "center", gap: 10, flexShrink: 0, borderBottom: "1px solid var(--border)", background: "var(--bg-panel)" }}>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><circle cx="6" cy="6" r="3" /><circle cx="18" cy="18" r="3" /><path d="M6 9v6a3 3 0 0 0 3 3h6" /><path d="M18 15V9a3 3 0 0 0-3-3H9" /></svg>
          <strong style={{ fontSize: 14 }}>{t("common.git")}</strong>
          <span style={{ minWidth: 0, flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", color: "var(--text-muted)", fontFamily: "var(--font-mono)", fontSize: 11 }}>{summary?.repositoryRoot ?? cwd}</span>
          <button type="button" onClick={() => void refresh().catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)))} disabled={disabled} title={t("git.refreshStatus")} aria-label={t("git.refreshStatus")} style={iconButtonStyle} onMouseEnter={(event) => { if (!disabled) event.currentTarget.style.background = "var(--bg-hover)"; }} onMouseLeave={(event) => { event.currentTarget.style.background = "transparent"; }}><span aria-hidden="true">↻</span></button>
          <button type="button" onClick={onClose} disabled={disabled} title={t("git.close")} aria-label={t("git.close")} style={iconButtonStyle} onMouseEnter={(event) => { if (!disabled) event.currentTarget.style.background = "var(--bg-hover)"; }} onMouseLeave={(event) => { event.currentTarget.style.background = "transparent"; }}><span aria-hidden="true">×</span></button>
        </header>
        <main style={{ flex: 1, minHeight: 0, overflowY: "auto", padding: 14 }}>
          {error && <div role="alert" style={{ marginBottom: 12, padding: "8px 10px", border: "1px solid rgba(248,113,113,.45)", background: "rgba(248,113,113,.08)", color: "#ef4444", fontSize: 12, lineHeight: 1.45, overflowWrap: "anywhere" }}>{error}{sshAuthFailed && <div style={{ marginTop: 6, color: "var(--text-muted)" }}>{t("git.sshKeyAuthFailed")}</div>}</div>}
          {!summary && !error && <div style={{ color: "var(--text-muted)", fontSize: 12 }}>{t("git.loadingRepository")}</div>}
          {summary && !summary.isGitRepository && <div style={{ color: "var(--text-muted)", fontSize: 12 }}>{t("git.notRepository")}</div>}
          {summary?.isGitRepository && <>
            <div style={summaryStyle}>
              <span style={{ color: "var(--text-muted)", fontSize: 11 }}>{t("git.branch")}</span><strong style={{ fontFamily: "var(--font-mono)", fontSize: 12 }}>{summary.branch ?? t("git.detachedHead")}</strong>
              <span style={{ color: "var(--text-muted)", fontSize: 11 }}>{t("git.sync")}</span><span>{summary.upstream ? <SyncIndicator ahead={summary.ahead} behind={summary.behind} /> : <span style={{ fontFamily: "var(--font-mono)", fontSize: 12 }}>{t("git.noUpstream")}</span>}</span>
            </div>
            {summary.upstream && summary.behind > 0 && !checkedUpdate && (
              <div style={{ marginTop: 10, padding: "9px 10px", border: "1px solid rgba(74,222,128,.5)", background: "rgba(74,222,128,.08)", color: "#16a34a", fontSize: 12, display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8 }}>
                <span>{t("git.availableUpdate", { count: summary.behind })}</span>
                <ActionButton label={t("git.updateNow")} action="pull" busy={busy} onClick={() => { setCheckedUpdate(true); void run("pull", { rebase: pullRebase }); }} />
              </div>
            )}
            {summary.operation && <div style={{ marginTop: 10, padding: "9px 10px", border: "1px solid rgba(214,168,75,.5)", background: "rgba(214,168,75,.10)", color: "var(--text)", fontSize: 12 }}>{t("git.operationInProgress", { operation: t(`git.operation.${summary.operation}`) })}{conflictCount ? ` ${t("git.operationConflicts", { count: conflictCount })}` : ""}<div style={{ display: "flex", gap: 6, marginTop: 8 }}><ActionButton label={t("git.continue")} busy={busy} action="continue" onClick={() => void run("continue")} /><ActionButton label={t("git.abort")} busy={busy} action="abort" danger onClick={() => void run("abort")} /></div></div>}
            <div style={sectionStyle}><SectionTitle title={t("git.changesSummary", { additions: summary.changes.additions, deletions: summary.changes.deletions })} action={<div style={{ display: "flex", gap: 6 }}>{unstaged.length > 0 && <ActionButton label={t("git.stageAll")} action="stage" busy={busy} onClick={() => void run("stage", { paths: unstaged.map((file) => file.filePath) })} />}{summary?.changes.files.length > 0 && <ActionButton label={t("git.discardAll")} action="discard_all" busy={busy} danger disabled={conflictCount > 0} title={t("git.discardAllTitle")} onClick={() => setConfirmingDiscardAll(true)} />}</div>} /><FileList files={unstaged} empty={t("git.noUnstagedChanges")} busy={busy} onOpenDiff={openDiff} onStage={(filePath) => void run("stage", { paths: [filePath] })} onDiscard={(file) => setConfirmingDiscardFile(file.filePath)} /></div>
            <div style={sectionStyle}><SectionTitle title={t("git.stagedSummary", { count: staged.length })} action={staged.length > 0 ? <ActionButton label={t("git.unstageAll")} action="unstage" busy={busy} onClick={() => void run("unstage", { paths: staged.map((file) => file.filePath) })} /> : undefined} /><FileList files={staged} empty={t("git.nothingStaged")} busy={busy} onOpenDiff={openDiff} onUnstage={(filePath) => void run("unstage", { paths: [filePath] })} onDiscard={(file) => setConfirmingDiscardFile(file.filePath)} /></div>
            <div style={sectionStyle}><SectionTitle title={t("git.commit")} /><textarea value={message} onChange={(event) => setMessage(event.target.value)} placeholder={t("git.commitMessage")} disabled={disabled} rows={7} style={{ width: "100%", resize: "vertical", padding: 8, border: "1px solid var(--border)", borderRadius: 5, background: "var(--bg)", color: "var(--text)", fontFamily: "inherit", fontSize: 12, lineHeight: 1.5 }} /><div style={{ marginTop: 7, display: "flex", justifyContent: "flex-end", gap: 6 }}><ActionButton label={t("git.summarizeCommit")} action="summarize" busy={busy} disabled={staged.length === 0 || conflictCount > 0} title={t("git.summarizeCommitTitle")} onClick={() => void summarizeCommitMessage()} /><ActionButton label={t("git.commitStaged")} action="commit" busy={busy} disabled={!message.trim() || staged.length === 0 || conflictCount > 0} onClick={() => void run("commit", { message })} /></div></div>
            <div style={sectionStyle}>
              <SectionTitle title={t("git.remote")} />
              {summary.remote && !editingRemote && <div style={{ display: "flex", alignItems: "center", gap: 6, minHeight: 30, marginBottom: 9 }}><div style={{ minWidth: 0, flex: 1, color: "var(--text-muted)", fontFamily: "var(--font-mono)", fontSize: 11, lineHeight: 1.45, overflowWrap: "anywhere" }}>{summary.remote}</div><ActionButton label={t("git.editRemote")} action="set_remote_url" busy={busy} onClick={() => { setRemoteDraft(summary.remote ?? ""); setEditingRemote(true); }} /></div>}
              {summary.remote && editingRemote && <form onSubmit={(event) => { event.preventDefault(); if (remoteDraft.trim()) void run("set_remote_url", { remoteUrl: remoteDraft }); }} style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 10 }}><input value={remoteDraft} onChange={(event) => setRemoteDraft(event.target.value)} placeholder={t("git.remoteUrl")} title={t("git.remoteUrl")} autoFocus disabled={disabled} style={{ ...credentialInputStyle, minWidth: 0, flex: 1 }} /><ActionButton label={t("git.cancelEditRemote")} action="set_remote_url" busy={busy} onClick={() => { setEditingRemote(false); setRemoteDraft(""); }} /><ActionButton label={t("git.saveRemote")} action="set_remote_url" busy={busy} disabled={!remoteDraft.trim()} onClick={() => void run("set_remote_url", { remoteUrl: remoteDraft })} /></form>}
              {(needsHttpsCredential || needsSshCredential) && <div style={{ display: "grid", gap: 7, marginBottom: 10, padding: 10, border: "1px solid var(--border)", borderRadius: 6, background: "var(--bg-panel)" }}>
                <div style={{ color: "var(--text-muted)", fontSize: 11 }}>{t(needsHttpsCredential ? "git.httpsCredentials" : "git.sshCredentials")}{summary.hasSavedCredential ? ` · ${t("git.savedCredential")}` : ""}</div>
                {needsHttpsCredential && <input value={username} onChange={(event) => setUsername(event.target.value)} placeholder={t("git.username")} autoComplete="username" disabled={disabled} style={credentialInputStyle} />}
                <input type="password" value={secret} onFocus={() => { if (secret === SAVED_CREDENTIAL_MASK) setSecret(""); }} onChange={(event) => setSecret(event.target.value)} placeholder={t(needsHttpsCredential ? "git.passwordOrToken" : "git.keyPassphrase")} autoComplete="current-password" disabled={disabled} style={credentialInputStyle} />
                <label style={{ display: "flex", alignItems: "center", gap: 5, color: "var(--text-muted)", fontSize: 11 }}><input type="checkbox" checked={rememberCredential} onChange={(event) => setRememberCredential(event.target.checked)} disabled={disabled} /> {t("git.rememberCredential")}</label>
              </div>}
              {summary.credentialKind === "none" && summary.remote?.startsWith("git://") && <div style={{ marginBottom: 10, color: "var(--text-muted)", fontSize: 11 }}>{t("git.nativeProtocolAnonymous")}</div>}
              <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}><ActionButton label={t("git.fetch")} action="fetch" busy={busy} onClick={() => void run("fetch")} /><ActionButton label={t(pullRebase ? "git.pullRebase" : "git.pullMerge")} action="pull" busy={busy} onClick={() => void run("pull", { rebase: pullRebase })} /><ActionButton label={t("git.push")} action="push" busy={busy} onClick={() => void run("push")} /><label style={{ display: "flex", alignItems: "center", gap: 5, color: "var(--text-muted)", fontSize: 11 }}><input type="checkbox" checked={pullRebase} onChange={(event) => setPullRebase(event.target.checked)} disabled={disabled} /> {t("git.rebaseWhenPulling")}</label></div>
            </div>
            <div style={sectionStyle}><SectionTitle title={t("git.mergeBranch")} /><div style={{ display: "flex", gap: 6 }}><MergeBranchPicker branches={mergeBranches} value={mergeBranch} disabled={disabled} onChange={setMergeBranch} /><ActionButton label={t("git.merge")} action="merge" busy={busy} disabled={!mergeBranch || mergeBranches.length === 0} onClick={() => void run("merge", { branch: mergeBranch })} /></div></div>
          </>}
        </main>
        {confirmingDiscardFile && summary?.isGitRepository && <DiscardFileConfirmDialog filePath={confirmingDiscardFile} busy={busy} onCancel={() => setConfirmingDiscardFile(null)} onConfirm={() => { const filePath = confirmingDiscardFile; setConfirmingDiscardFile(null); void run("discard", { paths: [filePath] }); }} />}
        {confirmingDiscardAll && summary?.isGitRepository && <DiscardAllConfirmDialog summary={summary} untrackedPaths={untrackedPaths} busy={busy} onCancel={() => setConfirmingDiscardAll(false)} onConfirm={() => { setConfirmingDiscardAll(false); void run("discard_all"); }} />}
      </section>
    </div>
  );
}

const iconButtonStyle: CSSProperties = { width: 28, height: 28, padding: 0, border: "none", borderRadius: 5, background: "transparent", color: "var(--text-muted)", cursor: "pointer", fontSize: 18, lineHeight: 1, transition: "background .12s, color .12s" };
const sectionStyle: CSSProperties = { marginTop: 18, paddingTop: 14, borderTop: "1px solid var(--border)" };
const summaryStyle: CSSProperties = { display: "grid", gridTemplateColumns: "58px minmax(0, 1fr)", rowGap: 7, alignItems: "center", padding: "10px", background: "var(--bg-panel)", border: "1px solid var(--border)", borderRadius: 6 };

function SyncIndicator({ ahead, behind }: { ahead: number; behind: number }) {
  const { t } = useI18n();
  const chipStyle = (active: boolean): CSSProperties => ({
    display: "inline-flex",
    alignItems: "center",
    gap: 4,
    minWidth: 36,
    height: 24,
    padding: "0 7px",
    border: "1px solid var(--border)",
    borderRadius: 999,
    background: "var(--bg)",
    color: active ? "currentColor" : "var(--text-dim)",
    flexShrink: 0,
  });
  const countStyle: CSSProperties = { fontFamily: "var(--font-mono)", fontSize: 11, lineHeight: 1 };
  return (
    <span style={{ display: "inline-flex", alignItems: "center", gap: 6, flexWrap: "wrap" }}>
      <span role="img" aria-label={t("git.ahead", { count: ahead })} title={t("git.ahead", { count: ahead })} style={ahead > 0 ? { ...chipStyle(true), color: "#60a5fa" } : chipStyle(false)}>
        <ArrowUp size={13} strokeWidth={2.5} aria-hidden="true" />
        <span style={countStyle}>{ahead}</span>
      </span>
      <span role="img" aria-label={t("git.behind", { count: behind })} title={t("git.behind", { count: behind })} style={behind > 0 ? { ...chipStyle(true), color: "#d6a84b" } : chipStyle(false)}>
        <ArrowDown size={13} strokeWidth={2.5} aria-hidden="true" />
        <span style={countStyle}>{behind}</span>
      </span>
    </span>
  );
}
const credentialInputStyle: CSSProperties = { width: "100%", boxSizing: "border-box", minHeight: 30, padding: "5px 7px", border: "1px solid var(--border)", borderRadius: 5, background: "var(--bg)", color: "var(--text)", fontFamily: "inherit", fontSize: 12 };
function SectionTitle({ title, action }: { title: string; action?: ReactNode }) { return <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8, marginBottom: 8, color: "var(--text-muted)", fontSize: 11, fontWeight: 700, letterSpacing: ".04em", textTransform: "uppercase" }}><span>{title}</span>{action}</div>; }
function ActionButton({ label, action, busy, disabled, danger, title, onClick }: { label: string; action: Action; busy: Action | null; disabled?: boolean; danger?: boolean; title?: string; onClick: () => void }) { const { t } = useI18n(); const pending = busy === action; const inactive = Boolean(busy) || disabled; const hoverBackground = danger ? "rgba(248,113,113,.12)" : "var(--bg-hover)"; return <button type="button" title={title} onClick={onClick} disabled={inactive} onMouseEnter={(event) => { if (!inactive) event.currentTarget.style.background = hoverBackground; }} onMouseLeave={(event) => { event.currentTarget.style.background = "transparent"; }} style={{ flexShrink: 0, height: 30, padding: "0 10px", border: "none", borderRadius: 5, background: "transparent", color: danger ? "#ef4444" : "var(--text)", cursor: inactive ? "not-allowed" : "pointer", opacity: inactive ? .58 : 1, fontSize: 11, fontWeight: 600, lineHeight: 1, whiteSpace: "nowrap", transition: "background .12s" }}>{pending ? t("git.working") : label}</button>; }
function FileList({ files, empty, busy, onOpenDiff, onStage, onUnstage, onDiscard }: { files: GitFileStatus[]; empty: string; busy: Action | null; onOpenDiff: (file: GitFileStatus) => void; onStage?: (filePath: string) => void; onUnstage?: (filePath: string) => void; onDiscard?: (file: GitFileStatus) => void }) { const { t } = useI18n(); if (!files.length) return <div style={{ color: "var(--text-dim)", fontSize: 12 }}>{empty}</div>; return <div style={{ height: 220, border: "1px solid var(--border)", borderRadius: 6, overflowY: "auto" }}>{files.map((file) => <div key={`${file.filePath}:${file.indexStatus}:${file.worktreeStatus}`} style={{ minHeight: 35, padding: "5px 7px", display: "flex", alignItems: "center", gap: 7, borderBottom: "1px solid var(--border)" }}><button type="button" onClick={() => onOpenDiff(file)} disabled={Boolean(busy)} title={file.filePath} style={{ minWidth: 0, flex: 1, display: "flex", alignItems: "center", gap: 7, padding: 0, border: "none", background: "transparent", color: "var(--text)", cursor: busy ? "not-allowed" : "pointer", textAlign: "left" }} onMouseEnter={(event) => { if (!busy) event.currentTarget.style.color = "var(--accent)"; }} onMouseLeave={(event) => { event.currentTarget.style.color = "var(--text)"; }}><span style={{ width: 14, flexShrink: 0, color: STATUS_COLOR[file.status], fontFamily: "var(--font-mono)", fontSize: 11, fontWeight: 700 }}>{file.code}</span><span style={{ minWidth: 0, flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontFamily: "var(--font-mono)", fontSize: 11 }}>{fileName(file.filePath)}</span></button>{onStage && <ActionButton label={t("git.stage")} action="stage" busy={busy} onClick={() => onStage(file.filePath)} />}{onUnstage && <ActionButton label={t("git.unstage")} action="unstage" busy={busy} onClick={() => onUnstage(file.filePath)} />}{onDiscard && file.status !== "untracked" && <ActionButton label={t("git.discard")} action="discard" busy={busy} danger onClick={() => onDiscard(file)} />}</div>)}</div>; }

function DiscardFileConfirmDialog({ filePath, busy, onCancel, onConfirm }: { filePath: string; busy: Action | null; onCancel: () => void; onConfirm: () => void }) {
  const { t } = useI18n();
  return (
    <div role="alertdialog" aria-label={t("git.discardFileConfirm", { file: fileName(filePath) })} style={{ position: "absolute", left: 14, right: 14, bottom: 14, zIndex: 5, padding: 14, display: "grid", gap: 10, border: "1px solid rgba(248,113,113,.45)", borderRadius: 6, background: "var(--bg-panel)", boxShadow: "0 8px 24px rgba(0,0,0,.18)" }}>
      <p style={{ margin: 0, color: "var(--text)", fontSize: 12, lineHeight: 1.5 }}>{t("git.discardFileConfirm", { file: fileName(filePath) })}</p>
      <div style={{ display: "flex", gap: 10, justifyContent: "flex-end" }}>
        <ActionButton label={t("git.discardAllConfirmCancel")} action="discard" busy={busy} onClick={onCancel} />
        <ActionButton label={t("git.discard")} action="discard" busy={busy} danger onClick={onConfirm} />
      </div>
    </div>
  );
}

/**
 * Confirmation shown before `discard_all`.
 *
 * Untracked files are deleted by this action and cannot be recovered, so the
 * dialog lists them by name and spells out that they are removed from disk.
 */
function DiscardAllConfirmDialog({ summary, untrackedPaths, busy, onCancel, onConfirm }: { summary: GitSummary; untrackedPaths: string[]; busy: Action | null; onCancel: () => void; onConfirm: () => void }) {
  const { t } = useI18n();
  const snapshot = useRef(untrackedPaths).current;
  const shown = snapshot.slice(0, 8);
  return (
    <div role="alertdialog" aria-label={t("git.discardAllConfirm")} style={{ padding: 14, display: "grid", gap: 10 }}>
      <p style={{ margin: 0, color: "var(--text)", fontSize: 12, lineHeight: 1.5 }}>{t("git.discardAllConfirm")}</p>
      <div style={{ display: "grid", gap: 5, padding: 9, border: "1px solid var(--border)", borderRadius: 6, background: "var(--bg-panel)", fontSize: 11, color: "var(--text-muted)" }}>
        <div>{t("git.discardAllTracked", { count: summary.changes.files.length - snapshot.length })}</div>
        <div>{snapshot.length > 0 ? t("git.discardAllUntracked", { count: snapshot.length }) : t("git.discardAllNoUntracked")}</div>
        {shown.length > 0 && <ul style={{ margin: 0, padding: "4px 0 0 16px", fontFamily: "var(--font-mono)", lineHeight: 1.6, overflowWrap: "anywhere" }}>{shown.map((filePath) => <li key={filePath}>{filePath}</li>)}{snapshot.length > shown.length && <li>{t("git.discardAllMore", { count: snapshot.length - shown.length })}</li>}</ul>}
      </div>
      <div style={{ display: "flex", gap: 10, justifyContent: "flex-end" }}>
        <ActionButton label={t("git.discardAllConfirmCancel")} action="discard_all" busy={busy} onClick={onCancel} />
        <ActionButton label={t("git.discardAllConfirmAction")} action="discard_all" busy={busy} danger onClick={onConfirm} />
      </div>
    </div>
  );
}
function MergeBranchPicker({ branches, value, disabled, onChange }: { branches: string[]; value: string; disabled: boolean; onChange: (branch: string) => void }) {
  return <BranchPicker branches={branches} value={value} disabled={disabled} placement="above" onChange={onChange} />;
}
