"use client";

import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode, type RefObject } from "react";
import { Loader2, Minus, Plus, RefreshCw, Trash2, Undo2, Upload } from "lucide-react";
import { createPortal } from "react-dom";
import { useI18n } from "@/hooks/useI18n";
import { readApiJson } from "@/lib/api-json";
import { APPLICATION_VERSION } from "@/lib/changelog";
import { nextTagName, sortTagsDescending } from "@/lib/git-tags";
import { isMacPlatform, isWindowsPlatform } from "@/lib/platform-key";
import { revealNext } from "@/lib/typewriter";
import { upgradeProjectVersion, VERSION_UPGRADE_LEVELS, type VersionUpgradeLevel, type VersionUpgradePreview } from "@/lib/version-upgrade";
import type { GitDiffSelection } from "@/lib/git-workspace";
import { GitConflictEditor, type GitConflictEditorHandle } from "./GitConflictEditor";
import { GitPushDialog, type GitPushPreview } from "./GitPushDialog";

type GitFileStatus = {
  filePath: string;
  status: "modified" | "added" | "deleted" | "renamed" | "untracked" | "conflict";
  code: string;
  indexStatus: string;
  worktreeStatus: string;
};

type GitSummary = {
  /// 本机能不能跑 `git --version`。false 时整个面板换成安装提示：
  /// 此时 `isGitRepository` 也是 false，但两者含义完全不同（一个是缺工具，
  /// 一个是目录没初始化），不能共用一句「不是 Git 仓库」。
  gitAvailable?: boolean;
  isGitRepository: boolean;
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
  mergeConflicts?: string[] | null;
  tags: string[];
  changes: { files: GitFileStatus[]; additions: number; deletions: number };
  untrackedPaths: string[];
};

type GitAction = "stage" | "unstage" | "discard" | "discard_all" | "commit" | "fetch" | "pull" | "push" | "continue" | "abort" | "set_remote_url" | "create_tag" | "delete_tag" | "push_tag";
type Action = GitAction | "commit_and_push" | "summarize" | `bump_${VersionUpgradeLevel}`;

function isRemoteAction(action: GitAction) {
  return action === "fetch" || action === "pull" || action === "push" || action === "push_tag";
}

const SAVED_CREDENTIAL_MASK = "••••••••";

type CredentialDraft = { username: string; secret: string; rememberCredential: boolean };
type PullPrompt = { cwd: string; remote: string | null; kind: "https" | "ssh"; rebase: boolean };

type InstallPlatform = "windows" | "mac" | "linux";

/** 各平台的 Git 安装指引。服务端渲染时 `navigator` 不存在，回落到 linux 文案。 */
function currentPlatform(): InstallPlatform {
  if (isWindowsPlatform()) return "windows";
  if (isMacPlatform()) return "mac";
  return "linux";
}

function installHintFor(platform: InstallPlatform): string {
  if (platform === "windows") return "git.missingWindowsHint";
  if (platform === "mac") return "git.missingMacHint";
  return "git.missingLinuxHint";
}

function fileName(filePath: string): string {
  return filePath.split(/[\\/]/).pop() ?? filePath;
}

interface GitPanelProps {
  cwd: string;
  sessionId: string | null;
  refreshTarget: HTMLElement | null;
  refreshButtonClassName: string;
  historyMutation: number;
  onClose: () => void;
  onChanged: () => void;
  onSelectFile: (file: GitDiffSelection) => void;
  selectedFile: GitDiffSelection | null;
  conflictTarget: HTMLDivElement | null;
  conflictActive: boolean;
  conflictEditorRef: RefObject<GitConflictEditorHandle | null>;
  onShowConflict: () => void;
  onHideConflict: () => void;
}

