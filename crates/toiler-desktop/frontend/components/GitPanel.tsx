"use client";

import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { ArrowDown, ArrowUp, GitBranch, Loader2, Minus, Plus, RefreshCw, Trash2, Undo2, Upload, X } from "lucide-react";
import { useI18n } from "@/hooks/useI18n";
import { readApiJson } from "@/lib/api-json";
import { APPLICATION_VERSION } from "@/lib/changelog";
import { nextTagName } from "@/lib/git-tags";
import { CHAT_TOP_BAR_HEIGHT } from "@/lib/panel-layout";
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
  /// 本平台能不能加密保存凭据。非 macOS 为 false：那里没有系统密钥库，
  /// 勾了「记住」不会真的落盘，勾选框应置灰并说明。
  canRememberCredential: boolean;
  operation: "merge" | "rebase" | "cherry-pick" | "revert" | null;
  branches: string[];
  tags: string[];
  changes: { files: GitFileStatus[]; additions: number; deletions: number };
  untrackedPaths: string[];
};

type Action = "stage" | "unstage" | "discard" | "discard_all" | "commit" | "fetch" | "pull" | "push" | "merge" | "continue" | "abort" | "set_remote_url" | "create_tag" | "delete_tag" | "push_tag" | "push_tags" | "summarize";

