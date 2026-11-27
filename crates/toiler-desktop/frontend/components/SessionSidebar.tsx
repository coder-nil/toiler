"use client";

import { useEffect, useLayoutEffect, useState, useCallback, useMemo, useRef, type CSSProperties, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { ArrowDown, ArrowUp, ChevronDown, ChevronRight, Eye, EyeOff, Folder, FolderPlus, GitBranch, GitCommitHorizontal, MessageSquare, PanelLeftClose, Pencil, Plus, RotateCw, Search, Speech, Star, Trash2, WandSparkles, X, Check, LoaderCircle } from "lucide-react";
import type { SessionInfo } from "@/lib/types";
import type { AppUpdateResponse } from "@/lib/api-types";
import { loadExplorerOpen, saveExplorerOpen } from "@/lib/file-explorer-state";
import { dispatchSessionRowContextMenu } from "@/lib/session-row-context-menu";
import { skillExpansionToCommand } from "@/lib/slash-display";
import { getProjectActivity, sessionsForProject } from "@/lib/project-groups";
import {
  clearLastSelectedProject,
  getLastSelectedProject,
  workspaceKeyOf,
} from "@/lib/workspace-memory";
import { getFileName } from "@/lib/file-paths";
import { subscribeToWindowForeground } from "@/lib/window-foreground";
import { CHAT_TOP_BAR_HEIGHT } from "@/lib/panel-layout";
import type { SessionSearchMatch } from "@/lib/session-search";
import { useI18n } from "@/hooks/useI18n";
import { AboutButton } from "./AboutDialog";
import { DirectoryPicker } from "./DirectoryPicker";
import { FileExplorer, TreeLoadingSpinner, type FileExplorerHandle } from "./FileExplorer";
import { BranchPicker } from "./BranchPicker";
import styles from "./SessionSidebar.module.css";

const ICON_STROKE = 1.8;

declare global {
  interface Window {
    piDesktop?: {
      selectDirectory: () => Promise<string | null>;
    };
  }
}

/**
 * 刷新按钮的动作反馈：图标自己转一圈，按钮本身既不换颜色也不换图标。
 * 沿着箭头顺时针转一圈，跑完正好回到原位；
 * 再次点击靠给 svg 换 key 重挂载，把这一圈重新跑一遍。
 * 首帧不加类，所以打开应用时不会白白转一圈。
 */
const REFRESH_SPIN_CLASS = "refresh-icon-spin";

function ToolbarIconButton({
  onClick,
  title,
  disabled,
  pressed,
  className,
  buttonRef,
  children,
}: {
  onClick: () => void;
  title: string;
  disabled?: boolean;
  pressed?: boolean;
  className?: string;
  buttonRef?: React.Ref<HTMLButtonElement>;
  children: ReactNode;
}) {
  return (
    <button
      ref={buttonRef}
      type="button"
      onClick={onClick}
      disabled={disabled}
      title={title}
      aria-label={title}
      aria-pressed={pressed}
      className={`${styles.iconButton}${className ? ` ${className}` : ""}`}
    >
      {children}
    </button>
  );
}

interface Props {
  selectedSessionId: string | null;
  onSelectSession: (session: SessionInfo, isRestore?: boolean) => void;
  onNewSession?: (sessionId: string, cwd: string) => void;
  initialSessionId?: string | null;
  skipInitialProjectSelection?: boolean;
  onInitialRestoreDone?: (hasInitialProject: boolean) => void;
  refreshKey?: number;
  onSessionDeleted?: (sessionId: string) => void;
  selectedCwd?: string | null;
  onCwdChange?: (
    cwd: string | null,
    projectRoot?: string | null,
    projectKey?: string | null,
  ) => void;
  onOpenFile?: (filePath: string, fileName: string, options?: { sourceSessionId?: string | null; modeHint?: "diff" }) => void;
  onFileRenamed?: (oldPath: string, newPath: string, newName: string) => void;
  canRenamePath?: (filePath: string) => boolean;
  explorerRefreshKey?: number;
  onExplorerRefresh?: () => void;
  gitWorkspaceCwd: string | null;
  gitWorkspaceRefreshKey: number;
  onToggleGitWorkspace: (cwd: string, contextCwd: string) => void;
  onAtMention?: (relativePath: string, isDir: boolean) => void;
  onAtMentions?: (relativePaths: string[]) => void;
  /** Fired when sessions that are not currently selected finish running. */
  onBackgroundTaskDone?: (sessions: SessionInfo[]) => void;
  onRunningSessionIdsChange?: (ids: Set<string>) => void;
  onGenerateTitle?: (sessionId: string) => void;
  titleGenerationStatus?: {
    sessionId: string;
    kind: "naming" | "success" | "error";
    message?: string;
  } | null;
  /** Update notification shown in the sidebar title row */
  appUpdate?: AppUpdateResponse | null;
  onAppUpdateClick?: () => void;
  /** 版本信息（ⓘ）：标题后的小图标，点击打开关于对话框。 */
  onAboutClick?: () => void;
  onHideSidebar?: () => void;
  openDirectoryRequest?: number;
}

interface WorktreeEntry {
  path: string;
  branch: string | null;
  isMain: boolean;
}

interface BranchSync {
  ahead: number;
  behind: number;
}

interface WorktreeState {
  /** The cwd this data was fetched for — guards against stale responses */
  forCwd: string;
  projectRoot: string;
  /** Stable server-computed identity; never derive OS path semantics here. */
  projectKey: string;
  isGit: boolean;
  /** 本机有没有 git。false 时 `isGit` 必然是 false，但两者含义不同：
   *  一个是「目录没初始化」，一个是「根本没装工具」，界面要分开提示。 */
  gitAvailable: boolean;
  /** False when forCwd is a repo subdirectory — the switcher is hidden there
   *  because subdir sessions keep their own project identity */
  isTopLevel: boolean;
  /** Canonical path of the checkout containing forCwd, resolved server-side. */
  currentWorktreePath: string | null;
  branchSync: BranchSync | null;
  worktrees: WorktreeEntry[];
  branches: string[];
  remoteBranches: string[];
}

interface BranchMenuPosition {
  top: number;
  left: number;
}

interface DeleteBranchDialogState {
  branch: string;
  remote: boolean;
  linkedWorktreePath?: string;
  forceLinkedWorktree: boolean;
}

interface ProjectSelection {
  root: string;
  key: string;
}

/** A selector row: the persisted project plus its starred (listed-first) flag. */
interface ProjectEntry extends ProjectSelection {
  starred: boolean;
  addedAt: string;
}

interface ValidatedProject {
  cwd: string;
  root: string;
  key: string;
}

interface AddedProject {
  projectKey: string;
  projectRoot: string;
  cwd: string;
  addedAt: string;
  /** Starred projects are listed before the rest of the recent list. */
  starred?: boolean;
}

const UNREAD_SESSIONS_STORAGE_KEY = "pi-desktop:unread-session-ids";
const RUNNING_SESSIONS_POLL_MS = 2500;

function loadUnreadSessionIds(): Set<string> {
  if (typeof window === "undefined") return new Set();
  try {
    const raw = window.localStorage.getItem(UNREAD_SESSIONS_STORAGE_KEY);
    if (!raw) return new Set();
    const parsed = JSON.parse(raw) as unknown;
    if (Array.isArray(parsed)) return new Set(parsed.filter((id): id is string => typeof id === "string"));
    return new Set();
  } catch {
    return new Set();
  }
}

function saveUnreadSessionIds(ids: Set<string>): void {
  if (typeof window === "undefined") return;
  try {
    if (ids.size === 0) window.localStorage.removeItem(UNREAD_SESSIONS_STORAGE_KEY);
    else window.localStorage.setItem(UNREAD_SESSIONS_STORAGE_KEY, JSON.stringify([...ids]));
  } catch {
    // ignore storage quota / privacy-mode errors
  }
}

function formatRelativeTime(dateStr: string): string {
  const date = new Date(dateStr);
  const now = new Date();
  const diff = now.getTime() - date.getTime();
  const mins = Math.floor(diff / 60000);
  const hours = Math.floor(diff / 3600000);
  const days = Math.floor(diff / 86400000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  if (hours < 24) return `${hours}h ago`;
  if (days < 7) return `${days}d ago`;
  return date.toLocaleDateString();
}

/** Substitute the home dir prefix with ~ (no path truncation — see PathLabel) */
function displayCwd(cwd: string, homeDir?: string): string {
  return (homeDir && cwd.startsWith(homeDir)) ? "~" + cwd.slice(homeDir.length) : cwd;
}

/**
 * Path label that ellipsizes on the LEFT, keeping the (most relevant) trailing
 * segments visible: "…orkspace/pi-desktop". Shows as much of the path as fits
 * instead of a fixed number of segments. The rtl container moves the ellipsis
 * to the left edge; the inner plaintext bidi isolation keeps the path itself
 * rendered strictly left-to-right (no punctuation reordering).
 */
function PathLabel({ text, style }: { text: string; style?: CSSProperties }) {
  return (
    <span
      style={{
        overflow: "hidden",
        textOverflow: "ellipsis",
        whiteSpace: "nowrap",
        display: "block",
        minWidth: 0,
        lineHeight: 1.35,
        direction: "rtl",
        textAlign: "left",
        ...style,
      }}
    >
      <span style={{ unicodeBidi: "plaintext" }}>{text}</span>
    </span>
  );
}

const DROPDOWN_ANIMATION_MS = 140;

function AnimatedDropdown({ open, children, style, className }: { open: boolean; children: ReactNode; style?: CSSProperties; className?: string }) {
  const [mounted, setMounted] = useState(open);
  const [visible, setVisible] = useState(open);

  useEffect(() => {
    let frame: number | undefined;
    let timeout: ReturnType<typeof setTimeout> | undefined;

    if (open) {
      setMounted(true);
      setVisible(false);
      frame = window.requestAnimationFrame(() => {
        frame = window.requestAnimationFrame(() => setVisible(true));
      });
    } else {
      setVisible(false);
      timeout = setTimeout(() => setMounted(false), DROPDOWN_ANIMATION_MS);
    }

    return () => {
      if (frame !== undefined) window.cancelAnimationFrame(frame);
      if (timeout) clearTimeout(timeout);
    };
  }, [open]);

  if (!mounted) return null;

  return (
    <div
      className={`${styles.animatedDropdown}${className ? ` ${className}` : ""}`}
      data-visible={visible}
      inert={!open}
      style={style}
    >
      {children}
    </div>
  );
}



interface SessionTreeNode {
  session: SessionInfo;
  children: SessionTreeNode[];
}

function buildSessionTree(sessions: SessionInfo[]): SessionTreeNode[] {
  const byId = new Map<string, SessionTreeNode>();
  for (const s of sessions) {
    byId.set(s.id, { session: s, children: [] });
  }

  // Build a map of parentSessionId chains so we can resolve missing ancestors
  const parentOf = new Map<string, string>();
  for (const s of sessions) {
    if (s.parentSessionId) parentOf.set(s.id, s.parentSessionId);
  }

  // Walk up the parentSessionId chain to find the nearest ancestor that exists in byId
  function resolveAncestor(id: string): string | null {
    let cur = parentOf.get(id);
    const visited = new Set<string>();
    while (cur) {
      if (visited.has(cur)) return null; // cycle guard
      visited.add(cur);
      if (byId.has(cur)) return cur;
      cur = parentOf.get(cur);
    }
    return null;
  }

  const roots: SessionTreeNode[] = [];
  for (const node of byId.values()) {
    const ancestor = resolveAncestor(node.session.id);
    if (ancestor) {
      byId.get(ancestor)!.children.push(node);
    } else {
      roots.push(node);
    }
  }

  // Sort each level by modified desc
  const sort = (nodes: SessionTreeNode[]) => {
    nodes.sort((a, b) => b.session.modified.localeCompare(a.session.modified));
    nodes.forEach((n) => sort(n.children));
  };
  sort(roots);
  return roots;
}

const SCRAMBLE_CHARS = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*";

function useScramble(target: string, running: boolean): string {
  const [display, setDisplay] = useState(target);
  const frameRef = useRef<number | null>(null);
  const iterRef = useRef(0);

  useEffect(() => {
    if (!running) {
      setDisplay(target);
      return;
    }
    iterRef.current = 0;
    const totalFrames = target.length * 4;

    const step = () => {
      iterRef.current += 1;
      const progress = iterRef.current / totalFrames;
      const resolved = Math.floor(progress * target.length);

      setDisplay(
        target
          .split("")
          .map((char, i) => {
            if (char === " ") return " ";
            if (i < resolved) return char;
            return SCRAMBLE_CHARS[Math.floor(Math.random() * SCRAMBLE_CHARS.length)];
          })
          .join("")
      );

      if (iterRef.current < totalFrames) {
        frameRef.current = requestAnimationFrame(step);
      } else {
        setDisplay(target);
      }
    };

    frameRef.current = requestAnimationFrame(step);
    return () => { if (frameRef.current) cancelAnimationFrame(frameRef.current); };
  }, [target, running]);

  return display;
}

function PiWebTitle({ onAboutClick }: { onAboutClick?: () => void }) {
  const [showVersion, setShowVersion] = useState(false);
  const [scrambling, setScrambling] = useState(false);
  const [hovered, setHovered] = useState(false);
  const revertTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const target = showVersion ? (process.env.NEXT_PUBLIC_APP_VERSION ?? "dev") : "Toiler";
  const display = useScramble(target, scrambling);

  const triggerScramble = useCallback((toVersion: boolean) => {
    setShowVersion(toVersion);
    setScrambling(true);
    setTimeout(() => setScrambling(false), (toVersion ? 6 : 8) * 4 * (1000 / 60) + 100);
  }, []);

  const handleClick = useCallback(() => {
    if (revertTimerRef.current) clearTimeout(revertTimerRef.current);
    triggerScramble(!showVersion);
  }, [showVersion, triggerScramble]);

  // 版本号 3 秒后退回标题；鼠标停在标题行上（正在看版本号、准备点 ⓘ）时不退回。
  useEffect(() => {
    if (!showVersion || hovered) return;
    revertTimerRef.current = setTimeout(() => triggerScramble(false), 3000);
    return () => {
      if (revertTimerRef.current) clearTimeout(revertTimerRef.current);
    };
  }, [showVersion, hovered, triggerScramble]);

  return (
    <span
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
      style={{ display: "inline-flex", alignItems: "center", gap: 6, minWidth: 0 }}
    >
      <button
        onClick={handleClick}
        style={{
          background: "none", border: "none", padding: 0, cursor: "default",
          /* 13px/600：窗口本身已经有标题了（macOS 菜单栏），这里不再是
             “网站 logo”，只是应用身份 + 关于入口。 */
          fontWeight: 600, fontSize: 13, letterSpacing: "-0.01em",
          color: showVersion ? "var(--accent)" : "var(--text)",
          fontFamily: "var(--font-mono)",
          minWidth: "6ch",
        }}
      >
        {display}
      </button>
      {showVersion && onAboutClick && <AboutButton onClick={onAboutClick} />}
    </span>
  );
}

export function SessionSidebar({ selectedSessionId, onSelectSession, onNewSession, initialSessionId, skipInitialProjectSelection, onInitialRestoreDone, refreshKey, onSessionDeleted, selectedCwd: selectedCwdProp, onCwdChange, onOpenFile, onFileRenamed, canRenamePath, explorerRefreshKey, onExplorerRefresh, gitWorkspaceCwd, gitWorkspaceRefreshKey, onToggleGitWorkspace, onAtMention, onAtMentions, onBackgroundTaskDone, onRunningSessionIdsChange, onGenerateTitle, titleGenerationStatus, appUpdate, onAppUpdateClick, onAboutClick, onHideSidebar, openDirectoryRequest }: Props) {
  const { t } = useI18n();
  const [allSessions, setAllSessions] = useState<SessionInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [initialLoadSettled, setInitialLoadSettled] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sessionSearchOpen, setSessionSearchOpen] = useState(false);
  const [sessionSearch, setSessionSearch] = useState("");
  const sessionSectionRef = useRef<HTMLElement>(null);
  const sessionSearchButtonRef = useRef<HTMLButtonElement>(null);
  const sessionSearchRef = useRef<HTMLInputElement>(null);
  const [sessionSearchResult, setSessionSearchResult] = useState<{ query: string; matches: SessionSearchMatch[] }>({
    query: "",
    matches: [],
  });
  const [sessionSearchPending, setSessionSearchPending] = useState(false);
  const [sessionSearchError, setSessionSearchError] = useState(false);
  const [selectedCwd, setSelectedCwd] = useState<string | null>(null);
  const [homeDir, setHomeDir] = useState<string>("");
  const [dropdownOpen, setDropdownOpen] = useState(false);
  const [projectFilter, setProjectFilter] = useState("");
  const [wtFilter, setWtFilter] = useState("");
  const [localBranchesOpen, setLocalBranchesOpen] = useState(true);
  const [remoteBranchesOpen, setRemoteBranchesOpen] = useState(false);
  const [remoteBranchMenu, setRemoteBranchMenu] = useState<string | null>(null);
  const [remoteBranchMenuPosition, setRemoteBranchMenuPosition] = useState<BranchMenuPosition | null>(null);
  const [createBranchDialog, setCreateBranchDialog] = useState<{ startPoint?: string } | null>(null);
  const [newBranchName, setNewBranchName] = useState("");
  const [renameBranchDialog, setRenameBranchDialog] = useState<string | null>(null);
  const [renamedBranchName, setRenamedBranchName] = useState("");
  const [deleteBranchDialog, setDeleteBranchDialog] = useState<DeleteBranchDialogState | null>(null);
  const [mergeBranchDialog, setMergeBranchDialog] = useState<string | null>(null);
  const [mergeTargetBranch, setMergeTargetBranch] = useState("");
  const [customPathOpen, setCustomPathOpen] = useState(false);
  const [customPathValue, setCustomPathValue] = useState("");
  const [customPathError, setCustomPathError] = useState<string | null>(null);
  const [customPathValidating, setCustomPathValidating] = useState(false);
  const [validatedProject, setValidatedProject] = useState<ValidatedProject | null>(null);
  const [addedProjects, setAddedProjects] = useState<AddedProject[]>([]);
  const [storedSelectedProject, setStoredSelectedProject] = useState<ProjectSelection | null>(null);
  const [projectListSettled, setProjectListSettled] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);
  // Worktree switcher state
  const [worktreeState, setWorktreeState] = useState<WorktreeState | null>(null);
  const [wtDropdownOpen, setWtDropdownOpen] = useState(false);
  const [branchMenu, setBranchMenu] = useState<string | null>(null);
  const [branchMenuPosition, setBranchMenuPosition] = useState<BranchMenuPosition | null>(null);
  const [wtError, setWtError] = useState<string | null>(null);
  const [wtBusy, setWtBusy] = useState(false);
  const [wtConfirmRemove, setWtConfirmRemove] = useState<string | null>(null);
  const [worktreeLoadingCwd, setWorktreeLoadingCwd] = useState<string | null>(null);
  const wtDropdownRef = useRef<HTMLDivElement>(null);
  const wtTriggerRef = useRef<HTMLButtonElement>(null);
  const [explorerOpen, setExplorerOpen] = useState(true);
  const [explorerKey, setExplorerKey] = useState(0);
  const [explorerLoading, setExplorerLoading] = useState(false);
  const [hideDotDirs, setHideDotDirsState] = useState(false);
  const [changesCount, setChangesCount] = useState(0);
  const [changesCollapsed, setChangesCollapsed] = useState(true);
  // 点一次刷新就让图标转一圈：计数只用来换 svg 的 key，让动画从头再跑。
  const [sessionRefreshTick, setSessionRefreshTick] = useState(0);
  const [explorerRefreshTick, setExplorerRefreshTick] = useState(0);
  const [runningSessionIds, setRunningSessionIds] = useState<Set<string>>(() => new Set());
  const [pendingUiSessionIds, setPendingUiSessionIds] = useState<Set<string>>(() => new Set());
  const [unreadSessionIds, setUnreadSessionIds] = useState<Set<string>>(() => loadUnreadSessionIds());
  const previousRunningSessionIdsRef = useRef<Set<string>>(new Set());
  // Once polling has delivered a snapshot it is the source of truth for
  // running state; late /api/sessions responses must not overwrite it.
  const runningPollAuthoritativeRef = useRef(false);
  const fileExplorerRef = useRef<FileExplorerHandle>(null);
  const refreshExplorer = useCallback(() => {
    if (onExplorerRefresh) onExplorerRefresh();
    else setExplorerKey((key) => key + 1);
  }, [onExplorerRefresh]);

  // The directory list shown in the selector is exactly the persisted records
  // in the app database (added_projects), so every entry can be removed
  // regardless of whether it has chat history. Sessions are still grouped under
  // the project the user selects; deleting a record never touches local files or
  // session files.
  const availableProjects = useMemo<ProjectEntry[]>(
    () => addedProjects
      .map((project) => ({
        key: project.projectKey,
        root: project.projectRoot,
        starred: project.starred === true,
        addedAt: project.addedAt,
      }))
      // Mirror the store's own order — starred first, then most recently added —
      // so an optimistic star click lands where a reload would put it. Array#sort
      // is stable: equal timestamps keep the order the list was loaded in.
      .sort((a, b) => (
        Number(b.starred) - Number(a.starred) || b.addedAt.localeCompare(a.addedAt)
      )),
    [addedProjects],
  );

  const loadSessions = useCallback(async (showLoading = false, force = false) => {
    try {
      if (showLoading) setLoading(true);
      const res = await fetch(force ? "/api/sessions?force=1" : "/api/sessions", {
        cache: "no-store",
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = await res.json() as { sessions: SessionInfo[]; runningSessionIds?: string[]; pendingUiSessionIds?: string[] };
      setAllSessions(data.sessions);
      // Treat the fetched running set as an initial fallback only. Once the
      // lightweight poll is live, a slow session-list fetch cannot overwrite it.
      if (!runningPollAuthoritativeRef.current) {
        setRunningSessionIds(new Set(data.runningSessionIds ?? []));
        setPendingUiSessionIds(new Set(data.pendingUiSessionIds ?? []));
      }
      // Drop unread markers for sessions that no longer exist (e.g. deleted).
      const existingIds = new Set(data.sessions.map((s) => s.id));
      setUnreadSessionIds((prev) => {
        if (prev.size === 0) return prev;
        const next = new Set([...prev].filter((id) => existingIds.has(id)));
        return next.size === prev.size ? prev : next;
      });
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      if (showLoading) {
        setLoading(false);
        setInitialLoadSettled(true);
      }
    }
  }, []);

  const initialLoadDone = useRef(false);
  useEffect(() => {
    const isFirst = !initialLoadDone.current;
    initialLoadDone.current = true;
    loadSessions(isFirst, !isFirst);
  }, [loadSessions, refreshKey]);

  // Browser storage is unavailable during server rendering. Restore the panel
  // preference after hydration so a collapsed explorer stays collapsed on reload.
  useEffect(() => {
    setExplorerOpen(loadExplorerOpen());
  }, []);

  // Persist unread markers so they survive a browser refresh before the user
  // has actually opened the completed session.
  useEffect(() => {
    saveUnreadSessionIds(unreadSessionIds);
  }, [unreadSessionIds]);

  useEffect(() => {
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let controller: AbortController | null = null;

    const clearTimer = () => {
      if (timer) clearTimeout(timer);
      timer = null;
    };

    const schedule = () => {
      clearTimer();
      if (stopped || document.visibilityState !== "visible") return;
      timer = setTimeout(() => void poll(), RUNNING_SESSIONS_POLL_MS);
    };

    const poll = async () => {
      if (stopped || document.visibilityState !== "visible") return;
      const current = new AbortController();
      controller?.abort();
      controller = current;
      try {
        const res = await fetch("/api/agent/running", {
          cache: "no-store",
          signal: current.signal,
        });
        if (!res.ok) return;
        const data = await res.json() as { runningSessionIds?: string[]; pendingUiSessionIds?: string[] };
        if (stopped || controller !== current) return;
        runningPollAuthoritativeRef.current = true;
        setRunningSessionIds(new Set(data.runningSessionIds ?? []));
        setPendingUiSessionIds(new Set(data.pendingUiSessionIds ?? []));
      } catch {
        // Keep the last known state; the next visible-tab poll retries.
      } finally {
        if (controller === current) controller = null;
        schedule();
      }
    };

    const onVisibilityChange = () => {
      if (document.visibilityState === "visible") {
        void poll();
        return;
      }
      clearTimer();
      controller?.abort();
      controller = null;
    };

    void poll();
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      stopped = true;
      clearTimer();
      controller?.abort();
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, []);

  // Keep receiving completion changes while the window is in the background.
  // Visible-tab polling above remains a fallback for browsers that interrupt SSE.
  useEffect(() => {
    const events = new EventSource("/api/agent/running/events");
    events.onmessage = (event) => {
      try {
        const data = JSON.parse(event.data) as { runningSessionIds?: string[]; pendingUiSessionIds?: string[] };
        runningPollAuthoritativeRef.current = true;
        setRunningSessionIds(new Set(data.runningSessionIds ?? []));
        setPendingUiSessionIds(new Set(data.pendingUiSessionIds ?? []));
      } catch {
        // Ignore malformed frames; EventSource reconnects after transient failures.
      }
    };
    return () => events.close();
  }, []);

  useEffect(() => {
    onRunningSessionIdsChange?.(runningSessionIds);
  }, [onRunningSessionIdsChange, runningSessionIds]);

  useEffect(() => {
    const previous = previousRunningSessionIdsRef.current;
    const completedInBackground = [...previous].filter((id) => !runningSessionIds.has(id) && id !== selectedSessionId);
    const newlyRunning = [...runningSessionIds].filter((id) => !previous.has(id));

    if (completedInBackground.length > 0 || newlyRunning.length > 0) {
      setUnreadSessionIds((prev) => {
        const next = new Set(prev);
        runningSessionIds.forEach((id) => next.delete(id));
        completedInBackground.forEach((id) => next.add(id));
        return next;
      });
    }
    const hasUnlistedRunningSession = newlyRunning.some(
      (id) => !allSessions.some((session) => session.id === id),
    );
    if (completedInBackground.length > 0 || hasUnlistedRunningSession) {
      loadSessions(false, true);
    }
    if (completedInBackground.length > 0) {
      onBackgroundTaskDone?.(
        completedInBackground.flatMap((id) => {
          const session = allSessions.find((candidate) => candidate.id === id);
          return session ? [session] : [];
        }),
      );
    }

    previousRunningSessionIdsRef.current = runningSessionIds;
  }, [runningSessionIds, selectedSessionId, allSessions, loadSessions, onBackgroundTaskDone]);

  useEffect(() => {
    if (!selectedSessionId) return;
    setUnreadSessionIds((prev) => {
      if (!prev.has(selectedSessionId)) return prev;
      const next = new Set(prev);
      next.delete(selectedSessionId);
      return next;
    });
  }, [selectedSessionId]);

  useEffect(() => {
    if (explorerRefreshKey !== undefined) setExplorerKey((k) => k + 1);
  }, [explorerRefreshKey]);

  useEffect(() => {
    fetch("/api/home").then((r) => r.json()).then((d: { home?: string }) => {
      if (d.home) setHomeDir(d.home);
    }).catch(() => {});
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    void fetch("/api/projects", { cache: "no-store", signal: controller.signal })
      .then(async (response) => {
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        return response.json() as Promise<{
          projects?: AddedProject[];
          selectedProject?: { projectKey?: string; projectRoot?: string } | null;
        }>;
      })
      .then((data) => {
        if (controller.signal.aborted) return;
        setAddedProjects(data.projects ?? []);
        const stored = data.selectedProject;
        setStoredSelectedProject(
          stored?.projectKey && stored.projectRoot
            ? { key: stored.projectKey, root: stored.projectRoot }
            : null,
        );
      })
      .catch(() => {
        // Session-backed projects remain usable if this optional list cannot load.
      })
      .finally(() => {
        if (!controller.signal.aborted) setProjectListSettled(true);
      });
    return () => controller.abort();
  }, []);

  const closeSessionSearch = useCallback(() => {
    setSessionSearchOpen(false);
    setSessionSearch("");
  }, []);

  useEffect(() => {
    if (!sessionSearchOpen) return;
    sessionSearchRef.current?.focus();

    const inSessionSection = (event: Event) => event.target instanceof Node
      && sessionSectionRef.current?.contains(event.target);
    const dismissOutside = (event: PointerEvent) => {
      if (!inSessionSection(event)) closeSessionSearch();
    };
    const dismissByEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.isComposing || !inSessionSection(event)) return;
      event.preventDefault();
      event.stopPropagation();
      closeSessionSearch();
      sessionSearchButtonRef.current?.focus();
    };
    document.addEventListener("pointerdown", dismissOutside, true);
    // 弹层内的 Esc 先关闭搜索，不能落到 document 上的全局停止会话快捷键。
    window.addEventListener("keydown", dismissByEscape, true);
    return () => {
      document.removeEventListener("pointerdown", dismissOutside, true);
      window.removeEventListener("keydown", dismissByEscape, true);
    };
  }, [closeSessionSearch, sessionSearchOpen]);

  useEffect(() => {
    const query = sessionSearch.trim();
    if (!query) {
      setSessionSearchResult({ query: "", matches: [] });
      setSessionSearchPending(false);
      setSessionSearchError(false);
      return;
    }

    const controller = new AbortController();
    setSessionSearchPending(true);
    setSessionSearchError(false);
    const timer = setTimeout(async () => {
      try {
        const params = new URLSearchParams({ q: query, limit: "80" });
        const res = await fetch(`/api/sessions/search?${params}`, {
          cache: "no-store",
          signal: controller.signal,
        });
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const data = await res.json() as { matches?: SessionSearchMatch[] };
        setSessionSearchResult({ query, matches: data.matches ?? [] });
      } catch (searchError) {
        if (searchError instanceof DOMException && searchError.name === "AbortError") return;
        setSessionSearchResult({ query, matches: [] });
        setSessionSearchError(true);
      } finally {
        if (!controller.signal.aborted) setSessionSearchPending(false);
      }
    }, 250);

    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [sessionSearch]);

  const restoredRef = useRef(false);
  const initialReadyNotifiedRef = useRef(false);

  const projectSelection = useCallback((root: string, key: string): ProjectSelection => ({
    root,
    key,
  }), []);

  /** Resolve both display root and stable identity from server-provided data. */
  const projectFor = useCallback((cwd: string | null): ProjectSelection | null => {
    if (!cwd) return null;
    // /api/cwd/validate resolves identity before a custom path becomes active,
    // preventing one render with a raw path key from looking like a switch.
    if (validatedProject?.cwd === cwd) {
      return projectSelection(validatedProject.root, validatedProject.key);
    }
    if (worktreeState && worktreeState.forCwd === cwd) {
      return projectSelection(worktreeState.projectRoot, worktreeState.projectKey);
    }
    // Any path in the loaded worktree list belongs to that project — covers
    // worktrees without sessions, so switching to them keeps the row mounted.
    if (worktreeState?.worktrees.some((w) => w.path === cwd)) {
      return projectSelection(worktreeState.projectRoot, worktreeState.projectKey);
    }
    const match = allSessions.find((session) => (
      session.cwd === cwd || (session.projectRoot ?? session.cwd) === cwd
    ));
    const addedProject = addedProjects.find((project) => (
      project.cwd === cwd || project.projectRoot === cwd
    ));
    return match
      ? projectSelection(match.projectRoot ?? match.cwd, workspaceKeyOf(match))
      : addedProject
        ? projectSelection(addedProject.projectRoot, addedProject.projectKey)
      : projectSelection(cwd, cwd);
  }, [validatedProject, worktreeState, allSessions, addedProjects, projectSelection]);

  // A worktree/session refresh can hydrate the stable key without changing
  // cwd, so notify when either changes. The parent treats same-cwd key changes
  // as identity hydration rather than a workspace switch.
  const lastNotifiedProjectRef = useRef<{ cwd: string | null; key: string | null } | null>(null);
  useEffect(() => {
    const project = projectFor(selectedCwd);
    const previous = lastNotifiedProjectRef.current;
    if (previous?.cwd === selectedCwd && previous.key === (project?.key ?? null)) return;
    lastNotifiedProjectRef.current = { cwd: selectedCwd, key: project?.key ?? null };
    onCwdChange?.(
      selectedCwd,
      project?.root ?? null,
      project?.key ?? null,
    );
    if (project && selectedCwd && projectListSettled) {
      if (storedSelectedProject?.key === project.key && storedSelectedProject.root === project.root) {
        clearLastSelectedProject();
        return;
      }
      void fetch("/api/projects", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          selectedProject: { projectKey: project.key, projectRoot: project.root },
        }),
      }).then((response) => {
        if (response.ok) {
          setStoredSelectedProject(project);
          clearLastSelectedProject();
        }
      }).catch(() => {
        // A later project change or app launch retries the database write.
      });
    }
  }, [selectedCwd, onCwdChange, projectFor, projectListSettled, storedSelectedProject]);

  // Sync the worktree switcher to the selected session's cwd. Sessions of all
  // worktrees in a project share one list, so clicking a session from another
  // worktree should move the effective cwd there. Only fires when the prop
  // value changes, so a manual switcher change is not snapped back.
  const lastSyncedCwdPropRef = useRef<string | null>(null);
  useEffect(() => {
    if (selectedCwdProp && selectedCwdProp !== lastSyncedCwdPropRef.current) {
      lastSyncedCwdPropRef.current = selectedCwdProp;
      setSelectedCwd(selectedCwdProp);
    }
  }, [selectedCwdProp]);

  // Load worktrees for the current effective cwd
  const [wtRefreshKey, setWtRefreshKey] = useState(0);
  useLayoutEffect(() => {
    if (!selectedCwd) {
      setWorktreeState(null);
      setWorktreeLoadingCwd(null);
      return;
    }
    let request: AbortController | undefined;
    const loadWorktrees = async (quiet: boolean) => {
      request?.abort();
      const { signal } = (request = new AbortController());
      if (!quiet) setWorktreeLoadingCwd(selectedCwd);
      try {
        const response = await fetch(`/api/worktrees?cwd=${encodeURIComponent(selectedCwd)}`, { signal, cache: "no-store" });
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        const d: Partial<Omit<WorktreeState, "forCwd">> & { error?: string } = await response.json();
        if (signal.aborted) return;
        if (d.error || !d.projectRoot) throw new Error(d.error);
        setWorktreeState({
          forCwd: selectedCwd,
          projectRoot: d.projectRoot,
          projectKey: d.projectKey ?? d.projectRoot,
          isGit: d.isGit ?? false,
          // 缺省当成「有 git」：老服务端不返回这个字段时，行为与以前一致。
          gitAvailable: d.gitAvailable ?? true,
          isTopLevel: d.isTopLevel ?? false,
          currentWorktreePath: d.currentWorktreePath ?? null,
          branchSync: d.branchSync ?? null,
          worktrees: d.worktrees ?? [],
          branches: d.branches ?? [],
          remoteBranches: d.remoteBranches ?? [],
        });
      } catch {
        // 前台刷新失败时保留当前项目已有的计数，避免徽标闪没。
        if (!signal.aborted) setWorktreeState((state) => quiet && state?.forCwd === selectedCwd ? state : null);
      } finally {
        if (!signal.aborted) setWorktreeLoadingCwd(null);
      }
    };
    void loadWorktrees(false);
    const unsubscribe = subscribeToWindowForeground(() => { void loadWorktrees(true); });
    return () => {
      unsubscribe();
      request?.abort();
    };
  }, [selectedCwd, wtRefreshKey, refreshKey, gitWorkspaceRefreshKey]);

  const explorerCwd = selectedCwd ?? selectedCwdProp;
  const explorerGit = worktreeState?.forCwd === explorerCwd ? worktreeState : null;
  const gitPanelOpen = Boolean(explorerCwd && gitWorkspaceCwd === explorerCwd);
  // 没装 Git 时仍提供入口，否则安装提示永远无法显示。
  const showExplorerGitButton = gitPanelOpen || explorerGit?.isGit === true || explorerGit?.gitAvailable === false;
  const openRepositoryGitPanel = useCallback((cwd: string) => {
    if (explorerCwd) onToggleGitWorkspace(cwd, explorerCwd);
  }, [explorerCwd, onToggleGitWorkspace]);
  const toggleGitPanel = () => {
    if (explorerCwd) openRepositoryGitPanel(explorerCwd);
  };

  // Auto-select cwd and restore session from URL on first load
  useEffect(() => {
    if (!initialLoadSettled || !projectListSettled) return;
    const notifyReady = (hasInitialProject: boolean) => {
      if (initialReadyNotifiedRef.current) return;
      initialReadyNotifiedRef.current = true;
      onInitialRestoreDone?.(hasInitialProject);
    };

    if (skipInitialProjectSelection) {
      notifyReady(true);
      return;
    }

    if (selectedCwd === null) {
      // If restoring a session, set cwd to match that session
      if (initialSessionId && !restoredRef.current) {
        restoredRef.current = true;
        const target = allSessions.find((s) => s.id === initialSessionId);
        if (target) {
          setSelectedCwd(target.cwd);
          onSelectSession(target, true);
          notifyReady(true);
          return;
        }
      }
      const legacyRememberedProject = getLastSelectedProject();
      const rememberedProject = storedSelectedProject ?? legacyRememberedProject;
      if (rememberedProject) {
        const target = availableProjects.find((project) => (
          project.key === rememberedProject.key || project.root === rememberedProject.root
        ));
        if (target) {
          setSelectedCwd(target.root);
          notifyReady(true);
          return;
        }
        if (legacyRememberedProject && availableProjects.length > 0) clearLastSelectedProject();
      }
      if (availableProjects.length > 0) {
        setSelectedCwd(availableProjects[0].root);
        notifyReady(true);
        return;
      }
    }
    notifyReady(selectedCwd !== null);
  }, [allSessions, availableProjects, initialLoadSettled, projectListSettled, storedSelectedProject, selectedCwd, initialSessionId, skipInitialProjectSelection, onSelectSession, onInitialRestoreDone]);

  // Prefer an exact UI selection while a refetch is in flight. Once the
  // response catches up, the server-resolved path handles Windows case and
  // separator differences without teaching the browser OS path semantics.
  const currentWorktree = worktreeState
    ? worktreeState.worktrees.find((worktree) => worktree.path === selectedCwd)
      ?? (worktreeState.forCwd === selectedCwd && worktreeState.currentWorktreePath
        ? worktreeState.worktrees.find((worktree) => worktree.path === worktreeState.currentWorktreePath)
        : undefined)
      ?? worktreeState.worktrees.find((worktree) => worktree.isMain)
    : undefined;
  const currentWorktreePath = currentWorktree?.path ?? null;

  const commitCustomPath = useCallback(async (candidate?: string) => {
    const path = (candidate ?? customPathValue).trim();
    if (!path || customPathValidating) return;

    setCustomPathValidating(true);
    setCustomPathError(null);
    try {
      const res = await fetch("/api/cwd/validate", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ cwd: path }),
      });
      const data = await res.json().catch(() => ({})) as {
        cwd?: string;
        projectRoot?: string;
        projectKey?: string;
        error?: string;
      };
      if (!res.ok || data.error || !data.cwd || !data.projectRoot || !data.projectKey) {
        setCustomPathError(data.error ?? `HTTP ${res.status}`);
        return;
      }
      const addedProject: AddedProject = {
        projectKey: data.projectKey,
        projectRoot: data.projectRoot,
        cwd: data.cwd,
        addedAt: new Date().toISOString(),
        starred: false,
      };
      setValidatedProject({
        cwd: addedProject.cwd,
        root: addedProject.projectRoot,
        key: addedProject.projectKey,
      });
      setAddedProjects((previous) => {
        // The store keeps an existing project's star when it is re-added, so the
        // optimistically inserted record has to carry it over too.
        const existing = previous.find((project) => project.projectKey === addedProject.projectKey);
        return [
          { ...addedProject, starred: existing?.starred === true },
          ...previous.filter((project) => project.projectKey !== addedProject.projectKey),
        ];
      });
      setSelectedCwd(addedProject.cwd);
      setCustomPathOpen(false);
      setCustomPathValue("");
      setDropdownOpen(false);
    } catch (e) {
      setCustomPathError(e instanceof Error ? e.message : String(e));
    } finally {
      setCustomPathValidating(false);
    }
  }, [customPathValue, customPathValidating]);

  const handleCustomPathClick = useCallback(() => {
    setCustomPathOpen(true);
    setCustomPathError(null);
    setDropdownOpen(false);
  }, []);

  const openDirectoryRequestRef = useRef(openDirectoryRequest ?? 0);
  useEffect(() => {
    if ((openDirectoryRequest ?? 0) === openDirectoryRequestRef.current) return;
    openDirectoryRequestRef.current = openDirectoryRequest ?? 0;
    handleCustomPathClick();
  }, [handleCustomPathClick, openDirectoryRequest]);
  const handleCheckoutBranch = useCallback(async (branch: string) => {
    if (!branch || wtBusy || !worktreeState || !selectedCwd) return;
    setWtBusy(true);
    setWtError(null);
    try {
      const res = await fetch("/api/worktrees", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ action: "checkout", cwd: selectedCwd, branch }),
      });
      const data = await res.json().catch(() => ({})) as { path?: string; branch?: string; error?: string };
      if (!res.ok || data.error || !data.path) {
        setWtError(data.error ?? `HTTP ${res.status}`);
        return;
      }
      setWtDropdownOpen(false);
      setWtFilter("");
      setWtRefreshKey((key) => key + 1);
    } catch (e) {
      setWtError(e instanceof Error ? e.message : String(e));
    } finally {
      setWtBusy(false);
    }
  }, [selectedCwd, wtBusy, worktreeState]);

  const handleBranchGitAction = useCallback(async (
    action: "create_branch" | "rename_branch" | "delete_branch" | "checkout_remote_branch" | "delete_remote_branch" | "pull" | "push" | "pull_branch" | "push_branch" | "merge_branch",
    branch: string,
    cwd: string | null,
    startPoint?: string,
    newBranch?: string,
    linkedWorktreePath?: string,
    forceLinkedWorktree = false,
    targetBranch?: string,
  ) => {
    if (!cwd || wtBusy) return;
    const body: Record<string, string> = { cwd, action };
    if (action === "create_branch") {
      body.branch = branch;
      if (startPoint) body.startPoint = startPoint;
    } else if (action === "rename_branch") {
      if (!newBranch?.trim() || newBranch.trim() === branch) return;
      body.branch = branch;
      body.newBranch = newBranch.trim();
    } else if (action === "delete_branch" || action === "delete_remote_branch") {
      body.branch = branch;
    } else if (action === "merge_branch") {
      if (!targetBranch?.trim() || targetBranch === branch) return;
      body.branch = branch;
      body.targetBranch = targetBranch;
    } else if (action === "checkout_remote_branch") {
      body.branch = branch;
    }
    setWtBusy(true);
    setWtError(null);
    setBranchMenu(null);
    setRemoteBranchMenu(null);
    try {
      if (action === "delete_branch" && linkedWorktreePath) {
        const removeWorktree = async (force: boolean) => {
          const response = await fetch("/api/worktrees", {
            method: "DELETE",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ cwd, path: linkedWorktreePath, force }),
          });
          const data = await response.json().catch(() => ({})) as { error?: string };
          return { response, data };
        };
        const removal = await removeWorktree(forceLinkedWorktree);
        if (removal.response.status === 409) {
          setDeleteBranchDialog((current) => current && current.branch === branch
            ? { ...current, forceLinkedWorktree: true }
            : current);
          return;
        }
        if (!removal.response.ok || removal.data.error) {
          setWtError(removal.data.error ?? `HTTP ${removal.response.status}`);
          return;
        }
      }
      const res = await fetch("/api/git", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      const data = await res.json().catch(() => ({})) as { error?: string };
      if (!res.ok || data.error) {
        setWtError(data.error ?? `HTTP ${res.status}`);
        return;
      }
      if (action === "delete_branch" || action === "delete_remote_branch") setDeleteBranchDialog(null);
      if (action === "merge_branch") setMergeBranchDialog(null);
      setWtRefreshKey((key) => key + 1);
    } catch (error) {
      setWtError(error instanceof Error ? error.message : String(error));
    } finally {
      setWtBusy(false);
    }
  }, [wtBusy]);

  const handleRemoveWorktree = useCallback(async (path: string, force: boolean) => {
    if (!worktreeState || wtBusy) return;
    setWtBusy(true);
    setWtError(null);
    try {
      const res = await fetch("/api/worktrees", {
        method: "DELETE",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ cwd: worktreeState.projectRoot, path, force }),
      });
      const data = await res.json().catch(() => ({})) as { error?: string; dirty?: boolean };
      if (!res.ok) {
        if (data.dirty && !force) {
          // Dirty worktree — ask the user to confirm a force removal
          setWtConfirmRemove(path);
          return;
        }
        setWtError(data.error ?? `HTTP ${res.status}`);
        return;
      }
      setWtConfirmRemove(null);
      if (currentWorktreePath === path) setSelectedCwd(worktreeState.projectRoot);
      setWtRefreshKey((k) => k + 1);
    } catch (e) {
      setWtError(e instanceof Error ? e.message : String(e));
    } finally {
      setWtBusy(false);
    }
  }, [worktreeState, wtBusy, currentWorktreePath]);

  // Close dropdowns on outside click
  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (dropdownRef.current && !dropdownRef.current.contains(e.target as Node)) {
        setDropdownOpen(false);
        setProjectFilter("");
      }
      const target = e.target as HTMLElement;
      if (wtDropdownRef.current && !wtDropdownRef.current.contains(target) && !target.closest("[data-branch-action-menu]")) {
        setWtDropdownOpen(false);
        setBranchMenu(null);
        setRemoteBranchMenu(null);
        setWtError(null);
        setWtConfirmRemove(null);
        setWtFilter("");
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, []);

  // Clicking a session moves the effective cwd to that session's worktree.
  // Done on the click path (not via the selectedCwd prop sync) so it also
  // works when the prop value won't change — e.g. re-clicking the already
  // open session after manually switching worktrees.
  const handleSelectSessionFromList = useCallback((s: SessionInfo) => {
    if (s.cwd) setSelectedCwd(s.cwd);
    closeSessionSearch();
    onSelectSession(s);
  }, [closeSessionSearch, onSelectSession]);

  const handleNewSession = useCallback(() => {
    if (!selectedCwd) return;
    // Generate a temporary UUID client-side — no backend call needed.
    // Pi will be spawned lazily when the user sends the first message.
    const tempId = typeof crypto.randomUUID === "function"
      ? crypto.randomUUID()
      : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}-${Math.random().toString(36).slice(2)}`;
    onNewSession?.(tempId, selectedCwd);
  }, [selectedCwd, onNewSession]);

  const showProjectFilter = availableProjects.length > 8;
  const visibleProjects = availableProjects.filter((project) => (
    !projectFilter.trim() || project.root.toLowerCase().includes(projectFilter.trim().toLowerCase())
  ));

  // Remove a directory from the list by deleting its database record only.
  // Session files and local files stay on disk; if the removed record was the
  // selected project, the selector falls back to a neighboring entry.
  const handleRemoveProject = useCallback(async (projectKey: string, selected: boolean, nextPath: string | null) => {
    try {
      const response = await fetch("/api/projects", {
        method: "DELETE",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ projectKey }),
      });
      if (!response.ok) return;
      setAddedProjects((previous) => previous.filter((project) => project.projectKey !== projectKey));
      if (selected) setSelectedCwd(nextPath);
    } catch {
      // Keep the project visible when its persistent record could not be removed.
    }
  }, []);

  // Star / unstar a project. The row and the sort order update immediately and
  // roll back if the write fails, so the list never claims a stored state it
  // does not have.
  const handleToggleProjectStar = useCallback(async (projectKey: string, starred: boolean) => {
    const apply = (value: boolean) => setAddedProjects((previous) => previous.map((project) => (
      project.projectKey === projectKey ? { ...project, starred: value } : project
    )));
    apply(starred);
    try {
      const response = await fetch("/api/projects", {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ projectKey, starred }),
      });
      if (response.ok) return;
    } catch {
      // fall through to the rollback
    }
    apply(!starred);
  }, []);

  // Sessions of every worktree in the selected project are shown together
  const selectedProject = projectFor(selectedCwd);

  // The collapsed selector shows a star when the current project is a starred
  // one, so the flag is visible without opening the list.
  const selectedProjectStarred = Boolean(
    selectedProject && availableProjects.find((project) => project.key === selectedProject.key)?.starred,
  );

  // Per-project activity counts (running / unread / waiting for an answer) for
  // the workspace selector. Uses the same stable server key as the project list
  // and filtering.
  const projectActivity = useMemo(
    () => getProjectActivity(allSessions, runningSessionIds, unreadSessionIds, pendingUiSessionIds),
    [allSessions, runningSessionIds, unreadSessionIds, pendingUiSessionIds],
  );

  const totalAwaitingCount = useMemo(
    () => [...projectActivity.values()].reduce((total, activity) => total + activity.awaiting, 0),
    [projectActivity],
  );

  // Any activity in a project other than the one currently selected — shown as
  // a dot on the (collapsed) selector button so it is visible without opening
  // the dropdown. Pending questions have their own speech badge above.
  const hasOtherWorkspaceActivity = useMemo(
    () => [...projectActivity.entries()].some(
      ([key, { running, unread }]) => key !== selectedProject?.key && (running > 0 || unread > 0),
    ),
    [projectActivity, selectedProject],
  );

  const normalizedSessionSearch = sessionSearch.trim().toLocaleLowerCase();
  const sessionSearchMatchMap = useMemo(() => {
    const resolvedMatches = sessionSearchResult.query === sessionSearch.trim()
      ? sessionSearchResult.matches
      : [];
    const matches = new Map(resolvedMatches.map((match) => [match.sessionId, match.snippet]));
    if (!normalizedSessionSearch) return matches;

    for (const session of allSessions) {
      const metadata = [session.name, session.firstMessage, session.cwd]
        .filter((value): value is string => Boolean(value));
      const matched = metadata.find((value) => value.toLocaleLowerCase().includes(normalizedSessionSearch));
      if (matched && !matches.has(session.id)) matches.set(session.id, matched.replace(/\s+/g, " ").trim());
    }
    return matches;
  }, [allSessions, normalizedSessionSearch, sessionSearch, sessionSearchResult]);
  const selectedProjectKey = selectedProject?.key;
  const filteredSessions = useMemo(() => normalizedSessionSearch
    ? allSessions.filter((session) => sessionSearchMatchMap.has(session.id))
    : selectedProjectKey
      ? sessionsForProject(allSessions, selectedProjectKey)
      : allSessions, [allSessions, normalizedSessionSearch, selectedProjectKey, sessionSearchMatchMap]);
  const showWorktreeSwitcher = Boolean(
    worktreeState?.isGit
    && worktreeState.isTopLevel
    && selectedCwd
    && selectedProject?.key === worktreeState.projectKey
  );
  // Git 面板入口跟着 Git 按钮走：底部有那个按钮时才在这行也放一个，
  // 否则只是重复一个网页版没有的功能。
  const showBranchGitButton = showWorktreeSwitcher;
  const worktreeGuide = selectedCwd
    && worktreeState?.isGit
    && selectedProject?.key === worktreeState.projectKey
    && !showWorktreeSwitcher
    ? {
         label: t("sidebar.openRepoRoot"),
         title: t("sidebar.openRepoRootTitle"),
      }
    : null;
  const worktreeLoading = Boolean(selectedCwd && worktreeLoadingCwd === selectedCwd);
  const inactiveWorktreeSelector = worktreeGuide
    ?? (worktreeLoading && !showWorktreeSwitcher
      ? {
           label: t("sidebar.worktrees"),
           title: t("sidebar.checkingWorktrees"),
        }
      : null);

  // Build parent-child tree within the filtered set
  const sessionTree = useMemo(() => buildSessionTree(filteredSessions), [filteredSessions]);
  const projectPath = selectedProject?.root ?? selectedCwd;
  const refreshSessions = () => {
    setSessionRefreshTick((tick) => tick + 1);
    void loadSessions(false, true);
  };

  return (
    <aside className={styles.sidebar} aria-label={t("sidebar.label")}>
      {customPathOpen && (
        <DirectoryPicker
          // Anchor the picker to the active project's root. With no project
          // selected, start in the current user's home directory; an empty
          // path would make the server fall back to the Windows drive list.
          initialPath={selectedProject?.root ?? homeDir}
          busy={customPathValidating}
          error={customPathError}
          onCancel={() => {
            setCustomPathOpen(false);
            setCustomPathError(null);
          }}
          onSelect={(path) => void commitCustomPath(path)}
        />
      )}
      <header className={`app-chrome ${styles.header}`}>
        <div
          data-tauri-drag-region="deep"
          className={`app-sidebar-title-row ${styles.titleRow}`}
          style={{ minHeight: CHAT_TOP_BAR_HEIGHT }}
        >
          <PiWebTitle onAboutClick={onAboutClick} />
          {appUpdate && onAppUpdateClick && appUpdate.updateAvailable && (
            <button
              type="button"
              onClick={onAppUpdateClick}
              title={appUpdate.updateAvailable ? t("appUpdate.available", { version: appUpdate.latestVersion }) : t("appUpdate.check")}
              aria-label={appUpdate.updateAvailable ? t("appUpdate.available", { version: appUpdate.latestVersion }) : t("appUpdate.check")}
              style={{
                flexShrink: 0,
                display: "inline-flex", alignItems: "center", justifyContent: "center",
                height: 20, padding: "0 8px", borderRadius: 5,
                border: appUpdate.updateAvailable ? "1px solid #d97706" : "1px solid var(--border)",
                background: appUpdate.updateAvailable ? "rgba(217,119,6,0.12)" : "transparent",
                color: appUpdate.updateAvailable ? "#d97706" : "var(--text-muted)",
                cursor: "pointer", fontSize: 11, fontWeight: 600,
                whiteSpace: "nowrap",
              }}
            >
              {`↑ v${appUpdate.latestVersion}`}
            </button>
          )}
          {onHideSidebar && (
            <ToolbarIconButton onClick={onHideSidebar} title={t("sidebar.hide")} className={styles.hideButton}>
              <PanelLeftClose size={16} strokeWidth={ICON_STROKE} aria-hidden="true" />
            </ToolbarIconButton>
          )}
        </div>

        <div ref={dropdownRef} className={styles.projectControl}>
          <button
            type="button"
            className={styles.projectTrigger}
            aria-expanded={dropdownOpen}
            onClick={() => setDropdownOpen((v) => !v)}
            title={projectPath ?? t("sidebar.selectProject")}
          >
            <span className={styles.projectMark} data-starred={selectedProjectStarred} aria-hidden="true">
              {selectedProjectStarred
                ? <Star size={16} strokeWidth={ICON_STROKE} fill="currentColor" />
                : <Folder size={16} strokeWidth={ICON_STROKE} />}
            </span>
            <span className={styles.projectText}>
              <span className={styles.projectName}>
                {projectPath ? getFileName(projectPath) || projectPath : t("sidebar.selectProject")}
              </span>
              {projectPath && (
                <PathLabel text={displayCwd(projectPath, homeDir)} style={{ fontSize: 11, color: "var(--text-muted)" }} />
              )}
            </span>
            {totalAwaitingCount > 0 && (
              <span
                title={t("sidebar.waitingForUser")}
                aria-label={`${t("sidebar.waitingForUser")} (${totalAwaitingCount})`}
                style={{
                  display: "inline-flex",
                  alignItems: "center",
                  gap: 3,
                  flexShrink: 0,
                  marginLeft: 6,
                  color: "#d97706",
                  fontSize: 10,
                  fontFamily: "var(--font-mono)",
                }}
              >
                <Speech size={11} strokeWidth={2.4} aria-hidden="true" />
                {totalAwaitingCount}
              </span>
            )}
            {hasOtherWorkspaceActivity && (
              <span
                title={t("sidebar.newActivity")}
                aria-label={t("sidebar.newActivity")}
                style={{
                  width: 8,
                  height: 8,
                  borderRadius: "50%",
                  flexShrink: 0,
                  marginLeft: 6,
                  background: "var(--accent)",
                }}
              />
            )}
            <ChevronDown size={13} strokeWidth={ICON_STROKE} className={styles.projectChevron} aria-hidden="true" />
          </button>
          <ToolbarIconButton onClick={handleCustomPathClick} title={t("sidebar.customPath")}>
            <FolderPlus size={16} strokeWidth={ICON_STROKE} aria-hidden="true" />
          </ToolbarIconButton>
          <AnimatedDropdown
            open={dropdownOpen}
            style={{
              position: "absolute",
              top: "calc(100% + 4px)",
              left: 0,
              right: 0,
              zIndex: 100,
              background: "var(--bg)",
              border: "1px solid var(--border)",
              borderRadius: 8,
              boxShadow: "0 6px 20px rgba(0,0,0,0.10)",
              overflow: "hidden",
            }}
          >
              {showProjectFilter && (
                <div style={{ padding: "6px 8px", borderBottom: "1px solid var(--border)" }}>
                  <input
                    value={projectFilter}
                    onChange={(e) => setProjectFilter(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Escape") {
                        setProjectFilter("");
                        setDropdownOpen(false);
                      }
                    }}
                     placeholder={t("sidebar.filterProjects")}
                    autoFocus
                    style={{
                      width: "100%",
                      fontSize: 11,
                      fontFamily: "var(--font-mono)",
                      padding: "5px 8px",
                      border: "1px solid var(--border)",
                      borderRadius: 5,
                      outline: "none",
                      background: "var(--bg)",
                      color: "var(--text)",
                      boxSizing: "border-box",
                    }}
                  />
                </div>
              )}
              <div style={{ maxHeight: "min(50vh, 380px)", overflowY: "auto" }}>
                {/* 行 hover 只改背景色（见 .project-row）：不改 margin、不淡入
                    行内按钮，行内文字和图标的位置与显隐全程不动。 */}
                {visibleProjects.map((project, index) => (
                  <div
                    key={project.key}
                    data-project-row
                    data-selected={project.key === selectedProject?.key ? "true" : "false"}
                    className="project-row"
                  >
                    <button
                      type="button"
                      onClick={() => {
                        setSelectedCwd(project.root);
                        setProjectFilter("");
                        setCustomPathOpen(false);
                        setCustomPathValue("");
                        setCustomPathError(null);
                        setDropdownOpen(false);
                      }}
                      style={{
                        display: "flex", alignItems: "center", gap: 7, flex: 1, minWidth: 0, padding: "8px 10px",
                        color: project.key === selectedProject?.key ? "var(--text)" : "var(--text-muted)",
                        cursor: "pointer", textAlign: "left", fontSize: 11, fontFamily: "var(--font-mono)"
                      }}
                      title={project.root}
                    >
                      <span style={{ flex: 1, fontFamily: "var(--font-mono)", fontSize: 11, color: project.key === selectedProject?.key ? "var(--text)" : "var(--text-muted)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{getFileName(project.root) || project.root}</span>
                      {showProjectActivity(projectActivity.get(project.key), t)}
                      <span style={{ width: 10, flexShrink: 0 }} />
                    </button>
                    <button
                      type="button"
                      // Starring is what puts a project at the top of this list, so
                      // the control must not also select the project.
                      onClick={(event) => {
                        event.stopPropagation();
                        void handleToggleProjectStar(project.key, !project.starred);
                      }}
                      data-star-btn
                      data-starred={project.starred ? "true" : "false"}
                      aria-pressed={project.starred}
                      title={project.starred
                        ? t("sidebar.unstarProject", { path: project.root })
                        : t("sidebar.starProject", { path: project.root })}
                      aria-label={project.starred
                        ? t("sidebar.unstarProject", { path: project.root })
                        : t("sidebar.starProject", { path: project.root })}
                      className="project-row-icon-button project-star-button"
                    >
                      <Star size={13} strokeWidth={1.8} fill={project.starred ? "currentColor" : "none"} aria-hidden="true" />
                    </button>
                    <button
                      type="button"
                      onClick={() => void handleRemoveProject(
                        project.key,
                        project.key === selectedProject?.key,
                        visibleProjects[index + 1]?.root ?? visibleProjects[index - 1]?.root ?? null,
                      )}
                      data-remove-btn
                      title={t("sidebar.removeProjectHistory", { path: project.root })}
                      aria-label={t("sidebar.removeProjectHistory", { path: project.root })}
                      className="project-row-icon-button project-remove-button"
                    >
                      <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true">
                        <path d="m6 6 12 12M18 6 6 18" />
                      </svg>
                    </button>
                  </div>
                ))}
                {visibleProjects.length === 0 && projectFilter.trim() && (
                   <div style={{ padding: "8px 10px", fontSize: 11, color: "var(--text-dim)" }}>{t("sidebar.noMatchingProjects")}</div>
                )}
              </div>

              {/* Custom path directory picker */}
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  handleCustomPathClick();
                }}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 7,
                  width: "100%",
                  padding: "8px 10px",
                  background: "none",
                  border: "none",
                  color: "var(--text-muted)",
                  cursor: "pointer",
                  textAlign: "left",
                  fontSize: 11,
                  transition: "color 0.12s, background 0.12s",
                }}
                onMouseEnter={(event) => {
                  event.currentTarget.style.color = "var(--text)";
                  event.currentTarget.style.background = "var(--bg-hover)";
                }}
                onMouseLeave={(event) => {
                  event.currentTarget.style.color = "var(--text-muted)";
                  event.currentTarget.style.background = "none";
                }}
              >
                <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.1" strokeLinecap="round" style={{ flexShrink: 0 }}>
                  <line x1="5" y1="1" x2="5" y2="9" />
                  <line x1="1" y1="5" x2="9" y2="5" />
                </svg>
                <span>{t("sidebar.customPath")}</span>
              </button>
          </AnimatedDropdown>
        </div>

        {/* Worktree switcher — shown only for git projects at a checkout top
            level (repo subdirs keep their own project identity, so switching
            from them would jump projects). Rendered whenever the selected cwd
            belongs to the loaded project (not just when forCwd matches), so
            switching between worktrees of one project keeps the row mounted
            instead of flickering while data refetches: all worktrees of a
            project share the same list anyway. */}
        {showWorktreeSwitcher && (() => {
          if (!worktreeState) return null;
          // 工作树列表可以沿用上一次的响应，但切换时不能显示上一分支的同步数字。
          const branchSync = worktreeState.forCwd === selectedCwd ? worktreeState.branchSync : null;
          const showWtFilter = worktreeState.worktrees.length + worktreeState.branches.length + worktreeState.remoteBranches.length >= 8;
          const visibleWorktrees = showWtFilter && wtFilter.trim()
            ? worktreeState.worktrees.filter((w) =>
                (w.branch ?? displayCwd(w.path, homeDir)).toLowerCase().includes(wtFilter.trim().toLowerCase()))
            : worktreeState.worktrees;
          const visibleBranches = showWtFilter && wtFilter.trim()
            ? worktreeState.branches.filter((branch) => branch.toLowerCase().includes(wtFilter.trim().toLowerCase()))
            : worktreeState.branches;
          const visibleRemoteBranches = showWtFilter && wtFilter.trim()
            ? worktreeState.remoteBranches.filter((branch) => branch.toLowerCase().includes(wtFilter.trim().toLowerCase()))
            : worktreeState.remoteBranches;
          return (
            <div ref={wtDropdownRef} style={{ position: "relative", marginTop: 6 }} onKeyDown={(event) => {
              if (!wtDropdownOpen || event.key !== "Escape") return;
              event.stopPropagation();
              setBranchMenu(null);
              setRemoteBranchMenu(null);
              setWtFilter("");
              setWtDropdownOpen(false);
              wtTriggerRef.current?.focus({ preventScroll: true });
            }}>
              {/* 与上方的项目目录选择框同一套样式：白底 + 10px 圆角 + 1px 边框。
                  高度定在外层并用 border-box，内部按钮跟 height:100%，
                  这样加了一道边框后总高仍是 29px，不会把下面内容预下去。 */}
              <div style={{ position: "relative", display: "flex", alignItems: "center", height: 29, boxSizing: "border-box", background: "var(--bg)", border: "1px solid var(--border)", borderRadius: "var(--radius-selector)", overflow: "hidden" }}>
              <button
                type="button"
                ref={wtTriggerRef}
                className={styles.worktreeTrigger}
                aria-expanded={wtDropdownOpen}
                onClick={() => {
                  if (wtDropdownOpen) {
                    setBranchMenu(null);
                    setRemoteBranchMenu(null);
                  }
                  setWtDropdownOpen(!wtDropdownOpen);
                }}
                title={currentWorktree ? t("sidebar.switchWorktreeTitle", { path: currentWorktree.path }) : t("sidebar.switchWorktree")}
                style={{
                  width: "100%",
                  height: "100%",
                  boxSizing: "border-box",
                  display: "flex",
                  alignItems: "center",
                  gap: 6,
                  padding: showBranchGitButton ? "0 100px 0 12px" : "0 72px 0 12px",
                  background: "transparent",
                  border: "none",
                  borderRadius: "calc(var(--radius-selector) - 2px)",
                  cursor: "pointer",
                  fontSize: 11,
                  lineHeight: 1.35,
                  color: "var(--text-muted)",
                  textAlign: "left",
                }}
              >
                <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0, color: currentWorktree && !currentWorktree.isMain ? "var(--accent)" : "var(--text-dim)" }}>
                  <line x1="6" y1="3" x2="6" y2="15" />
                  <circle cx="18" cy="6" r="3" />
                  <circle cx="6" cy="18" r="3" />
                  <path d="M18 9a9 9 0 0 1-9 9" />
                </svg>
                <PathLabel
                  text={currentWorktree ? (currentWorktree.branch ?? displayCwd(currentWorktree.path, homeDir)) : "…"}
                  style={{ flex: 1, fontFamily: "var(--font-mono)", color: "var(--text)" }}
                />
                {branchSync && (
                  <span className={styles.branchSync}>
                    <span title={t("git.ahead", { count: branchSync.ahead })} data-direction="ahead" data-active={branchSync.ahead > 0}>
                      <ArrowUp size={11} aria-hidden="true" />
                      <span aria-hidden="true">{branchSync.ahead}</span>
                      <span className="sr-only">{t("git.ahead", { count: branchSync.ahead })}</span>
                    </span>
                    <span title={t("git.behind", { count: branchSync.behind })} data-direction="behind" data-active={branchSync.behind > 0}>
                      <ArrowDown size={11} aria-hidden="true" />
                      <span aria-hidden="true">{branchSync.behind}</span>
                      <span className="sr-only">{t("git.behind", { count: branchSync.behind })}</span>
                    </span>
                  </span>
                )}
                {worktreeState.worktrees.length > 1 && (
                  <span style={{ flexShrink: 0, color: "var(--text-dim)", fontSize: 10 }}>
                    {worktreeState.worktrees.length}
                  </span>
                )}
                <svg width="9" height="9" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}>
                  <polyline points="2 3.5 5 6.5 8 3.5" />
                </svg>
              </button>
              {([
                { action: "pull" as const, label: t("sidebar.pull"), right: showBranchGitButton ? 56 : 28, icon: <><path d="M12 3v12" /><path d="m7 10 5 5 5-5" /><path d="M5 21h14" /></> },
                { action: "push" as const, label: t("sidebar.push"), right: showBranchGitButton ? 28 : 0, icon: <><path d="M12 15V3" /><path d="m7 8 5-5 5 5" /><path d="M5 21h14" /></> },
              ]).map(({ action, label, right, icon }) => (
                <button
                  key={action}
                  type="button"
                  onClick={(event) => { event.stopPropagation(); void handleBranchGitAction(action, "", selectedCwd); }}
                  disabled={wtBusy || worktreeLoadingCwd === selectedCwd || !selectedCwd}
                  title={label}
                  aria-label={label}
                  style={{
                    position: "absolute",
                    top: 0,
                    right,
                    width: 28,
                    height: "100%",
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    padding: 0,
                    background: "transparent",
                    border: "none",
                    borderRadius: "calc(var(--radius-selector) - 2px)",
                    color: "var(--text-muted)",
                    cursor: wtBusy || worktreeLoadingCwd === selectedCwd || !selectedCwd ? "not-allowed" : "pointer",
                    opacity: wtBusy || worktreeLoadingCwd === selectedCwd || !selectedCwd ? 0.55 : 1,
                  }}
                  onMouseEnter={(event) => { if (!event.currentTarget.disabled) event.currentTarget.style.background = "var(--bg-hover)"; }}
                  onMouseLeave={(event) => { event.currentTarget.style.background = "transparent"; }}
                >
                  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">{icon}</svg>
                </button>
              ))}
              {/* Git 面板入口。放在分支这一行：它就是这个项目 git 状态的入口，
                  和仓库名/拉取/推送属于同一组，比沉在侧边栏底部好找。 */}
              {showBranchGitButton && (
              <button
                type="button"
                onClick={(event) => { event.stopPropagation(); toggleGitPanel(); }}
                title={t("gitWorkspace.title")}
                aria-label={t("gitWorkspace.title")}
                aria-pressed={gitPanelOpen === true}
                style={{
                  position: "absolute",
                  top: 0,
                  right: 0,
                  width: 28,
                  height: "100%",
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  padding: 0,
                  background: gitPanelOpen ? "var(--bg-selected)" : "transparent",
                  border: "none",
                  borderRadius: "calc(var(--radius-selector) - 2px)",
                  color: gitPanelOpen ? "var(--text)" : "var(--text-muted)",
                  cursor: "pointer",
                }}
                onMouseEnter={(event) => { event.currentTarget.style.background = "var(--bg-selected)"; }}
                onMouseLeave={(event) => { event.currentTarget.style.background = gitPanelOpen ? "var(--bg-selected)" : "transparent"; }}
              >
                <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <circle cx="6" cy="6" r="3" />
                  <circle cx="18" cy="18" r="3" />
                  <path d="M6 9v6a3 3 0 0 0 3 3h6" />
                  <path d="M18 15V9a3 3 0 0 0-3-3H9" />
                </svg>
              </button>
              )}
              </div>

              <AnimatedDropdown open={wtDropdownOpen} className={styles.worktreeDropdown}>
                  {showWtFilter && (
                    <div className={styles.worktreeFilter}>
                      <Search size={13} aria-hidden="true" />
                      <input
                        value={wtFilter}
                        onChange={(e) => setWtFilter(e.target.value)}
                        aria-label={t("sidebar.filterWorktrees")}
                        placeholder={t("sidebar.filterWorktrees")}
                        autoFocus
                      />
                    </div>
                  )}
                  <div className={styles.worktreeList}>
                    {visibleWorktrees.length > 0 && (
                      <div className={styles.worktreeSectionHeading}>
                        <span>{t("sidebar.worktreeGroup")}</span>
                        <span className={styles.worktreeCount}>{visibleWorktrees.length}</span>
                      </div>
                    )}
                    {visibleWorktrees.map((wt) => {
                      const isCurrent = wt.path === currentWorktreePath;
                      if (wtConfirmRemove === wt.path) {
                        return (
                          <div key={wt.path} style={{ display: "flex", alignItems: "center", gap: 6, padding: "7px 10px", borderBottom: "1px solid var(--border)", background: "rgba(239,68,68,0.06)" }}>
                            <span style={{ flex: 1, fontSize: 11, color: "var(--text)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                              {t("sidebar.forceRemoveCheckout")}
                            </span>
                            <button
                              onClick={() => void handleRemoveWorktree(wt.path, true)}
                              disabled={wtBusy}
                              style={{ padding: "3px 9px", background: "#ef4444", border: "none", borderRadius: 5, color: "#fff", fontSize: 11, fontWeight: 600, cursor: "pointer", flexShrink: 0 }}
                            >
                              {t("sidebar.force")}
                            </button>
                            <button
                              onClick={() => setWtConfirmRemove(null)}
                              style={{ padding: "3px 9px", background: "var(--bg-hover)", border: "1px solid var(--border)", borderRadius: 5, color: "var(--text-muted)", fontSize: 11, cursor: "pointer", flexShrink: 0 }}
                            >
                              {t("sidebar.cancel")}
                            </button>
                          </div>
                        );
                      }
                      return (
                        <div
                          key={wt.path}
                          className={styles.worktreeRow}
                        >
                          <button
                            type="button"
                            className={styles.worktreeRef}
                            aria-current={isCurrent ? "true" : undefined}
                            onClick={() => {
                              setSelectedCwd(wt.path);
                              setWtDropdownOpen(false);
                              setWtError(null);
                              setWtFilter("");
                            }}
                            title={wt.path}
                          >
                            {isCurrent ? (
                              <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="var(--accent)" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}>
                                <polyline points="1.5 5 4 7.5 8.5 2.5" />
                              </svg>
                            ) : (
                              <span style={{ width: 10, flexShrink: 0 }} />
                            )}
                            <PathLabel text={wt.branch ?? displayCwd(wt.path, homeDir)} style={{ flex: 1 }} />
                          </button>
                          {!wt.isMain && (
                            <button
                              type="button"
                              className={styles.worktreeRemove}
                              onClick={() => void handleRemoveWorktree(wt.path, false)}
                              disabled={wtBusy}
                              title={t("sidebar.removeWorktreeTitle", { path: wt.path })}
                              aria-label={t("sidebar.removeWorktreeTitle", { path: wt.path })}
                            >
                              <Trash2 size={13} aria-hidden="true" />
                            </button>
                          )}
                        </div>
                      );
                    })}
                    {showWtFilter && visibleWorktrees.length === 0 && wtFilter.trim() && (
                      visibleBranches.length === 0 && visibleRemoteBranches.length === 0 && (
                        <div className={styles.worktreeEmpty} role="status">{t("sidebar.noMatchingWorktrees")}</div>
                      )
                    )}
                    <button
                      type="button"
                      onClick={() => setLocalBranchesOpen((open) => !open)}
                      aria-expanded={localBranchesOpen}
                      className={styles.worktreeSectionHeading}
                    >
                      <span>{t("sidebar.localBranches")}</span>
                      <span className={styles.worktreeCount}>{visibleBranches.length}</span>
                      <ChevronRight size={12} className={styles.worktreeSectionCaret} aria-hidden="true" />
                    </button>
                    {localBranchesOpen && visibleBranches.map((branch) => {
                      const branchWorktree = worktreeState.worktrees.find((worktree) => worktree.branch === branch);
                      const isCurrent = branchWorktree?.path === currentWorktreePath;
                      return (
                        <div key={branch} className={styles.worktreeRow}>
                          <button
                            type="button"
                            className={styles.worktreeRef}
                            aria-current={isCurrent ? "true" : undefined}
                            onClick={(event) => {
                              const rect = event.currentTarget.getBoundingClientRect();
                              setBranchMenu((open) => open === branch ? null : branch);
                              setRemoteBranchMenu(null);
                              setBranchMenuPosition({ top: rect.top, left: rect.right + 4 });
                            }}
                            disabled={wtBusy}
                            aria-haspopup="menu"
                            aria-expanded={branchMenu === branch}
                            title={branchWorktree?.path ?? branch}
                          >
                            {isCurrent ? (
                              <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="var(--accent)" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}>
                                <polyline points="1.5 5 4 7.5 8.5 2.5" />
                              </svg>
                            ) : (
                              <span style={{ width: 10, flexShrink: 0 }} />
                            )}
                            <PathLabel text={branch} style={{ flex: 1 }} />
                            <svg width="8" height="8" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0, color: "var(--text-dim)" }}><polyline points="3 2 7 5 3 8" /></svg>
                          </button>
                          {branchMenu === branch && branchMenuPosition && (
                            <BranchMenuPortal branch={branch} position={branchMenuPosition}>
                              {!isCurrent && <BranchMenuButton label={t("sidebar.checkOut")} onClick={() => {
                                setBranchMenu(null);
                                if (branchWorktree) {
                                  setSelectedCwd(branchWorktree.path);
                                  setWtDropdownOpen(false);
                                  setWtError(null);
                                  setWtFilter("");
                                } else {
                                  void handleCheckoutBranch(branch);
                                }
                              }} />}
                              <BranchMenuButton label={t("sidebar.newBranch")} onClick={() => { setBranchMenu(null); setNewBranchName(""); setCreateBranchDialog({}); }} />
                              <BranchMenuButton label={t("sidebar.mergeBranch")} onClick={() => {
                                const currentBranch = worktreeState.worktrees.find((worktree) => worktree.path === currentWorktreePath)?.branch;
                                setBranchMenu(null);
                                setWtError(null);
                                setMergeTargetBranch(currentBranch && currentBranch !== branch ? currentBranch : worktreeState.branches.find((candidate) => candidate !== branch) ?? "");
                                setMergeBranchDialog(branch);
                              }} />
                              <BranchMenuButton label={t("sidebar.pull")} onClick={() => void handleBranchGitAction(branchWorktree ? "pull" : "pull_branch", branch, branchWorktree?.path ?? selectedCwd)} />
                              <BranchMenuButton label={t("sidebar.push")} onClick={() => void handleBranchGitAction(branchWorktree ? "push" : "push_branch", branch, branchWorktree?.path ?? selectedCwd)} />
                              <BranchMenuDivider />
                              <BranchMenuButton label={t("sidebar.renameBranch")} onClick={() => {
                                setBranchMenu(null);
                                setRenamedBranchName(branch);
                                setRenameBranchDialog(branch);
                              }} />
                              {!isCurrent && !branchWorktree?.isMain && <BranchMenuButton label={t("sidebar.delete")} danger onClick={() => {
                                setBranchMenu(null);
                                setWtError(null);
                                setDeleteBranchDialog({ branch, remote: false, linkedWorktreePath: branchWorktree?.path, forceLinkedWorktree: false });
                              }} />}
                            </BranchMenuPortal>
                          )}
                        </div>
                      );
                    })}
                    {localBranchesOpen && visibleBranches.length === 0 && !wtFilter.trim() && (
                      <div className={styles.worktreeEmpty}>{t("sidebar.noLocalBranches")}</div>
                    )}
                    <button
                      type="button"
                      onClick={() => setRemoteBranchesOpen((open) => !open)}
                      aria-expanded={remoteBranchesOpen}
                      className={styles.worktreeSectionHeading}
                    >
                      <span>{t("sidebar.remoteBranches")}</span>
                      <span className={styles.worktreeCount}>{visibleRemoteBranches.length}</span>
                      <ChevronRight size={12} className={styles.worktreeSectionCaret} aria-hidden="true" />
                    </button>
                    {remoteBranchesOpen && visibleRemoteBranches.map((branch) => (
                      <div key={branch} className={styles.worktreeRow}>
                        <button
                          type="button"
                          className={styles.worktreeRef}
                          onClick={(event) => {
                            const rect = event.currentTarget.getBoundingClientRect();
                            setRemoteBranchMenu((open) => open === branch ? null : branch);
                            setBranchMenu(null);
                            setRemoteBranchMenuPosition({ top: rect.top, left: rect.right + 4 });
                          }}
                          aria-haspopup="menu"
                          aria-expanded={remoteBranchMenu === branch}
                          title={branch}
                        >
                          <span style={{ width: 10, flexShrink: 0 }} />
                          <PathLabel text={branch} style={{ flex: 1 }} />
                          <svg width="8" height="8" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0, color: "var(--text-dim)" }}><polyline points="3 2 7 5 3 8" /></svg>
                        </button>
                        {remoteBranchMenu === branch && remoteBranchMenuPosition && (
                          <BranchMenuPortal branch={branch} position={remoteBranchMenuPosition}>
                            <BranchMenuButton label={t("sidebar.checkOut")} onClick={() => void handleBranchGitAction("checkout_remote_branch", branch, selectedCwd)} />
                            <BranchMenuButton label={t("sidebar.newBranch")} onClick={() => { setRemoteBranchMenu(null); setNewBranchName(""); setCreateBranchDialog({ startPoint: branch }); }} />
                            <BranchMenuDivider />
                            <BranchMenuButton label={t("sidebar.delete")} danger onClick={() => {
                              setRemoteBranchMenu(null);
                              setWtError(null);
                              setDeleteBranchDialog({ branch, remote: true, forceLinkedWorktree: false });
                            }} />
                          </BranchMenuPortal>
                        )}
                      </div>
                    ))}
                    {remoteBranchesOpen && visibleRemoteBranches.length === 0 && !wtFilter.trim() && (
                      <div className={styles.worktreeEmpty}>{t("sidebar.noRemoteBranches")}</div>
                    )}
                  </div>

                  {wtError && (
                    <div className={styles.worktreeError} role="alert">{wtError}</div>
                  )}
              </AnimatedDropdown>
            </div>
          );
        })()}
        {inactiveWorktreeSelector && (
          <button
            type="button"
            aria-disabled="true"
            tabIndex={-1}
            title={inactiveWorktreeSelector.title}
            style={{
              width: "100%",
              height: 29,
              boxSizing: "border-box",
              marginTop: 6,
              display: "flex",
              alignItems: "center",
              gap: 6,
              padding: "0 10px",
              border: "1px solid var(--border)",
              borderRadius: "var(--radius-selector)",
              background: "var(--bg-panel)",
              color: "var(--text-muted)",
              fontSize: 11,
              lineHeight: 1.35,
              whiteSpace: "nowrap",
              textAlign: "left",
              cursor: "default",
            }}
          >
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}>
              <line x1="6" y1="3" x2="6" y2="15" />
              <circle cx="18" cy="6" r="3" />
              <circle cx="6" cy="18" r="3" />
              <path d="M18 9a9 9 0 0 1-9 9" />
            </svg>
            <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>{inactiveWorktreeSelector.label}</span>
          </button>
        )}
      </header>

      {explorerCwd && (
        <section className={styles.explorer} data-open={explorerOpen} aria-label={t("files.explorer")}>
          <div className={styles.sectionBar}>
              <button
                type="button"
                className={styles.explorerTrigger}
                onClick={() => {
                  const next = !explorerOpen;
                  setExplorerOpen(next);
                  saveExplorerOpen(next);
                }}
                aria-expanded={explorerOpen}
              >
                {explorerOpen && explorerLoading ? (
                  <TreeLoadingSpinner size={12} label={t("files.loading")} />
                ) : (
                  <ChevronRight size={13} strokeWidth={ICON_STROKE} className={styles.explorerChevron} aria-hidden="true" />
                )}
                <span>{t("files.explorer")}</span>
              </button>
            <div className={styles.sectionActions}>
                  {explorerOpen && (
                    <ToolbarIconButton
                      onClick={() => fileExplorerRef.current?.setHideDotDirs((v) => !v)}
                      title={hideDotDirs ? t("files.showHiddenFiles") : t("files.hideHiddenFiles")}
                      pressed={hideDotDirs}
                    >
                      {hideDotDirs ? <EyeOff size={14} strokeWidth={ICON_STROKE} /> : <Eye size={14} strokeWidth={ICON_STROKE} />}
                    </ToolbarIconButton>
                  )}
                  {explorerOpen && changesCount > 0 && (
                    <ToolbarIconButton
                      onClick={() => setChangesCollapsed((v) => !v)}
                      title={t("sidebar.changedFiles", { count: changesCount })}
                      pressed={!changesCollapsed}
                    >
                      <GitCommitHorizontal size={14} strokeWidth={ICON_STROKE} aria-hidden="true" />
                    </ToolbarIconButton>
                  )}
                  <ToolbarIconButton
                    onClick={() => {
                      setExplorerRefreshTick((tick) => tick + 1);
                      refreshExplorer();
                    }}
                    title={t("sidebar.refreshExplorer")}
                  >
                    <RotateCw
                      key={explorerRefreshTick}
                      className={explorerRefreshTick > 0 ? REFRESH_SPIN_CLASS : undefined}
                      size={14}
                      strokeWidth={ICON_STROKE}
                      aria-hidden="true"
                    />
                  </ToolbarIconButton>
              {showExplorerGitButton && (
                <ToolbarIconButton
                  onClick={toggleGitPanel}
                  title={t("gitWorkspace.title")}
                  pressed={gitPanelOpen}
                >
                  <GitBranch size={14} strokeWidth={ICON_STROKE} aria-hidden="true" />
                </ToolbarIconButton>
              )}
            </div>
          </div>
          {/* 保留已展开的目录，切回文件树时不重新挂载。 */}
          {explorerOpen && (
            <div className={styles.explorerScroll}>
              <FileExplorer
                ref={fileExplorerRef}
                cwd={explorerCwd}
                onOpenFile={onOpenFile ?? (() => {})}
                onFileRenamed={onFileRenamed}
                canRenamePath={canRenamePath}
                refreshKey={explorerKey}
                onAtMention={onAtMention}
                onAtMentions={onAtMentions}
                onOpenRepositoryGitPanel={openRepositoryGitPanel}
                onLoadingChange={setExplorerLoading}
                changesCollapsed={changesCollapsed}
                onChangesCountChange={setChangesCount}
                hideDotDirs={hideDotDirs}
                onHideDotDirsChange={setHideDotDirsState}
              />
            </div>
          )}
        </section>
      )}

      <section
        ref={sessionSectionRef}
        className={styles.sessions}
        aria-label={t("sidebar.sessions")}
        data-search-open={sessionSearchOpen}
      >
        <div className={styles.sessionHeader}>
          <div className={styles.sectionBar}>
            <h2 className={styles.sectionHeading}>{t("sidebar.sessions")}</h2>
            <div className={styles.sectionActions}>
              <button
                type="button"
                className={styles.newSession}
                onClick={() => {
                  closeSessionSearch();
                  handleNewSession();
                }}
                disabled={!selectedCwd}
                title={selectedCwd ? t("sidebar.newSessionTitle", { path: selectedCwd }) : t("sidebar.selectProject")}
                aria-label={t("i18n.newSession")}
              >
                <Plus size={14} strokeWidth={ICON_STROKE} aria-hidden="true" />
                <span>{t("i18n.newSession")}</span>
              </button>
              <ToolbarIconButton onClick={refreshSessions} title={t("sidebar.refresh")}>
                <RotateCw
                  key={sessionRefreshTick}
                  className={sessionRefreshTick > 0 ? REFRESH_SPIN_CLASS : undefined}
                  size={14}
                  strokeWidth={ICON_STROKE}
                  aria-hidden="true"
                />
              </ToolbarIconButton>
              <button
                ref={sessionSearchButtonRef}
                type="button"
                className={styles.iconButton}
                aria-label={t("sidebar.searchSessions")}
                title={t("sidebar.searchSessions")}
                aria-expanded={sessionSearchOpen}
                aria-controls="sidebar-session-search"
                onClick={() => {
                  if (sessionSearchOpen) closeSessionSearch();
                  else setSessionSearchOpen(true);
                }}
              >
                <Search size={14} strokeWidth={ICON_STROKE} aria-hidden="true" />
              </button>
            </div>
          </div>
          <div id="sidebar-session-search" className={styles.search} role="search" aria-label={t("sidebar.searchSessions")} hidden={!sessionSearchOpen}>
            <Search size={14} strokeWidth={ICON_STROKE} aria-hidden="true" />
            <input
              ref={sessionSearchRef}
              type="text"
              role="searchbox"
              value={sessionSearch}
              onChange={(event) => setSessionSearch(event.target.value)}
              placeholder={t("sidebar.searchSessions")}
              aria-label={t("sidebar.searchSessions")}
            />
            {sessionSearch && (
              <ToolbarIconButton onClick={() => {
                setSessionSearch("");
                sessionSearchRef.current?.focus();
              }} title={t("sidebar.clearSearch")}>
                <X size={13} strokeWidth={ICON_STROKE} aria-hidden="true" />
              </ToolbarIconButton>
            )}
          </div>
        </div>

        <div className={styles.sessionList} aria-busy={loading || (Boolean(normalizedSessionSearch) && sessionSearchPending)}>
          {loading && (
            <>
              <span className="sr-only" role="status">{t("sidebar.loading")}</span>
              <div className={styles.skeletons} aria-hidden="true">
                {[0, 1, 2].map((row) => <div key={row}><span /><span /></div>)}
              </div>
            </>
          )}
          {error && (
            <div className={styles.listState} role="alert">
              <strong>{t("sidebar.loadFailed")}</strong>
              <span>{error}</span>
            </div>
          )}
          {!loading && !error && normalizedSessionSearch && sessionSearchPending && filteredSessions.length === 0 && (
            <div className={styles.listState} role="status">{t("sidebar.searchingSessions")}</div>
          )}
          {!loading && !error && normalizedSessionSearch && sessionSearchError && filteredSessions.length === 0 && (
            <div className={styles.listState} role="alert">{t("sidebar.searchFailed")}</div>
          )}
          {!loading && !error && filteredSessions.length === 0 && (!normalizedSessionSearch || (!sessionSearchPending && !sessionSearchError)) && (
            <div className={styles.listState} role="status">
              <MessageSquare size={18} strokeWidth={ICON_STROKE} aria-hidden="true" />
              <strong>{normalizedSessionSearch ? t("sidebar.noMatchingSessions") : t("sidebar.noSessions")}</strong>
              {!normalizedSessionSearch && <span>{t(selectedCwd ? "sidebar.emptyProjectHint" : "sidebar.emptyWorkspaceHint")}</span>}
            </div>
          )}
          {sessionTree.map((node) => (
            <SessionTreeItem
              key={node.session.id}
              node={node}
              selectedSessionId={selectedSessionId}
              runningSessionIds={runningSessionIds}
              unreadSessionIds={unreadSessionIds}
              onSelectSession={handleSelectSessionFromList}
              onRenamed={loadSessions}
              onSessionDeleted={(id) => {
                onSessionDeleted?.(id);
                loadSessions();
              }}
              onGenerateTitle={onGenerateTitle}
              titleGenerationStatus={titleGenerationStatus}
              searchMatches={sessionSearchMatchMap}
              depth={0}
            />
          ))}
        </div>
      </section>

      {createBranchDialog && (
        <CreateBranchDialog
          value={newBranchName}
          busy={wtBusy}
          onChange={setNewBranchName}
          onCancel={() => { if (!wtBusy) setCreateBranchDialog(null); }}
          onConfirm={() => {
            if (!newBranchName.trim()) return;
            void handleBranchGitAction("create_branch", newBranchName.trim(), selectedCwd, createBranchDialog.startPoint)
              .finally(() => setCreateBranchDialog(null));
          }}
        />
      )}
      {renameBranchDialog && (
        <RenameBranchDialog
          value={renamedBranchName}
          busy={wtBusy}
          onChange={setRenamedBranchName}
          onCancel={() => { if (!wtBusy) setRenameBranchDialog(null); }}
          onConfirm={() => {
            if (!renamedBranchName.trim() || renamedBranchName.trim() === renameBranchDialog) return;
            void handleBranchGitAction("rename_branch", renameBranchDialog, selectedCwd, undefined, renamedBranchName.trim())
              .finally(() => setRenameBranchDialog(null));
          }}
        />
      )}
      {deleteBranchDialog && (
        <DeleteBranchDialog
          state={deleteBranchDialog}
          busy={wtBusy}
          error={wtError}
          onCancel={() => { if (!wtBusy) { setDeleteBranchDialog(null); setWtError(null); } }}
          onConfirm={() => void handleBranchGitAction(
            deleteBranchDialog.remote ? "delete_remote_branch" : "delete_branch",
            deleteBranchDialog.branch,
            selectedCwd,
            undefined,
            undefined,
            deleteBranchDialog.linkedWorktreePath,
            deleteBranchDialog.forceLinkedWorktree,
          )}
        />
      )}
      {mergeBranchDialog && worktreeState && (
        <MergeBranchDialog
          source={mergeBranchDialog}
          branches={worktreeState.branches}
          value={mergeTargetBranch}
          busy={wtBusy}
          error={wtError}
          onChange={setMergeTargetBranch}
          onCancel={() => { if (!wtBusy) { setMergeBranchDialog(null); setWtError(null); } }}
          onConfirm={() => void handleBranchGitAction("merge_branch", mergeBranchDialog, selectedCwd, undefined, undefined, undefined, false, mergeTargetBranch)}
        />
      )}
    </aside>
  );
}

function BranchMenuPortal({ branch, position, children }: { branch: string; position: BranchMenuPosition; children: ReactNode }) {
  if (typeof document === "undefined") return null;
  return createPortal(
    <div data-branch-action-menu role="menu" aria-label={branch} className={styles.branchActionMenu} style={position}>
      {children}
    </div>,
    document.body,
  );
}

function BranchMenuDivider() {
  return <div role="separator" style={{ height: 1, margin: "4px 4px", background: "var(--border)" }} />;
}

function BranchMenuButton({ label, disabled, danger, onClick }: { label: string; disabled?: boolean; danger?: boolean; onClick: () => void }) {
  return (
    <button
      type="button"
      role="menuitem"
      className={styles.branchActionItem}
      data-danger={danger}
      disabled={disabled}
      onClick={onClick}
    >
      {label}
    </button>
  );
}

function CreateBranchDialog({ value, busy, onChange, onCancel, onConfirm }: { value: string; busy: boolean; onChange: (value: string) => void; onCancel: () => void; onConfirm: () => void }) {
  const { t } = useI18n();
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => { inputRef.current?.focus(); }, []);
  useEffect(() => { const handleKeyDown = (event: KeyboardEvent) => { if (event.key === "Escape" && !busy) { event.preventDefault(); event.stopPropagation(); onCancel(); } }; document.addEventListener("keydown", handleKeyDown, true); return () => document.removeEventListener("keydown", handleKeyDown, true); }, [busy, onCancel]);
  return (
    <div role="presentation" onClick={(event) => { if (!busy && event.target === event.currentTarget) onCancel(); }} style={{ position: "fixed", inset: 0, zIndex: 1100, display: "flex", alignItems: "center", justifyContent: "center", padding: 16, background: "rgba(0,0,0,.4)" }}>
      <div role="dialog" aria-modal="true" aria-labelledby="create-branch-title" style={{ width: 360, maxWidth: "100%", border: "1px solid var(--border)", borderRadius: 8, background: "var(--bg-panel)", boxShadow: "0 12px 36px rgba(0,0,0,.24)" }}>
        <div style={{ padding: "16px 18px" }}>
          <div id="create-branch-title" style={{ color: "var(--text)", fontSize: 14, fontWeight: 700 }}>{t("sidebar.createLocalBranch")}</div>
          <input ref={inputRef} value={value} onChange={(event) => onChange(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") onConfirm(); if (event.key === "Escape") onCancel(); }} placeholder={t("sidebar.branchName")} disabled={busy} style={{ width: "100%", height: 32, marginTop: 12, padding: "0 9px", border: "1px solid var(--border)", borderRadius: 5, outline: "none", background: "var(--bg)", color: "var(--text)", fontFamily: "var(--font-mono)", fontSize: 12 }} />
        </div>
        <div style={{ display: "flex", justifyContent: "flex-end", gap: 6, padding: "10px 18px", borderTop: "1px solid var(--border)" }}>
          <button type="button" onClick={onCancel} disabled={busy} style={{ height: 30, padding: "0 10px", border: "none", borderRadius: 5, background: "transparent", color: "var(--text-muted)", cursor: busy ? "not-allowed" : "pointer", fontSize: 12 }}>{t("sidebar.cancel")}</button>
          <button type="button" onClick={onConfirm} disabled={busy || !value.trim()} style={{ height: 30, padding: "0 10px", border: "none", borderRadius: 5, background: "var(--accent)", color: "#fff", cursor: busy || !value.trim() ? "not-allowed" : "pointer", opacity: busy || !value.trim() ? .6 : 1, fontSize: 12, fontWeight: 600 }}>{busy ? t("sidebar.creating") : t("sidebar.create")}</button>
        </div>
      </div>
    </div>
  );
}

function RenameBranchDialog({ value, busy, onChange, onCancel, onConfirm }: { value: string; busy: boolean; onChange: (value: string) => void; onCancel: () => void; onConfirm: () => void }) {
  const { t } = useI18n();
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => { inputRef.current?.focus(); inputRef.current?.select(); }, []);
  useEffect(() => { const handleKeyDown = (event: KeyboardEvent) => { if (event.key === "Escape" && !busy) { event.preventDefault(); event.stopPropagation(); onCancel(); } }; document.addEventListener("keydown", handleKeyDown, true); return () => document.removeEventListener("keydown", handleKeyDown, true); }, [busy, onCancel]);
  return (
    <div role="presentation" onClick={(event) => { if (!busy && event.target === event.currentTarget) onCancel(); }} style={{ position: "fixed", inset: 0, zIndex: 1100, display: "flex", alignItems: "center", justifyContent: "center", padding: 16, background: "rgba(0,0,0,.4)" }}>
      <div role="dialog" aria-modal="true" aria-labelledby="rename-branch-title" style={{ width: 360, maxWidth: "100%", border: "1px solid var(--border)", borderRadius: 8, background: "var(--bg-panel)", boxShadow: "0 12px 36px rgba(0,0,0,.24)" }}>
        <div style={{ padding: "16px 18px" }}>
          <div id="rename-branch-title" style={{ color: "var(--text)", fontSize: 14, fontWeight: 700 }}>{t("sidebar.renameLocalBranch")}</div>
          <input ref={inputRef} value={value} onChange={(event) => onChange(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") onConfirm(); if (event.key === "Escape") onCancel(); }} placeholder={t("sidebar.branchName")} disabled={busy} style={{ width: "100%", height: 32, marginTop: 12, padding: "0 9px", border: "1px solid var(--border)", borderRadius: 5, outline: "none", background: "var(--bg)", color: "var(--text)", fontFamily: "var(--font-mono)", fontSize: 12 }} />
        </div>
        <div style={{ display: "flex", justifyContent: "flex-end", gap: 6, padding: "10px 18px", borderTop: "1px solid var(--border)" }}>
          <button type="button" onClick={onCancel} disabled={busy} style={{ height: 30, padding: "0 10px", border: "none", borderRadius: 5, background: "transparent", color: "var(--text-muted)", cursor: busy ? "not-allowed" : "pointer", fontSize: 12 }}>{t("sidebar.cancel")}</button>
          <button type="button" onClick={onConfirm} disabled={busy || !value.trim()} style={{ height: 30, padding: "0 10px", border: "none", borderRadius: 5, background: "var(--accent)", color: "#fff", cursor: busy || !value.trim() ? "not-allowed" : "pointer", opacity: busy || !value.trim() ? .6 : 1, fontSize: 12, fontWeight: 600 }}>{t("sidebar.rename")}</button>
        </div>
      </div>
    </div>
  );
}

function DeleteBranchDialog({ state, busy, error, onCancel, onConfirm }: { state: DeleteBranchDialogState; busy: boolean; error: string | null; onCancel: () => void; onConfirm: () => void }) {
  const { t } = useI18n();
  useEffect(() => { const handleKeyDown = (event: KeyboardEvent) => { if (event.key === "Escape" && !busy) { event.preventDefault(); event.stopPropagation(); onCancel(); } }; document.addEventListener("keydown", handleKeyDown, true); return () => document.removeEventListener("keydown", handleKeyDown, true); }, [busy, onCancel]);
  const title = state.remote ? t("sidebar.deleteRemoteBranch") : t("sidebar.deleteLocalBranch");
  const description = state.remote
    ? t("sidebar.deleteBranchDescription", { branch: state.branch })
    : state.forceLinkedWorktree
      ? t("sidebar.forceDeleteBranchWorktreeDescription", { branch: state.branch })
      : state.linkedWorktreePath
        ? t("sidebar.deleteBranchAndWorktreeDescription", { branch: state.branch })
        : t("sidebar.deleteBranchDescription", { branch: state.branch });
  return (
    <div role="presentation" onClick={(event) => { if (!busy && event.target === event.currentTarget) onCancel(); }} style={{ position: "fixed", inset: 0, zIndex: 1100, display: "flex", alignItems: "center", justifyContent: "center", padding: 16, background: "rgba(0,0,0,.4)" }}>
      <div role="dialog" aria-modal="true" aria-labelledby="delete-branch-title" style={{ width: 360, maxWidth: "100%", border: "1px solid var(--border)", borderRadius: 8, background: "var(--bg-panel)", boxShadow: "0 12px 36px rgba(0,0,0,.24)" }}>
        <div style={{ padding: "16px 18px" }}>
          <div id="delete-branch-title" style={{ color: "var(--text)", fontSize: 14, fontWeight: 700 }}>{title}</div>
          <div style={{ marginTop: 8, color: "var(--text-muted)", fontSize: 12, lineHeight: 1.5, overflowWrap: "anywhere" }}>{description}</div>
          {error && <div role="alert" style={{ marginTop: 10, color: "#ef4444", fontSize: 11, lineHeight: 1.45 }}>{error}</div>}
        </div>
        <div style={{ display: "flex", justifyContent: "flex-end", gap: 6, padding: "10px 18px", borderTop: "1px solid var(--border)" }}>
          <button type="button" onClick={onCancel} disabled={busy} style={{ height: 30, padding: "0 10px", border: "none", borderRadius: 5, background: "transparent", color: "var(--text-muted)", cursor: busy ? "not-allowed" : "pointer", fontSize: 12 }}>{t("sidebar.cancel")}</button>
          <button type="button" onClick={onConfirm} disabled={busy} style={{ height: 30, padding: "0 10px", border: "none", borderRadius: 5, background: "#dc2626", color: "#fff", cursor: busy ? "not-allowed" : "pointer", opacity: busy ? .6 : 1, fontSize: 12, fontWeight: 600 }}>{t("sidebar.delete")}</button>
        </div>
      </div>
    </div>
  );
}

function MergeBranchDialog({ source, branches, value, busy, error, onChange, onCancel, onConfirm }: { source: string; branches: string[]; value: string; busy: boolean; error: string | null; onChange: (value: string) => void; onCancel: () => void; onConfirm: () => void }) {
  const { t } = useI18n();
  const targets = branches.filter((branch) => branch !== source);
  useEffect(() => { const handleKeyDown = (event: KeyboardEvent) => { if (event.key === "Escape" && !busy) { event.preventDefault(); event.stopPropagation(); onCancel(); } }; document.addEventListener("keydown", handleKeyDown, true); return () => document.removeEventListener("keydown", handleKeyDown, true); }, [busy, onCancel]);
  return (
    <div role="presentation" onClick={(event) => { if (!busy && event.target === event.currentTarget) onCancel(); }} style={{ position: "fixed", inset: 0, zIndex: 1100, display: "flex", alignItems: "center", justifyContent: "center", padding: 16, background: "rgba(0,0,0,.4)" }}>
      <div role="dialog" aria-modal="true" aria-labelledby="merge-branch-title" style={{ width: 360, maxWidth: "100%", border: "1px solid var(--border)", borderRadius: 8, background: "var(--bg-panel)", boxShadow: "0 12px 36px rgba(0,0,0,.24)" }}>
        <div style={{ padding: "16px 18px" }}>
          <div id="merge-branch-title" style={{ color: "var(--text)", fontSize: 14, fontWeight: 700 }}>{t("sidebar.mergeBranchTitle")}</div>
          <div style={{ marginTop: 8, color: "var(--text-muted)", fontSize: 12, lineHeight: 1.5 }}>{t("sidebar.mergeBranchDescription", { branch: source })}</div>
          <label style={{ display: "block", marginTop: 14, color: "var(--text-dim)", fontSize: 11 }}>{t("sidebar.mergeTargetBranch")}</label>
          <div style={{ marginTop: 6 }}><BranchPicker branches={targets} value={value} disabled={busy} placeholder={t("sidebar.selectBranch")} onChange={onChange} /></div>
          {error && <div role="alert" style={{ marginTop: 10, color: "#ef4444", fontSize: 11, lineHeight: 1.45 }}>{error}</div>}
        </div>
        <div style={{ display: "flex", justifyContent: "flex-end", gap: 6, padding: "10px 18px", borderTop: "1px solid var(--border)" }}>
          <button type="button" onClick={onCancel} disabled={busy} style={{ height: 30, padding: "0 10px", border: "none", borderRadius: 5, background: "transparent", color: "var(--text-muted)", cursor: busy ? "not-allowed" : "pointer", fontSize: 12 }}>{t("sidebar.cancel")}</button>
          <button type="button" onClick={onConfirm} disabled={busy || !value} style={{ height: 30, padding: "0 10px", border: "none", borderRadius: 5, background: "var(--accent)", color: "#fff", cursor: busy || !value ? "not-allowed" : "pointer", opacity: busy || !value ? .6 : 1, fontSize: 12, fontWeight: 600 }}>{t("sidebar.merge")}</button>
        </div>
      </div>
    </div>
  );
}

function SessionTreeItem({
  node,
  selectedSessionId,
  runningSessionIds,
  unreadSessionIds,
  onSelectSession,
  onRenamed,
  onSessionDeleted,
  onGenerateTitle,
  titleGenerationStatus,
  searchMatches,
  depth,
}: {
  node: SessionTreeNode;
  selectedSessionId: string | null;
  runningSessionIds: Set<string>;
  unreadSessionIds: Set<string>;
  onSelectSession: (s: SessionInfo) => void;
  onRenamed?: () => void;
  onSessionDeleted?: (id: string) => void;
  onGenerateTitle?: (sessionId: string) => void;
  titleGenerationStatus?: Props["titleGenerationStatus"];
  searchMatches: ReadonlyMap<string, string>;
  depth: number;
}) {
  const [collapsed, setCollapsed] = useState(false);
  const hasChildren = node.children.length > 0;
  const searchMatch = searchMatches.get(node.session.id);

  return (
    <div>
      <SessionItem
        session={node.session}
        isSelected={node.session.id === selectedSessionId}
        isRunning={runningSessionIds.has(node.session.id)}
        isUnread={unreadSessionIds.has(node.session.id)}
        onClick={() => onSelectSession(node.session)}
        onRenamed={onRenamed}
        onDeleted={onSessionDeleted}
        onGenerateTitle={onGenerateTitle}
        titleGenerationStatus={titleGenerationStatus}
        searchMatch={searchMatch}
        depth={depth}
        hasChildren={hasChildren}
        collapsed={collapsed}
        onToggleCollapse={() => setCollapsed((v) => !v)}
      />
      {hasChildren && !collapsed && (
        <div>
          {node.children.map((child) => (
            <SessionTreeItem
              key={child.session.id}
              node={child}
              selectedSessionId={selectedSessionId}
              runningSessionIds={runningSessionIds}
              unreadSessionIds={unreadSessionIds}
              onSelectSession={onSelectSession}
              onRenamed={onRenamed}
              onSessionDeleted={onSessionDeleted}
              onGenerateTitle={onGenerateTitle}
              titleGenerationStatus={titleGenerationStatus}
              searchMatches={searchMatches}
              depth={depth + 1}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function RunningSessionIndicator() {
  const { t } = useI18n();
  return (
    <span
      title={t("sidebar.agentRunning")}
      aria-label={t("sidebar.agentRunning")}
      style={{
        width: 14,
        height: 14,
        display: "inline-flex",
        alignItems: "center",
        justifyContent: "center",
        flexShrink: 0,
        color: "var(--accent)",
      }}
    >
      <LoaderCircle className={styles.spinner} size={13} strokeWidth={ICON_STROKE} aria-hidden="true" />
    </span>
  );
}

function UnreadSessionIndicator() {
  const { t } = useI18n();
  return (
    <span
      title={t("sidebar.newActivity")}
      aria-label={t("sidebar.newSessionActivity")}
      style={{
        width: 14,
        height: 14,
        display: "inline-flex",
        alignItems: "center",
        justifyContent: "center",
        flexShrink: 0,
        color: "#0891b2",
      }}
    >
      <span className={styles.unreadDot} aria-hidden="true" />
    </span>
  );
}

/**
 * Compact per-project activity badges for the workspace selector dropdown items:
 * a speech (person speaking) + count when a session is waiting for an answer,
 * a spinning running icon + count, and an unread dot + count. Renders nothing when the
 * project has no activity. Counts share the accent / unread colors of the
 * per-session indicators so the two stay visually consistent.
 */
function showProjectActivity(
  activity: { running: number; unread: number; awaiting: number } | undefined,
  t: (key: string) => string,
): ReactNode {
  if (!activity || (activity.awaiting === 0 && activity.running === 0 && activity.unread === 0)) return null;
  return (
    <span style={{ display: "inline-flex", alignItems: "center", gap: 5, flexShrink: 0, marginLeft: 6 }}>
      {activity.awaiting > 0 && (
        <span
          title={t("sidebar.waitingForUser")}
          aria-label={`${t("sidebar.waitingForUser")} (${activity.awaiting})`}
          style={{ display: "inline-flex", alignItems: "center", gap: 3, color: "#d97706", fontSize: 10, fontFamily: "var(--font-mono)" }}
        >
          <Speech size={10} strokeWidth={2.5} aria-hidden="true" />
          {activity.awaiting}
        </span>
      )}
      {activity.running > 0 && (
        <span
          title={t("sidebar.agentRunning")}
          aria-label={`${t("sidebar.agentRunning")} (${activity.running})`}
          style={{ display: "inline-flex", alignItems: "center", gap: 3, color: "var(--accent)", fontSize: 10, fontFamily: "var(--font-mono)" }}
        >
          <svg width="10" height="10" viewBox="0 0 24 24" fill="none" aria-hidden="true" style={{ display: "block" }}>
            <g>
              <path d="M21 12a9 9 0 1 1-3.8-7.4" stroke="currentColor" strokeWidth="2.8" strokeLinecap="round" />
              <animateTransform attributeName="transform" type="rotate" from="0 12 12" to="360 12 12" dur="0.9s" repeatCount="indefinite" />
            </g>
          </svg>
          {activity.running}
        </span>
      )}
      {activity.unread > 0 && (
        <span
          title={t("sidebar.newSessionActivity")}
          aria-label={`${t("sidebar.newSessionActivity")} (${activity.unread})`}
          style={{ display: "inline-flex", alignItems: "center", gap: 3, color: "#0891b2", fontSize: 10, fontFamily: "var(--font-mono)" }}
        >
          <span style={{ width: 6, height: 6, borderRadius: "50%", background: "currentColor", display: "inline-block" }} />
          {activity.unread}
        </span>
      )}
    </span>
  );
}

function SessionItem({
  session,
  isSelected,
  isRunning,
  isUnread,
  onClick,
  onRenamed,
  onDeleted,
  onGenerateTitle,
  titleGenerationStatus,
  searchMatch,
  depth = 0,
  hasChildren = false,
  collapsed = false,
  onToggleCollapse,
}: {
  session: SessionInfo;
  isSelected: boolean;
  isRunning?: boolean;
  isUnread?: boolean;
  onClick: () => void;
  onRenamed?: () => void;
  onDeleted?: (id: string) => void;
  onGenerateTitle?: (sessionId: string) => void;
  titleGenerationStatus?: Props["titleGenerationStatus"];
  searchMatch?: string;
  depth?: number;
  hasChildren?: boolean;
  collapsed?: boolean;
  onToggleCollapse?: () => void;
}) {
  const { t } = useI18n();
  const [renaming, setRenaming] = useState(false);
  const [renameValue, setRenameValue] = useState("");
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  // Select the whole name once the rename input is mounted (startRename's
  // immediate setTimeout can fire before the input exists).
  useEffect(() => {
    if (renaming) {
      const id = requestAnimationFrame(() => inputRef.current?.select());
      return () => cancelAnimationFrame(id);
    }
  }, [renaming]);

  // A stored first message may be an SDK-expanded <skill> block; collapse it
  // back to the compact /skill:name args command the user typed before using
  // it as the auto-name fallback, mirroring MessageView's rendering.
  const displayFirstMessage = skillExpansionToCommand(session.firstMessage) ?? session.firstMessage;
  const title = session.name || displayFirstMessage.slice(0, 50) || session.id.slice(0, 12);
  const titleStatus = titleGenerationStatus?.sessionId === session.id ? titleGenerationStatus : null;
  const titleGenerationBusy = titleGenerationStatus?.kind === "naming";
  const canGenerateTitle = session.messageCount > 0 && !titleGenerationBusy;

  const startRename = useCallback((e: React.MouseEvent) => {
    e.stopPropagation();
    if (session.transient) return;
    setRenameValue(session.name || displayFirstMessage.slice(0, 50) || session.id.slice(0, 12));
    setRenaming(true);
  }, [session.name, session.transient, displayFirstMessage, session.id]);

  const commitRename = useCallback(async () => {
    const name = renameValue.trim();
    setRenaming(false);
    // No-op when unchanged: the fallback title (first message / id) isn't a
    // real stored name, so don't persist it as one. (The rename input seeds
    // from the same collapsed displayFirstMessage, so an untouched rename of
    // a skill-invoked session stays a no-op instead of persisting raw XML.)
    if (renameValue === title || name === (session.name ?? "")) return;
    try {
      await fetch(`/api/sessions/${encodeURIComponent(session.id)}`, {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name }),
      });
      onRenamed?.();
    } catch {
      // ignore
    }
  }, [renameValue, session.id, session.name, onRenamed, title]);

  const performDelete = useCallback(async () => {
    if (session.transient) return;
    setConfirmDelete(false);
    setDeleting(true);
    try {
      await fetch(`/api/sessions/${encodeURIComponent(session.id)}`, { method: "DELETE" });
      onDeleted?.(session.id);
    } catch {
      setDeleting(false);
    }
  }, [session.id, session.transient, onDeleted]);

  const handleDeleteClick = useCallback((e: React.MouseEvent) => {
    e.stopPropagation();
    if (e.shiftKey) {
      void performDelete();
    } else {
      setConfirmDelete(true);
    }
  }, [performDelete]);

  const handleDeleteConfirm = useCallback((e: React.MouseEvent) => {
    e.stopPropagation();
    void performDelete();
  }, [performDelete]);

  const handleDeleteCancel = useCallback((e: React.MouseEvent) => {
    e.stopPropagation();
    setConfirmDelete(false);
  }, []);

  const handleContextMenu = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    const handled = dispatchSessionRowContextMenu({
      id: session.id,
      path: session.path,
      cwd: session.cwd,
      name: session.name,
      clientX: e.clientX,
      clientY: e.clientY,
      refresh: () => { onRenamed?.(); },
    });
    if (!handled) return;
    e.preventDefault();
    e.stopPropagation();
  }, [onRenamed, session.cwd, session.id, session.name, session.path]);

  return (
    <div
      className={styles.sessionRow}
      data-current={isSelected}
      data-delete={confirmDelete}
      data-deleting={deleting}
      data-forks={hasChildren}
      style={{ "--fork-indent": `${Math.min(depth, 3) * 10}px` } as CSSProperties}
      onContextMenu={confirmDelete || renaming ? undefined : handleContextMenu}
    >
      {confirmDelete ? (
        /* ── Delete confirmation: same height, two flat buttons ── */
        <>
          <div style={{ flex: 1, minWidth: 0, fontSize: 12, color: "var(--text)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
            {t("sidebar.deleteSession", { title: title.slice(0, 22) + (title.length > 22 ? "…" : "") })}
          </div>
          <div className={styles.deleteActions}>
            <button type="button" onClick={handleDeleteConfirm} className={styles.confirmDelete}>
              {t("sidebar.delete")}
            </button>
            <button type="button" onClick={handleDeleteCancel} className={styles.cancelDelete} autoFocus>
              {t("sidebar.cancel")}
            </button>
          </div>
        </>
      ) : renaming ? (
        /* ── Rename: input fills the same row ── */
        <input
          ref={inputRef}
          value={renameValue}
          onChange={(e) => setRenameValue(e.target.value)}
          onBlur={commitRename}
          onKeyDown={(e) => {
            if (e.key === "Enter") commitRename();
            if (e.key === "Escape") setRenaming(false);
          }}
          aria-label={t("sidebar.rename")}
          className={styles.renameInput}
          autoFocus
        />
      ) : (
        <>
          <button type="button" className={styles.sessionTarget} onClick={onClick} aria-current={isSelected ? "true" : undefined} disabled={deleting}>
            {depth > 0
              ? <GitBranch size={14} strokeWidth={ICON_STROKE} className={styles.sessionGlyph} aria-hidden="true" />
              : <MessageSquare size={14} strokeWidth={ICON_STROKE} className={styles.sessionGlyph} aria-hidden="true" />}
            <span className={styles.sessionCopy}>
              <span className={styles.sessionTitle} title={title}>{title}</span>
              <span className={styles.sessionMeta}>
                {searchMatch ? (
                  <span title={searchMatch}>{searchMatch}</span>
                ) : isRunning ? (
                  <RunningSessionIndicator />
                ) : isUnread ? (
                  <UnreadSessionIndicator />
                ) : (
                  <time dateTime={session.modified} title={session.modified}>{formatRelativeTime(session.modified)}</time>
                )}
                {!searchMatch && <span>{t("sidebar.messagesCount", { count: session.messageCount })}</span>}
                {!searchMatch && session.worktreeBranch && (
                  <span title={session.cwd} className={styles.sessionBranch}>
                    <GitBranch size={10} strokeWidth={ICON_STROKE} aria-hidden="true" />
                    <span>{session.worktreeBranch}</span>
                  </span>
                )}
              </span>
            </span>
          </button>

          {hasChildren && (
            <button
              type="button"
              className={`${styles.iconButton} ${styles.forkToggle}`}
              onClick={onToggleCollapse}
              aria-expanded={!collapsed}
              aria-label={t(collapsed ? "sidebar.expandForks" : "sidebar.collapseForks")}
              title={t(collapsed ? "sidebar.expandForks" : "sidebar.collapseForks")}
            >
              <ChevronDown size={13} strokeWidth={ICON_STROKE} aria-hidden="true" />
            </button>
          )}

          {/* 操作浮层不参与桌面行布局，鼠标移入时标题不会突然缩短。 */}
          {!session.transient && !deleting && (
            <div className={styles.sessionActions} data-status={titleStatus?.kind}>
              <button
                type="button"
                className={styles.iconButton}
                onClick={() => {
                  if (canGenerateTitle) onGenerateTitle?.(session.id);
                }}
                disabled={!canGenerateTitle}
                title={titleStatus?.kind === "error"
                  ? titleStatus.message ?? t("title.failed")
                  : session.messageCount === 0
                    ? t("title.noMessages")
                    : titleStatus?.kind === "naming"
                      ? t("title.generating")
                      : titleStatus?.kind === "success"
                        ? t("title.updated")
                        : t("title.generateSession")}
                aria-label={titleStatus?.kind === "naming" ? t("title.generating") : t("title.generate")}
              >
                {titleStatus?.kind === "naming"
                  ? <LoaderCircle className={styles.spinner} size={14} strokeWidth={ICON_STROKE} aria-hidden="true" />
                  : titleStatus?.kind === "success"
                    ? <Check size={14} strokeWidth={ICON_STROKE} aria-hidden="true" />
                    : <WandSparkles size={14} strokeWidth={ICON_STROKE} aria-hidden="true" />}
              </button>
              <button type="button" className={styles.iconButton} onClick={startRename} title={t("sidebar.rename")} aria-label={t("sidebar.rename")}>
                <Pencil size={14} strokeWidth={ICON_STROKE} aria-hidden="true" />
              </button>
              <button type="button" className={`${styles.iconButton} ${styles.deleteIcon}`} onClick={handleDeleteClick} title={t("sidebar.deleteWithShiftClick")} aria-label={t("sidebar.delete")}>
                <Trash2 size={14} strokeWidth={ICON_STROKE} aria-hidden="true" />
              </button>
            </div>
          )}
        </>
      )}
    </div>
  );
}