export function GitPanel({ cwd, sessionId, refreshTarget, refreshButtonClassName, historyMutation, onClose, onChanged, onSelectFile, selectedFile,
  conflictTarget, conflictActive, conflictEditorRef, onShowConflict, onHideConflict }: GitPanelProps) {
  const { locale, t } = useI18n();
  const [summary, setSummary] = useState<GitSummary | null>(null);
  const [message, setMessage] = useState("");
  const [pullRebase, setPullRebase] = useState(false);
  const [pullPrompt, setPullPrompt] = useState<PullPrompt | null>(null);
  const [conflict, setConflict] = useState<{ cwd: string; path: string } | null>(null);
  const [pushPrompt, setPushPrompt] = useState<{ cwd: string; committed: boolean } | null>(null);
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
  const [desktop, setDesktop] = useState(false);
  // 首帧服务端渲染没有 `navigator`，先用 linux 文案占位，挂载后立刻改成真实平台。
  // 这段只在 git 不可用时的安装提示里用到，改文案不会影响布局。
  const [platform, setPlatform] = useState<InstallPlatform>("linux");
  const [versionPreview, setVersionPreview] = useState<{ level: VersionUpgradeLevel; data: VersionUpgradePreview } | null>(null);
  const [versionResult, setVersionResult] = useState<{ cwd: string; tag: string } | null>(null);
  const panelRef = useRef<HTMLElement>(null);
  const versionDialogRef = useRef<HTMLDivElement>(null);
  useEffect(() => { panelRef.current?.focus({ preventScroll: true }); }, []);
  useEffect(() => { setDesktop(window.__PI_WEB_DESKTOP__ === true); }, []);
  useEffect(() => { setPlatform(currentPlatform()); }, []);
  useEffect(() => { if (versionPreview) versionDialogRef.current?.focus(); }, [versionPreview]);

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
    const data = await readApiJson<GitSummary>(res, { hint: t("git.serverErrorHint") });
    setSummary(data);
    return data;
  }, [cwd, t]);

  // 一个项目里可以有多个仓库（子项目），从文件浏览器的徽标切换仓库根时先丢掉上一个
  // 仓库的文件列表与错误，否则新目标会先闪一帧旧仓库的内容。
  useEffect(() => {
    setSummary(null);
    setError(null);
    setPullPrompt(null);
    setPushPrompt(null);
    setConflict(null);
    setVersionPreview(null);
    setVersionResult(null);
  }, [cwd]);

  useEffect(() => { void refresh().catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason))); }, [refresh, historyMutation]);

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

  const run = useCallback(async (
    action: GitAction | "commit_and_push",
    extra: Record<string, unknown> = {},
    draft: CredentialDraft | null = { username, secret, rememberCredential },
  ) => {
    setBusy(action);
    setError(null);
    const firstAction = action === "commit_and_push" ? "commit" : action;
    const request = async (gitAction: GitAction, payload: Record<string, unknown> = {}) => {
      const credentialKind = summary?.credentialKind;
      const credential = isRemoteAction(gitAction) && draft?.secret && draft.secret !== SAVED_CREDENTIAL_MASK && (credentialKind === "https" || credentialKind === "ssh")
        ? { kind: credentialKind, secret: draft.secret, ...(credentialKind === "https" ? { username: draft.username } : {}) }
        : undefined;
      const res = await fetch("/api/git", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ cwd, action: gitAction, ...payload, ...(credential && draft ? { credential, rememberCredential: draft.rememberCredential } : {}) }) });
      return readApiJson<GitSummary>(res, { hint: t("git.serverErrorHint") });
    };
    try {
      const data = await request(firstAction, extra);
      if (firstAction === "commit") setMessage("");
      setSummary(data);
      if (action === "commit_and_push") setPushPrompt({ cwd, committed: true });
      // 只在 Git 确认推送成功后清除对应版本标签的提示。
      if (action === "push_tag") {
        setVersionResult((result) => result?.cwd === cwd && extra.tag === result.tag ? null : result);
      }
      if (action === "create_tag") {
        setNewTag("");
        setTagMessage("");
      }
      if (action === "set_remote_url") {
        setEditingRemote(false);
        setRemoteDraft("");
        setSecret("");
        setUsername("");
        setRememberCredential(false);
        prefilledKeyRef.current = null;
      }
      if (isRemoteAction(firstAction) && draft !== null) {
        setSecret(data.hasSavedCredential ? SAVED_CREDENTIAL_MASK : "");
      }
    } catch (reason) {
      const detail = reason instanceof Error ? reason.message : String(reason);
      if (!(action === "push" && extra.previewToken)) {
        setError(detail);
        const current = await refresh().catch(() => undefined);
        if (action === "pull" && current?.operation === "merge" && current.mergeConflicts?.length) {
          setError(null);
          onChanged();
        }
      }
      return detail;
    } finally {
      setBusy(null);
    }
    onChanged();
    return null;
  }, [cwd, onChanged, refresh, rememberCredential, secret, summary?.credentialKind, t, username]);

  const confirmPush = async (preview: GitPushPreview) => {
    const error = await run("push", { previewToken: preview.token }, preview.usesPanelCredential ? undefined : null);
    if (error === null) setPushPrompt(null);
    return error;
  };

  const pull = (rebase: boolean, draft?: CredentialDraft) => {
    setCheckedUpdate(true);
    void run("pull", { rebase }, draft);
  };

  const requestPull = () => {
    if (busy || !summary?.isGitRepository) return;
    const { credentialKind: kind, hasSavedCredential, remote } = summary;
    if (!hasSavedCredential && (kind === "https" || kind === "ssh")) {
      setPullPrompt({ cwd, remote, kind, rebase: pullRebase });
      return;
    }
    pull(pullRebase);
  };

  const versionUpgrade = useCallback(async (level: VersionUpgradeLevel, preview?: VersionUpgradePreview) => {
    setBusy(`bump_${level}`);
    setError(null);
    setVersionResult(null);
    try {
      const data = await upgradeProjectVersion(cwd, level, preview);
      if (!preview) {
        setVersionPreview({ level, data });
      } else {
        setVersionPreview(null);
        setVersionResult({ cwd, tag: data.tag });
        onChanged();
        await refresh();
      }
    } catch (reason) {
      setVersionPreview(null);
      setError(reason instanceof Error ? reason.message : String(reason));
      void refresh().catch(() => undefined);
    } finally {
      setBusy(null);
    }
  }, [cwd, onChanged, refresh, t]);

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

  const activeWorktreeFile = selectedFile?.scope === "unstaged" || selectedFile?.scope === "staged" ? selectedFile : null;

  const mergeConflicts = summary?.operation === "merge" ? summary.mergeConflicts : null;
  const managesConflicts = Array.isArray(mergeConflicts);
  const staged = summary?.changes.files.filter((file) => file.indexStatus !== " " && file.indexStatus !== "?" && (!managesConflicts || file.status !== "conflict")) ?? [];
  const unstaged = summary?.changes.files.filter((file) => (file.worktreeStatus !== " " || file.indexStatus === "?") && (!managesConflicts || file.status !== "conflict")) ?? [];
  const untrackedPaths = summary?.untrackedPaths ?? [];
  const sortedTags = sortTagsDescending(summary?.tags ?? []);
  // 标签名输入框的示例值跟着仓库已有的标签走（建好一个后下次自动往前一步）。
  const tagPlaceholder = t("git.tagName", { name: nextTagName(summary?.tags ?? [], APPLICATION_VERSION) });
  // "Discard all" only restores tracked files from the index, so an untracked-only
  // worktree would leave the button visible but inert. Gate it on real worktree
  // changes (untracked files are never discarded and stay out of this set).
  const hasDiscardableChanges = unstaged.some((file) => file.status !== "untracked");
  const conflictCount = mergeConflicts?.length ?? summary?.changes.files.filter((file) => file.status === "conflict").length ?? 0;
  const canCommit = Boolean(message.trim()) && staged.length > 0 && conflictCount === 0 && summary?.operation !== "merge";
  const disabled = busy !== null;

  function closeConflict() {
    setConflict(null);
    onHideConflict();
  }

  function openConflict(path: string) {
    if (conflict?.path === path) { onShowConflict(); return; }
    const open = () => { setConflict({ cwd, path }); onShowConflict(); };
    if (conflictEditorRef.current) conflictEditorRef.current.requestLeave(open);
    else open();
  }

  function abortOperation() {
    const abort = () => { void run("abort").then((error) => { if (error === null) closeConflict(); }); };
    if (conflictEditorRef.current) conflictEditorRef.current.requestLeave(abort);
    else abort();
  }

  async function conflictResolved(response: Response) {
    const data = await readApiJson<GitSummary>(response, { hint: t("git.serverErrorHint") });
    setSummary(data);
    const next = data.mergeConflicts?.[0];
    if (next) {
      setConflict({ cwd, path: next });
      onShowConflict();
    } else closeConflict();
    onChanged();
  }
  const needsHttpsCredential = summary?.credentialKind === "https";
  const needsSshCredential = summary?.credentialKind === "ssh";
  // ssh reports every authentication failure — a passphrase-protected key that
  // the askpass broker could not unlock, a key the server refused, an agent
  // without identities — as the same "Permission denied (publickey)" line, so
  // the raw stderr alone never tells the user what to do next.
  const sshAuthFailed = needsSshCredential && /permission denied \(publickey\)/i.test(error ?? "");

  const refreshFromToolbar = async () => {
    try {
      await refresh();
      onChanged();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key !== "Escape" || event.defaultPrevented || disabled) return;
    event.preventDefault();
    event.stopPropagation();
    if (versionPreview) {
      setVersionPreview(null);
    } else if (confirmingDiscardAll) {
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
  };

  return (
    <section ref={panelRef} tabIndex={-1} aria-label={t("common.git")} className="git-panel" onKeyDown={handleKeyDown}>
        {/* 刷新保持在 Git 操作区的标题栏。 */}
        {refreshTarget && createPortal(
            <button
              type="button"
              className={refreshButtonClassName}
              onClick={() => void refreshFromToolbar()}
              disabled={disabled || Boolean(versionPreview)}
              title={t("git.refreshStatus")}
              aria-label={t("git.refreshStatus")}
            >
              <RefreshCw size={14} strokeWidth={1.8} aria-hidden="true" />
            </button>,
          refreshTarget,
        )}
        <div className="git-panel-body" inert={Boolean(versionPreview)}>
          {error && <div role="alert" className="git-alert">{error}{sshAuthFailed && <div className="git-alert-detail">{t("git.sshKeyAuthFailed")}</div>}</div>}
          {!summary && !error && <GitPanelSkeleton />}
          {summary?.gitAvailable === false && <div className="git-hint" role="alert">
            <strong style={{ display: "block", marginBottom: 6 }}>{t("git.missingTitle")}</strong>
            <span style={{ display: "block" }}>{t("git.missingBody")}</span>
            <span style={{ display: "block", marginTop: 6, color: "var(--text-dim)" }}>
              {t(installHintFor(platform))}
            </span>
          </div>}
          {summary?.gitAvailable !== false && summary && !summary.isGitRepository && <div className="git-hint">{t("git.notRepository")}</div>}
          {summary?.gitAvailable !== false && summary?.isGitRepository && <>
            <div className="git-section">
              <SectionTitle title={t("git.remote")} />
              {summary.remote && !editingRemote && <div className="git-actions-row" style={{ marginBottom: 9, flexWrap: "nowrap" }}>
                <div className="git-remote-url" title={summary.remote} style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", color: "var(--text-dim)", fontFamily: "var(--font-mono, monospace)", fontSize: 12 }}>{summary.remote}</div>
                <div style={{ flexShrink: 0 }}><ActionButton label={t("git.editRemote")} action="set_remote_url" busy={busy} onClick={() => { setRemoteDraft(summary.remote ?? ""); setEditingRemote(true); }} /></div>
              </div>}
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
              <div className="git-actions-row"><ActionButton label={t("git.fetch")} action="fetch" busy={busy} onClick={() => void run("fetch")} /><ActionButton label={t(pullRebase ? "git.pullRebase" : "git.pullMerge")} action="pull" busy={busy} onClick={requestPull} /><ActionButton label={t("git.push")} action="push" busy={busy} onClick={() => setPushPrompt({ cwd, committed: false })} /><label className="git-check"><input type="checkbox" checked={pullRebase} onChange={(event) => setPullRebase(event.target.checked)} disabled={disabled} /> {t("git.rebaseWhenPulling")}</label></div>
            </div>
            {summary.upstream && summary.behind > 0 && !checkedUpdate && (
              <div className="git-note">
                <span className="git-note-text">{t("git.availableUpdate", { count: summary.behind })}</span>
                <ActionButton label={t("git.updateNow")} action="pull" busy={busy} onClick={requestPull} />
              </div>
            )}
            {summary.operation && <div className="git-note" data-tone="warn">
              <span className="git-note-text">{managesConflicts && conflictCount === 0 ? t("gitConflict.allResolved") : <>{t("git.operationInProgress", { operation: t(`git.operation.${summary.operation}`) })}{conflictCount ? ` ${t("git.operationConflicts", { count: conflictCount })}` : ""}</>}</span>
              <div className="git-note-actions"><ActionButton label={t(summary.operation === "merge" ? "gitConflict.continue" : "git.continue")} busy={busy} action="continue" disabled={summary.operation === "merge" && (!managesConflicts || conflictCount > 0)} onClick={() => void run("continue")} /><ActionButton label={t("git.abort")} busy={busy} action="abort" danger onClick={abortOperation} /></div>
            </div>}
            {!!mergeConflicts?.length && <div className="git-section">
              <SectionTitle title={t("gitConflict.remaining", { count: mergeConflicts.length })} />
              <ul className="git-file-list">
                {mergeConflicts.map((path) => <li key={path} className="git-file-row"><button type="button" className="git-file-open" data-conflict-file disabled={disabled} title={path} aria-pressed={conflictActive && conflict?.path === path} onClick={() => openConflict(path)}>
                  <span className="git-file-code" data-status="conflict" aria-hidden="true">C</span><span className="git-file-name">{path}</span>
                </button></li>)}
              </ul>
            </div>}
            <div className="git-section">
              <SectionTitle title={t("git.changesSummary", { additions: summary.changes.additions, deletions: summary.changes.deletions })} action={<>{unstaged.length > 0 && <ActionButton label={t("git.stageAll")} action="stage" busy={busy} onClick={() => void run("stage", { paths: unstaged.map((file) => file.filePath) })} />}{summary?.changes.files.length > 0 && <ActionButton label={t("git.discardAll")} action="discard_all" busy={busy} danger disabled={conflictCount > 0} title={t("git.discardAllTitle")} onClick={() => setConfirmingDiscardAll(true)} />}</>} />
              <FileList files={unstaged} empty={t("git.noUnstagedChanges")} busy={busy} selectedPath={activeWorktreeFile?.scope === "unstaged" ? activeWorktreeFile.path : undefined} onOpenDiff={(file) => onSelectFile({ scope: "unstaged", path: file.filePath })} onStage={(filePath) => void run("stage", { paths: [filePath] })} onDiscard={(file) => setConfirmingDiscardFile(file.filePath)} />
            </div>
            <div className="git-section">
              <SectionTitle title={t("git.stagedSummary", { count: staged.length })} action={staged.length > 0 && summary.operation !== "merge" ? <ActionButton label={t("git.unstageAll")} action="unstage" busy={busy} onClick={() => void run("unstage", { paths: staged.map((file) => file.filePath) })} /> : undefined} />
              <FileList files={staged} empty={t("git.nothingStaged")} busy={busy} selectedPath={activeWorktreeFile?.scope === "staged" ? activeWorktreeFile.path : undefined} onOpenDiff={(file) => onSelectFile({ scope: "staged", path: file.filePath })} onUnstage={summary.operation === "merge" ? undefined : (filePath) => void run("unstage", { paths: [filePath] })} onDiscard={(file) => setConfirmingDiscardFile(file.filePath)} />
            </div>
            <div className="git-section">
              <SectionTitle title={t("git.commit")} />
              <textarea className="git-textarea" value={message} onChange={(event) => setMessage(event.target.value)} placeholder={t("git.commitMessage")} disabled={disabled} rows={6} />
              <div className="git-commit-actions">
                <ActionButton label={t("git.summarizeCommit")} action="summarize" busy={busy} disabled={staged.length === 0 || conflictCount > 0} title={t("git.summarizeCommitTitle")} onClick={() => void summarizeCommitMessage()} />
                <ActionButton label={t("git.commit")} action="commit" busy={busy} disabled={!canCommit} onClick={() => void run("commit", { message })} />
                <ActionButton label={t("git.commitAndPush")} action="commit_and_push" busy={busy} disabled={!canCommit} onClick={() => void run("commit_and_push", { message })} />
              </div>
            </div>
            {/* 标签推送需要 origin，未配置远程时禁用各标签的推送按钮。 */}
            <div className="git-section">
              <SectionTitle title={t("git.tags")} action={summary.tags.length > 0 ? t("git.tagCount", { count: summary.tags.length }) : undefined} />
              <div className="git-field">
                <input className="git-input" value={newTag} onChange={(event) => setNewTag(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && newTag.trim() && !disabled) void run("create_tag", { tag: newTag.trim(), ...(tagMessage.trim() ? { message: tagMessage.trim() } : {}) }); }} placeholder={tagPlaceholder} title={tagPlaceholder} disabled={disabled} />
                <input className="git-input" value={tagMessage} onChange={(event) => setTagMessage(event.target.value)} placeholder={t("git.tagMessage")} title={t("git.tagMessage")} disabled={disabled} />
              </div>
              <div className="git-actions-row" style={{ marginBottom: summary.tags.length > 0 ? 9 : 0 }}>
                <ActionButton label={t("git.createTag")} action="create_tag" busy={busy} disabled={!newTag.trim() || Boolean(versionPreview)} onClick={() => void run("create_tag", { tag: newTag.trim(), ...(tagMessage.trim() ? { message: tagMessage.trim() } : {}) })} />
                {VERSION_UPGRADE_LEVELS.map((level) => <ActionButton key={level} label={t(`git.versionUpgrade.${level}`)} action={`bump_${level}`} busy={busy} disabled={!desktop || Boolean(summary.operation) || Boolean(versionPreview)} title={t(desktop ? "git.versionUpgradeHint" : "git.versionUpgradeDesktopOnly")} onClick={() => void versionUpgrade(level)} />)}
              </div>
              {!desktop && <div className="git-hint">{t("git.versionUpgradeDesktopOnly")}</div>}
              {versionResult?.cwd === cwd && <div role="status" className="git-note">{t("git.versionUpgradeDone", { tag: versionResult.tag })}</div>}
              {summary.tags.length === 0
                ? <div className="git-hint">{t("git.noTags")}</div>
                : <div className="git-file-list git-tag-list" role="region" aria-label={t("git.tags")} tabIndex={0}>{sortedTags.map((tag) => (
                  <div key={tag} className="git-file-row">
                    <span className="git-tag-name" title={tag}>{tag}</span>
                    <FileAction label={t("git.pushTag")} icon={Upload} action="push_tag" busy={busy} disabled={!summary.remote} onClick={() => void run("push_tag", { tag })} />
                    <FileAction label={t("git.deleteTag")} icon={Trash2} action="delete_tag" busy={busy} danger onClick={() => setConfirmingDeleteTag(tag)} />
                  </div>
                ))}</div>}
            </div>
          </>}
        </div>
        {conflictTarget && conflict?.cwd === cwd && createPortal(
          <GitConflictEditor key={conflict.path} cwd={cwd} path={conflict.path} branch={summary?.branch ?? null} remaining={conflictCount}
            active={conflictActive} handleRef={conflictEditorRef} onReveal={onShowConflict} onClose={closeConflict} onResolved={conflictResolved} />,
          conflictTarget,
        )}
        {pushPrompt?.cwd === cwd && <GitPushDialog
          cwd={cwd} committed={pushPrompt.committed} onCancel={() => setPushPrompt(null)} onPush={confirmPush}
        />}
        {pullPrompt?.cwd === cwd && summary?.isGitRepository && pullPrompt.remote === summary.remote && pullPrompt.kind === summary.credentialKind && <PullCredentialDialog
          kind={pullPrompt.kind}
          initialDraft={{ username, secret: secret === SAVED_CREDENTIAL_MASK ? "" : secret, rememberCredential }}
          canRememberCredential={summary.canRememberCredential}
          onCancel={() => setPullPrompt(null)}
          onConfirm={(draft) => { setPullPrompt(null); pull(pullPrompt.rebase, draft); }}
        />}
        {versionPreview && summary?.isGitRepository && <div ref={versionDialogRef} tabIndex={-1} role="alertdialog" aria-modal="true" aria-label={t("git.versionUpgradeConfirm", { version: versionPreview.data.version })} className="git-version-confirm">
          <div className="git-confirm-panel">
            <p className="git-confirm-text">{t("git.versionUpgradeConfirm", { version: versionPreview.data.version })}</p>
            <p className="git-confirm-note">{t("git.versionUpgradeScope", { current: versionPreview.data.current, tag: versionPreview.data.tag })}</p>
            <pre className="git-version-preview">{versionPreview.data.files.join("\n")}{"\n\n"}{versionPreview.data.output}</pre>
            <div className="git-confirm-actions">
              <ActionButton label={t("git.discardAllConfirmCancel")} action={`bump_${versionPreview.level}`} busy={busy} onClick={() => setVersionPreview(null)} />
              <ActionButton label={t("git.versionUpgradeApply")} action={`bump_${versionPreview.level}`} busy={busy} onClick={() => void versionUpgrade(versionPreview.level, versionPreview.data)} />
            </div>
          </div>
        </div>}
        {confirmingDiscardFile && summary?.isGitRepository && <DiscardFileConfirmDialog filePath={confirmingDiscardFile} busy={busy} onCancel={() => setConfirmingDiscardFile(null)} onConfirm={() => { const filePath = confirmingDiscardFile; setConfirmingDiscardFile(null); void run("discard", { paths: [filePath] }); }} />}
        {confirmingDiscardAll && summary?.isGitRepository && <DiscardAllConfirmDialog summary={summary} untrackedPaths={untrackedPaths} busy={busy} onCancel={() => setConfirmingDiscardAll(false)} onConfirm={() => { setConfirmingDiscardAll(false); void run("discard_all"); }} />}
        {confirmingDeleteTag && summary?.isGitRepository && <DeleteTagConfirmDialog tag={confirmingDeleteTag} busy={busy} onCancel={() => setConfirmingDeleteTag(null)} onConfirm={() => { const tag = confirmingDeleteTag; setConfirmingDeleteTag(null); void run("delete_tag", { tag }); }} />}
    </section>
  );
}