const SAVED_CREDENTIAL_MASK = "••••••••";

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
  const [newTag, setNewTag] = useState("");
  const [tagMessage, setTagMessage] = useState("");
  const [confirmingDeleteTag, setConfirmingDeleteTag] = useState<string | null>(null);
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
      // 远程动作都要带上凭据：推标签同样可能遇到 HTTPS token。
      const isRemoteAction = action === "fetch" || action === "pull" || action === "push" || action === "push_tag" || action === "push_tags";
      const credentialKind = summary?.credentialKind;
      const credential = isRemoteAction && secret && secret !== SAVED_CREDENTIAL_MASK && (credentialKind === "https" || credentialKind === "ssh")
        ? { kind: credentialKind, secret, ...(credentialKind === "https" ? { username } : {}) }
        : undefined;
      const res = await fetch("/api/git", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ cwd, action, ...extra, ...(credential ? { credential, rememberCredential } : {}) }) });
      const data = await readApiJson<GitSummary>(res, { hint: t("git.serverErrorHint") });
      setSummary(data);
      if (action === "commit") setMessage("");
      if (action === "create_tag") {
        setNewTag("");
        setTagMessage("");
      }
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
  // 标签名输入框的示例值跟着仓库已有的标签走（建好一个后下次自动往前一步）。
  const tagPlaceholder = t("git.tagName", { name: nextTagName(summary?.tags ?? [], APPLICATION_VERSION) });
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
        } else if (confirmingDeleteTag) {
          setConfirmingDeleteTag(null);
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
  }, [confirmingDiscardAll, confirmingDeleteTag, confirmingDiscardFile, disabled, editingRemote, onClose]);

  return (
    <div role="presentation" className="git-panel-overlay">
      <section role="dialog" aria-label={t("common.git")} className="git-panel-dialog">
        {/* 标题条与对话顶栏同高（CHAT_TOP_BAR_HEIGHT）：它是从右侧抽出来的浮层，
            两条横线的 y 必须对得上，否则并排看就是错位的。高度仍是内联——安全区
            只有内联的 paddingTop 能压住类里的 padding 简写。 */}
        <header
          className="git-panel-header"
          // 标题条可拖窗口：面板贴右侧、头上一整条与对话顶栏同高，鼠标自然
          // 会往那儿落。与 app-topbar 一样用 deep——裸属性只在直接点中这个
          // 元素时生效，而这条带子里铺着图标、标题、路径、按钮。
          data-tauri-drag-region="deep"
          style={{ height: `calc(${CHAT_TOP_BAR_HEIGHT}px + env(safe-area-inset-top))`, paddingTop: "env(safe-area-inset-top)" }}
        >
          <GitBranch size={17} strokeWidth={2} className="git-panel-header-icon" aria-hidden="true" />
          <strong className="git-panel-title">{t("common.git")}</strong>
          <span className="git-panel-path" title={summary?.repositoryRoot ?? cwd}>{summary?.repositoryRoot ?? cwd}</span>
          <button type="button" className="git-icon-btn" onClick={() => void refresh().catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)))} disabled={disabled} title={t("git.refreshStatus")} aria-label={t("git.refreshStatus")}><RefreshCw size={14} strokeWidth={2} aria-hidden="true" /></button>
          <button type="button" className="git-icon-btn" onClick={onClose} disabled={disabled} title={t("git.close")} aria-label={t("git.close")}><X size={15} strokeWidth={2} aria-hidden="true" /></button>
        </header>
        <main className="git-panel-body">
          {error && <div role="alert" className="git-alert">{error}{sshAuthFailed && <div className="git-alert-detail">{t("git.sshKeyAuthFailed")}</div>}</div>}
          {!summary && !error && <div className="git-hint">{t("git.loadingRepository")}</div>}
          {summary && !summary.isGitRepository && <div className="git-hint">{t("git.notRepository")}</div>}
          {summary?.isGitRepository && <>
            <div className="git-summary">
              <span className="git-summary-label">{t("git.branch")}</span>
              <strong className="git-summary-value" title={summary.branch ?? undefined}>{summary.branch ?? t("git.detachedHead")}</strong>
              <span className="git-summary-label">{t("git.sync")}</span>
              <span>{summary.upstream ? <SyncIndicator ahead={summary.ahead} behind={summary.behind} /> : <span className="git-summary-note">{t("git.noUpstream")}</span>}</span>
            </div>
            {summary.upstream && summary.behind > 0 && !checkedUpdate && (
              <div className="git-note">
                <span className="git-note-text">{t("git.availableUpdate", { count: summary.behind })}</span>
                <ActionButton label={t("git.updateNow")} action="pull" busy={busy} onClick={() => { setCheckedUpdate(true); void run("pull", { rebase: pullRebase }); }} />
              </div>
            )}
            {summary.operation && <div className="git-note" data-tone="warn"><span className="git-note-text">{t("git.operationInProgress", { operation: t(`git.operation.${summary.operation}`) })}{conflictCount ? ` ${t("git.operationConflicts", { count: conflictCount })}` : ""}</span><div className="git-note-actions"><ActionButton label={t("git.continue")} busy={busy} action="continue" onClick={() => void run("continue")} /><ActionButton label={t("git.abort")} busy={busy} action="abort" danger onClick={() => void run("abort")} /></div></div>}
            <div className="git-section">
              <SectionTitle title={t("git.changesSummary", { additions: summary.changes.additions, deletions: summary.changes.deletions })} action={<>{unstaged.length > 0 && <ActionButton label={t("git.stageAll")} action="stage" busy={busy} onClick={() => void run("stage", { paths: unstaged.map((file) => file.filePath) })} />}{summary?.changes.files.length > 0 && <ActionButton label={t("git.discardAll")} action="discard_all" busy={busy} danger disabled={conflictCount > 0} title={t("git.discardAllTitle")} onClick={() => setConfirmingDiscardAll(true)} />}</>} />
              <FileList files={unstaged} empty={t("git.noUnstagedChanges")} busy={busy} onOpenDiff={openDiff} onStage={(filePath) => void run("stage", { paths: [filePath] })} onDiscard={(file) => setConfirmingDiscardFile(file.filePath)} />
            </div>
            <div className="git-section">
              <SectionTitle title={t("git.stagedSummary", { count: staged.length })} action={staged.length > 0 ? <ActionButton label={t("git.unstageAll")} action="unstage" busy={busy} onClick={() => void run("unstage", { paths: staged.map((file) => file.filePath) })} /> : undefined} />
              <FileList files={staged} empty={t("git.nothingStaged")} busy={busy} onOpenDiff={openDiff} onUnstage={(filePath) => void run("unstage", { paths: [filePath] })} onDiscard={(file) => setConfirmingDiscardFile(file.filePath)} />
            </div>
            <div className="git-section">
              <SectionTitle title={t("git.commit")} />
              <textarea className="git-textarea" value={message} onChange={(event) => setMessage(event.target.value)} placeholder={t("git.commitMessage")} disabled={disabled} rows={6} />
              <div className="git-commit-actions"><ActionButton label={t("git.summarizeCommit")} action="summarize" busy={busy} disabled={staged.length === 0 || conflictCount > 0} title={t("git.summarizeCommitTitle")} onClick={() => void summarizeCommitMessage()} /><ActionButton label={t("git.commitStaged")} action="commit" busy={busy} disabled={!message.trim() || staged.length === 0 || conflictCount > 0} onClick={() => void run("commit", { message })} /></div>
            </div>
            <div className="git-section">
              <SectionTitle title={t("git.remote")} />
              {summary.remote && !editingRemote && <div className="git-actions-row" style={{ marginBottom: 9 }}><div style={{ minWidth: 0, flex: 1, color: "var(--text-muted)", fontFamily: "var(--font-mono)", fontSize: 11, lineHeight: 1.45, overflowWrap: "anywhere" }}>{summary.remote}</div><ActionButton label={t("git.editRemote")} action="set_remote_url" busy={busy} onClick={() => { setRemoteDraft(summary.remote ?? ""); setEditingRemote(true); }} /></div>}
              {summary.remote && editingRemote && <form onSubmit={(event) => { event.preventDefault(); if (remoteDraft.trim()) void run("set_remote_url", { remoteUrl: remoteDraft }); }} className="git-actions-row" style={{ marginBottom: 10 }}><input className="git-input" value={remoteDraft} onChange={(event) => setRemoteDraft(event.target.value)} placeholder={t("git.remoteUrl")} title={t("git.remoteUrl")} autoFocus disabled={disabled} style={{ minWidth: 0, flex: 1 }} /><ActionButton label={t("git.cancelEditRemote")} action="set_remote_url" busy={busy} onClick={() => { setEditingRemote(false); setRemoteDraft(""); }} /><ActionButton label={t("git.saveRemote")} action="set_remote_url" busy={busy} disabled={!remoteDraft.trim()} onClick={() => void run("set_remote_url", { remoteUrl: remoteDraft })} /></form>}
              {(needsHttpsCredential || needsSshCredential) && <div className="git-field">
                <div className="git-field-label">{t(needsHttpsCredential ? "git.httpsCredentials" : "git.sshCredentials")}{summary.hasSavedCredential ? ` · ${t("git.savedCredential")}` : ""}</div>
                {needsHttpsCredential && <input className="git-input" value={username} onChange={(event) => setUsername(event.target.value)} placeholder={t("git.username")} autoComplete="username" disabled={disabled} />}
                <input className="git-input" type="password" value={secret} onFocus={() => { if (secret === SAVED_CREDENTIAL_MASK) setSecret(""); }} onChange={(event) => setSecret(event.target.value)} placeholder={t(needsHttpsCredential ? "git.passwordOrToken" : "git.keyPassphrase")} autoComplete="current-password" disabled={disabled} />
                <label className="git-check"><input type="checkbox" checked={rememberCredential} onChange={(event) => setRememberCredential(event.target.checked)} disabled={disabled || !summary.canRememberCredential} /> {t("git.rememberCredential")}</label>
                {/* 非 macOS 没有系统密钥库（见 `git_credentials` 模块），勾了也不会
                    落盘。置灰 + 说明，比让用户勾一个永远不生效的框诚实。 */}
                {!summary.canRememberCredential && <div className="git-hint">{t("git.rememberCredentialUnavailable")}</div>}
              </div>}
              {summary.credentialKind === "none" && summary.remote?.startsWith("git://") && <div className="git-hint" style={{ marginBottom: 10 }}>{t("git.nativeProtocolAnonymous")}</div>}
              <div className="git-actions-row"><ActionButton label={t("git.fetch")} action="fetch" busy={busy} onClick={() => void run("fetch")} /><ActionButton label={t(pullRebase ? "git.pullRebase" : "git.pullMerge")} action="pull" busy={busy} onClick={() => void run("pull", { rebase: pullRebase })} /><ActionButton label={t("git.push")} action="push" busy={busy} onClick={() => void run("push")} /><label className="git-check"><input type="checkbox" checked={pullRebase} onChange={(event) => setPullRebase(event.target.checked)} disabled={disabled} /> {t("git.rebaseWhenPulling")}</label></div>
            </div>
            <div className="git-section"><SectionTitle title={t("git.mergeBranch")} /><div className="git-actions-row"><MergeBranchPicker branches={mergeBranches} value={mergeBranch} disabled={disabled} onChange={setMergeBranch} /><ActionButton label={t("git.merge")} action="merge" busy={busy} disabled={!mergeBranch || mergeBranches.length === 0} onClick={() => void run("merge", { branch: mergeBranch })} /></div></div>
            {/* 标签：本地列表 / 创建（带附注则建附注 tag）/ 删除本地 / 推到远端。
                推送需要 origin，所以没配 remote 时两处推送按钮都是 disabled，
                而不是点下去才吃一个 `'origin' does not appear to be a repository`。 */}
            <div className="git-section">
              <SectionTitle title={t("git.tags")} action={summary.tags.length > 0 ? <><span className="git-summary-note">{t("git.tagCount", { count: summary.tags.length })}</span><ActionButton label={t("git.pushAllTags")} action="push_tags" busy={busy} disabled={!summary.remote} onClick={() => void run("push_tags")} /></> : undefined} />
              <div className="git-field">
                <input className="git-input" value={newTag} onChange={(event) => setNewTag(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && newTag.trim() && !disabled) void run("create_tag", { tag: newTag.trim(), ...(tagMessage.trim() ? { message: tagMessage.trim() } : {}) }); }} placeholder={tagPlaceholder} title={tagPlaceholder} disabled={disabled} />
                <input className="git-input" value={tagMessage} onChange={(event) => setTagMessage(event.target.value)} placeholder={t("git.tagMessage")} title={t("git.tagMessage")} disabled={disabled} />
              </div>
              <div className="git-actions-row" style={{ marginBottom: summary.tags.length > 0 ? 9 : 0 }}>
                <ActionButton label={t("git.createTag")} action="create_tag" busy={busy} disabled={!newTag.trim()} onClick={() => void run("create_tag", { tag: newTag.trim(), ...(tagMessage.trim() ? { message: tagMessage.trim() } : {}) })} />
              </div>
              {summary.tags.length === 0
                ? <div className="git-hint">{t("git.noTags")}</div>
                : <div className="git-file-list">{summary.tags.map((tag) => (
                  <div key={tag} className="git-file-row">
                    <span className="git-tag-name" title={tag}>{tag}</span>
                    <FileAction label={t("git.pushTag")} icon={Upload} action="push_tag" busy={busy} disabled={!summary.remote} onClick={() => void run("push_tag", { tag })} />
                    <FileAction label={t("git.deleteTag")} icon={Trash2} action="delete_tag" busy={busy} danger onClick={() => setConfirmingDeleteTag(tag)} />
                  </div>
                ))}</div>}
            </div>
          </>}
        </main>
        {confirmingDiscardFile && summary?.isGitRepository && <DiscardFileConfirmDialog filePath={confirmingDiscardFile} busy={busy} onCancel={() => setConfirmingDiscardFile(null)} onConfirm={() => { const filePath = confirmingDiscardFile; setConfirmingDiscardFile(null); void run("discard", { paths: [filePath] }); }} />}
        {confirmingDiscardAll && summary?.isGitRepository && <DiscardAllConfirmDialog summary={summary} untrackedPaths={untrackedPaths} busy={busy} onCancel={() => setConfirmingDiscardAll(false)} onConfirm={() => { setConfirmingDiscardAll(false); void run("discard_all"); }} />}
        {confirmingDeleteTag && summary?.isGitRepository && <DeleteTagConfirmDialog tag={confirmingDeleteTag} busy={busy} onCancel={() => setConfirmingDeleteTag(null)} onConfirm={() => { const tag = confirmingDeleteTag; setConfirmingDeleteTag(null); void run("delete_tag", { tag }); }} />}
      </section>
    </div>
  );
}

function SyncIndicator({ ahead, behind }: { ahead: number; behind: number }) {
  const { t } = useI18n();
  return (
    <span className="git-sync">
      <span role="img" aria-label={t("git.ahead", { count: ahead })} title={t("git.ahead", { count: ahead })} className="git-sync-chip" data-tone="ahead" data-active={ahead > 0}>
        <ArrowUp size={12} strokeWidth={2.5} aria-hidden="true" />
        <span>{ahead}</span>
      </span>
      <span role="img" aria-label={t("git.behind", { count: behind })} title={t("git.behind", { count: behind })} className="git-sync-chip" data-tone="behind" data-active={behind > 0}>
        <ArrowDown size={12} strokeWidth={2.5} aria-hidden="true" />
        <span>{behind}</span>
      </span>
    </span>
  );
}
function SectionTitle({ title, action }: { title: string; action?: ReactNode }) {
  return <div className="git-section-title"><span className="git-section-title-text">{title}</span>{action && <div className="git-section-actions">{action}</div>}</div>;
}

/** 文本按钮：待执行的动作在原地显示「处理中」，其余情况显示 label。 */
function ActionButton({ label, action, busy, disabled, danger, title, onClick }: { label: string; action: Action; busy: Action | null; disabled?: boolean; danger?: boolean; title?: string; onClick: () => void }) {
  const { t } = useI18n();
  const pending = busy === action;
  return <button type="button" className="git-btn" data-tone={danger ? "danger" : undefined} title={title} onClick={onClick} disabled={Boolean(busy) || disabled}>{pending ? t("git.working") : label}</button>;
}