function PullCredentialDialog({ kind, initialDraft, canRememberCredential, onCancel, onConfirm }: {
  kind: PullPrompt["kind"];
  initialDraft: CredentialDraft;
  canRememberCredential: boolean;
  onCancel: () => void;
  onConfirm: (draft: CredentialDraft) => void;
}) {
  const { t } = useI18n();
  const titleId = useId();
  const hintId = useId();
  const dialog = useRef<HTMLDialogElement>(null);
  const [draft, setDraft] = useState(initialDraft);
  const isHttps = kind === "https";
  useLayoutEffect(() => {
    const element = dialog.current;
    element?.showModal();
    return () => { if (element?.open) element.close(); };
  }, []);

  return (
    <dialog ref={dialog} className="git-pull-dialog" aria-labelledby={titleId} aria-describedby={hintId}
      onKeyDown={(event) => { if (event.key === "Escape") event.stopPropagation(); }}
      onCancel={(event) => { event.preventDefault(); onCancel(); }}>
      <form onSubmit={(event) => {
        event.preventDefault();
        onConfirm({ ...draft, username: draft.username.trim(), rememberCredential: canRememberCredential && Boolean(draft.secret) && draft.rememberCredential });
      }}>
        <h2 id={titleId}>{t("git.pullCredentialTitle")}</h2>
        <p id={hintId} className="git-confirm-note">{t(isHttps ? "git.pullHttpsHint" : "git.pullSshHint")}</p>
        {isHttps && <label className="git-field-label">{t("git.username")}
          <input className="git-input" autoComplete="username" value={draft.username} required={Boolean(draft.secret)}
            onChange={(event) => setDraft({ ...draft, username: event.target.value })} />
        </label>}
        <label className="git-field-label">{t(isHttps ? "git.passwordOrToken" : "git.keyPassphrase")}
          <input className="git-input" type="password" autoComplete="current-password" value={draft.secret}
            onChange={(event) => setDraft({ ...draft, secret: event.target.value })} />
        </label>
        {canRememberCredential && <label className="git-check">
          <input type="checkbox" checked={Boolean(draft.secret) && draft.rememberCredential} disabled={!draft.secret}
            onChange={(event) => setDraft({ ...draft, rememberCredential: event.target.checked })} /> {t("git.rememberCredential")}
        </label>}
        <div className="git-confirm-actions">
          <button className="git-btn" type="button" onClick={onCancel}>{t("git.cancelEditRemote")}</button>
          <button className="git-btn" type="submit">{t("git.confirmPull")}</button>
        </div>
      </form>
    </dialog>
  );
}