/** 文件行的图标按钮：常驻（不靠 hover 淡入）、尺寸固定，正在跑的动作转圈。 */
function FileAction({ label, icon: Icon, action, busy, disabled, danger, onClick }: { label: string; icon: typeof Plus; action: Action; busy: Action | null; disabled?: boolean; danger?: boolean; onClick: () => void }) {
  const pending = busy === action;
  return (
    <button type="button" className="git-icon-btn" data-tone={danger ? "danger" : undefined} data-pending={pending ? "true" : undefined} title={label} aria-label={label} onClick={onClick} disabled={Boolean(busy) || disabled}>
      {pending ? <Loader2 size={14} strokeWidth={2} aria-hidden="true" /> : <Icon size={14} strokeWidth={2} aria-hidden="true" />}
    </button>
  );
}

function FileList({ files, empty, busy, onOpenDiff, onStage, onUnstage, onDiscard }: { files: GitFileStatus[]; empty: string; busy: Action | null; onOpenDiff: (file: GitFileStatus) => void; onStage?: (filePath: string) => void; onUnstage?: (filePath: string) => void; onDiscard?: (file: GitFileStatus) => void }) {
  const { t } = useI18n();
  if (!files.length) return <div className="git-hint">{empty}</div>;
  return (
    <div className="git-file-list">
      {files.map((file) => (
        <div key={`${file.filePath}:${file.indexStatus}:${file.worktreeStatus}`} className="git-file-row" data-conflict={file.status === "conflict" ? "true" : undefined}>
          <button type="button" className="git-file-open" onClick={() => onOpenDiff(file)} disabled={Boolean(busy)} title={file.filePath}>
            <span className="git-file-code" data-status={file.status} aria-hidden="true">{file.code}</span>
            <span className="git-file-name">{fileName(file.filePath)}</span>
          </button>
          {onStage && <FileAction label={t("git.stage")} icon={Plus} action="stage" busy={busy} onClick={() => onStage(file.filePath)} />}
          {onUnstage && <FileAction label={t("git.unstage")} icon={Minus} action="unstage" busy={busy} onClick={() => onUnstage(file.filePath)} />}
          {onDiscard && file.status !== "untracked" && <FileAction label={t("git.discard")} icon={Undo2} action="discard" busy={busy} danger onClick={() => onDiscard(file)} />}
        </div>
      ))}
    </div>
  );
}

function DiscardFileConfirmDialog({ filePath, busy, onCancel, onConfirm }: { filePath: string; busy: Action | null; onCancel: () => void; onConfirm: () => void }) {
  const { t } = useI18n();
  return (
    <div role="alertdialog" aria-label={t("git.discardFileConfirm", { file: fileName(filePath) })} className="git-confirm">
      <p className="git-confirm-text">{t("git.discardFileConfirm", { file: fileName(filePath) })}</p>
      <div className="git-confirm-actions">
        <ActionButton label={t("git.discardAllConfirmCancel")} action="discard" busy={busy} onClick={onCancel} />
        <ActionButton label={t("git.discard")} action="discard" busy={busy} danger onClick={onConfirm} />
      </div>
    </div>
  );
}

/**
 * Confirmation shown before `delete_tag`.
 *
 * Tags are cheap to recreate only if the name matters, not the target: the dialog
 * spells out that this deletes the local tag only, so nobody expects the remote
 * one to disappear too.
 */
function DeleteTagConfirmDialog({ tag, busy, onCancel, onConfirm }: { tag: string; busy: Action | null; onCancel: () => void; onConfirm: () => void }) {
  const { t } = useI18n();
  return (
    <div role="alertdialog" aria-label={t("git.deleteTagConfirm", { tag })} className="git-confirm">
      <p className="git-confirm-text">{t("git.deleteTagConfirm", { tag })}</p>
      <p className="git-confirm-note">{t("git.deleteTagLocalOnly")}</p>
      <div className="git-confirm-actions">
        <ActionButton label={t("git.discardAllConfirmCancel")} action="delete_tag" busy={busy} onClick={onCancel} />
        <ActionButton label={t("git.deleteTag")} action="delete_tag" busy={busy} danger onClick={onConfirm} />
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
    <div role="alertdialog" aria-label={t("git.discardAllConfirm")} className="git-confirm-panel">
      <p className="git-confirm-text">{t("git.discardAllConfirm")}</p>
      <div className="git-confirm-list">
        <div>{t("git.discardAllTracked", { count: summary.changes.files.length - snapshot.length })}</div>
        <div>{snapshot.length > 0 ? t("git.discardAllUntracked", { count: snapshot.length }) : t("git.discardAllNoUntracked")}</div>
        {shown.length > 0 && <ul>{shown.map((filePath) => <li key={filePath}>{filePath}</li>)}{snapshot.length > shown.length && <li>{t("git.discardAllMore", { count: snapshot.length - shown.length })}</li>}</ul>}
      </div>
      <div className="git-confirm-actions">
        <ActionButton label={t("git.discardAllConfirmCancel")} action="discard_all" busy={busy} onClick={onCancel} />
        <ActionButton label={t("git.discardAllConfirmAction")} action="discard_all" busy={busy} danger onClick={onConfirm} />
      </div>
    </div>
  );
}
function MergeBranchPicker({ branches, value, disabled, onChange }: { branches: string[]; value: string; disabled: boolean; onChange: (branch: string) => void }) {
  return <BranchPicker branches={branches} value={value} disabled={disabled} placement="above" onChange={onChange} />;
}