/**
 * 数据到达前的骨架:按各分区最终的样子占位(远程 / 变更 / 暂存 / 提交)。
 *
 * 原来这里只有一行「正在加载仓库...」,`/api/git` 返回后整块面板从一行文字
 * 撑成六七屏控件,滚动位置与鼠标落点都会跳。骨架先把分区结构立起来,数据到了
 * 只换内容。加载提示只保留给读屏(`role="status"`),视觉上由骨架自己表达。
 */
function GitPanelSkeleton() {
  const { t } = useI18n();
  return (
    <>
      <span className="sr-only" role="status">{t("git.loadingRepository")}</span>
      <div className="git-skeleton" aria-hidden="true">
        <div className="git-section">
          <div className="git-skeleton-title"><span className="git-skeleton-bar" style={{ width: "24%" }} /></div>
          <div className="git-skeleton-row"><span className="git-skeleton-bar" style={{ width: "72%" }} /></div>
        </div>
        <div className="git-section">
          <div className="git-skeleton-title">
            <span className="git-skeleton-bar" style={{ width: "32%" }} />
            <span className="git-skeleton-actions"><span className="git-skeleton-pill" /><span className="git-skeleton-pill" /></span>
          </div>
          <div className="git-file-list">
            {[0, 1, 2].map((row) => <div key={row} className="git-file-row">
              <span className="git-skeleton-code" /><span className="git-skeleton-bar" style={{ width: `${58 - row * 9}%` }} />
            </div>)}
          </div>
        </div>
        <div className="git-section">
          <div className="git-skeleton-title"><span className="git-skeleton-bar" style={{ width: "26%" }} /></div>
          <div className="git-file-list">
            {[0, 1].map((row) => <div key={row} className="git-file-row">
              <span className="git-skeleton-code" /><span className="git-skeleton-bar" style={{ width: `${52 - row * 8}%` }} />
            </div>)}
          </div>
        </div>
        <div className="git-section">
          <div className="git-skeleton-title"><span className="git-skeleton-bar" style={{ width: "18%" }} /></div>
          <div className="git-skeleton-field" />
          <div className="git-skeleton-actions git-skeleton-actions-end"><span className="git-skeleton-pill" /><span className="git-skeleton-pill" /></div>
        </div>
      </div>
    </>
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

function FileList({ files, empty, busy, selectedPath, onOpenDiff, onStage, onUnstage, onDiscard }: { files: GitFileStatus[]; empty: string; busy: Action | null; selectedPath?: string; onOpenDiff: (file: GitFileStatus) => void; onStage?: (filePath: string) => void; onUnstage?: (filePath: string) => void; onDiscard?: (file: GitFileStatus) => void }) {
  const { t } = useI18n();
  if (!files.length) return <div className="git-hint">{empty}</div>;
  return (
    <div className="git-file-list">
      {files.map((file) => (
        <div key={`${file.filePath}:${file.indexStatus}:${file.worktreeStatus}`} className="git-file-row" data-conflict={file.status === "conflict" ? "true" : undefined}>
          <button type="button" className="git-file-open" onClick={() => onOpenDiff(file)} aria-pressed={selectedPath === file.filePath} disabled={Boolean(busy)} title={file.filePath}>
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
