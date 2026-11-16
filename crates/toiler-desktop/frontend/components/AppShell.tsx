"use client";

import { useState, useCallback, useRef, useEffect, useLayoutEffect, useMemo } from "react";
import dynamic from "next/dynamic";
import { useRouter, useSearchParams } from "next/navigation";
import { useGlobalKeyboardShortcuts } from "@/hooks/useKeyboardShortcuts";
import { SessionSidebar } from "./SessionSidebar";
import { ChatWindow } from "./ChatWindow";
import { TabBar, type Tab } from "./TabBar";
import { openFileTab, markFileTabRevealHandled, renameFileTab, saveFileViewerState, saveFileDirtyState } from "./file-tab-state";
import { ProjectTrustDialog } from "./ProjectTrustDialog";
import { SettingsPanel } from "./SettingsPanel";
import { AboutDialog } from "./AboutDialog";
import { BranchNavigator, ConversationBranchesIcon } from "./BranchNavigator";
import { SystemPromptPanel } from "./SystemPromptPanel";
import { GitPanel } from "./GitPanel";
import { useTheme, resolveTheme, setThemeState } from "@/hooks/useTheme";
import { useI18n } from "@/hooks/useI18n";
import { useIsMobile } from "@/hooks/useIsMobile";
import { useViewportHeight } from "@/hooks/useViewportHeight";
import { useResizablePanel } from "@/hooks/useResizablePanel";
import { useAudio } from "@/hooks/useAudio";
import { copyText } from "@/lib/clipboard";
import { getBannerEnabled, setBannerEnabled } from "@/lib/banner-preference";
import { getFileName } from "@/lib/file-paths";
import type { FileOpenLocation } from "@/lib/file-links";
import { buildAtMentionText, buildFileAtMentionsText, buildFileLineMentionText } from "@/lib/file-fuzzy";
import {
  claimExtensionAttentionNotification,
  shouldShowBrowserNotification,
  showBrowserNotification,
} from "@/lib/browser-notifications";
import { isDesktopShell, showDesktopNotification } from "@/lib/desktop-notifications";
import { getInitialNavigation } from "@/lib/initial-navigation";
import {
  clearLastOpen,
  getLastOpenSession,
  setLastOpenSession,
  workspaceKeyOf,
} from "@/lib/workspace-memory";
import {
  getDefaultRightPanelWidth,
  getRightPanelMaxWidth,
  getSidebarMaxWidth,
  CHAT_TOP_BAR_HEIGHT,
  RIGHT_PANEL_FALLBACK_WIDTH,
  RIGHT_PANEL_MAX_WIDTH,
  RIGHT_PANEL_MIN_WIDTH,
  SIDEBAR_DEFAULT_WIDTH,
  SIDEBAR_MAX_WIDTH,
  SIDEBAR_MIN_WIDTH,
} from "@/lib/panel-layout";
import type { BlockingExtensionUiRequest, SessionInfo, SessionTreeNode } from "@/lib/types";
import type { AppUpdateResponse, ProjectTrustStatus } from "@/lib/api-types";
import type { ChatInputHandle } from "./ChatInput";
import type { SessionStatsInfo } from "@/lib/pi-types";
import { chatLinkModeHint, type FileViewerState } from "@/lib/file-viewer-state";
import { skillExpansionToCommand } from "@/lib/slash-display";
import { FileViewer } from "./FileViewer";

const TerminalPanel = dynamic(() => import("./TerminalPanel").then((module) => module.TerminalPanel), { ssr: false });

type SessionCopyField = "file" | "id";
type AutoNameStatus =
  | { kind: "idle" }
  | { kind: "naming"; sessionId: string }
  | { kind: "success"; sessionId: string }
  | { kind: "error"; sessionId: string; message: string };
// 方图标键的命中区。以前它等于工具条高度，工具条从 36 提到 42 后就会跟着变成
// 42×42——手机上那是好处，桌面上会把 26px 的按键静默放大到 42。所以这里解耦：
// 固定 34，工具条高度另外算。
const TOP_BAR_ICON_BUTTON_SIZE = 34;
const NOTIFICATION_ICON = "/icons/icon-192.png";
const NOTIFICATION_PROMPT_MAX_LENGTH = 100;
const RIGHT_PANEL_TRANSITION_MS = 200;
// 会话还没有首条 user 消息时服务端给的占位文案（与 `session_view::first_message_text`
// / `sessions_store::read_summary` 一致）。它不是真正的标题。
const NO_FIRST_MESSAGE_PLACEHOLDER = "(no messages)";

function isPlaceholderFirstMessage(text: string | undefined): boolean {
  return !text || text === NO_FIRST_MESSAGE_PLACEHOLDER;
}

function preserveChatScrollDuringPanelTransition(container: HTMLDivElement): () => void {
  const content = container.querySelector<HTMLElement>("[data-chat-message-content]");
  const containerRect = container.getBoundingClientRect();
  const anchor = content
    ? Array.from(content.children).find((child): child is HTMLElement => {
        if (!(child instanceof HTMLElement)) return false;
        const rect = child.getBoundingClientRect();
        return rect.height > 0 && rect.bottom > containerRect.top && rect.top < containerRect.bottom;
      }) ?? null
    : null;
  const anchorOffset = anchor
    ? anchor.getBoundingClientRect().top - containerRect.top
    : null;
  const fallbackScrollTop = container.scrollTop;
  const startedAt = performance.now();
  let frameId: number | null = null;
  let cancelled = false;

  const restore = () => {
    if (cancelled || !container.isConnected) return;
    if (anchor?.isConnected && anchorOffset !== null) {
      const currentOffset = anchor.getBoundingClientRect().top - container.getBoundingClientRect().top;
      container.scrollTop += currentOffset - anchorOffset;
    } else {
      container.scrollTop = fallbackScrollTop;
    }
    if (performance.now() - startedAt <= RIGHT_PANEL_TRANSITION_MS + 100) {
      frameId = requestAnimationFrame(restore);
    }
  };

  frameId = requestAnimationFrame(restore);
  return () => {
    cancelled = true;
    if (frameId !== null) cancelAnimationFrame(frameId);
  };
}

function summarizeNotificationPrompt(prompt: string | null): string | null {
  if (!prompt) return null;
  const singleLine = prompt.replace(/\s+/g, " ").trim();
  if (!singleLine) return null;
  return singleLine.length > NOTIFICATION_PROMPT_MAX_LENGTH
    ? `${singleLine.slice(0, NOTIFICATION_PROMPT_MAX_LENGTH - 1)}...`
    : singleLine;
}

export function AppShell() {
  const router = useRouter();
  const searchParams = useSearchParams();
  const [initialNavigation] = useState(() => getInitialNavigation(searchParams));
  const { theme, preference, toggleTheme } = useTheme();
  const themeLabelKey =
    preference === "light" ? "theme.light" : preference === "dark" ? "theme.dark" : "theme.auto";
  const { locale, setLocale, t: translate, supportedLocales } = useI18n();
  const isMobile = useIsMobile();
  useViewportHeight();
  // Audio ownership lives here (not in ChatWindow) so the completion tone can
  // also fire for tasks finishing in a non-active workspace whose ChatWindow
  // is not mounted. ChatWindow receives the audio callbacks as props.
  const { soundEnabled, onSoundToggle, playDoneSound, unlockAudio, soundEnabledRef } = useAudio();
  const notifiedAttentionRequestIdsRef = useRef(new Set<string>());
  const [selectedSession, setSelectedSession] = useState<SessionInfo | null>(null);
  const [runningSessionIds, setRunningSessionIds] = useState<Set<string>>(() => new Set());
  const handleRunningSessionIdsChange = useCallback((ids: Set<string>) => {
    setRunningSessionIds((previous) => {
      if (previous.size === ids.size && [...ids].every((id) => previous.has(id))) return previous;
      return ids;
    });
  }, []);
  // The temporary id distinguishes consecutive fresh composers in one cwd.
  const [newSessionCwd, setNewSessionCwd] = useState<string | null>(null);
  const [newSessionDraftId, setNewSessionDraftId] = useState("initial");
  const activeNewSessionDraftKeyRef = useRef<string | null>(null);
  const [initialCwdStatus, setInitialCwdStatus] = useState<"idle" | "validating" | "ready" | "error">(
    () => initialNavigation.requestedCwd ? "validating" : "idle",
  );
  const [initialCwdError, setInitialCwdError] = useState<string | null>(null);
  const [refreshKey, setRefreshKey] = useState(0);
  const [sessionKey, setSessionKey] = useState(0);
  const [explorerRefreshKey, setExplorerRefreshKey] = useState(0);
  const [modelsRefreshKey, setModelsRefreshKey] = useState(0);
  const [settingsOpen, setSettingsOpen] = useState(false);
  /** 版本信息（ⓘ）。侧边栏标题行与设置 → 常规 共用同一个对话框。 */
  const [aboutOpen, setAboutOpen] = useState(false);
  const [bannerEnabled, setBannerEnabledState] = useState(true);
  const [gitPanelOpen, setGitPanelOpen] = useState(false);
  /**
   * Git 面板的仓库根。`null` = 跟随当前项目；一个项目里可以有多个各自带 `.git`
   * 的子项目，此时它指向文件浏览器里那颗被点击的 Git 徽标所在的子目录。
   */
  const [gitPanelCwd, setGitPanelCwd] = useState<string | null>(null);
  const [activeProjectIsGit, setActiveProjectIsGit] = useState<boolean | null>(null);
  const [projectTrust, setProjectTrust] = useState<ProjectTrustStatus | null>(null);
  const [projectTrustDialogOpen, setProjectTrustDialogOpen] = useState(false);
  const [projectTrustBusy, setProjectTrustBusy] = useState(false);
  const [projectTrustError, setProjectTrustError] = useState<string | null>(null);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [rightPanelOpen, setRightPanelOpen] = useState(false);
  const chatScrollContainerRef = useRef<HTMLDivElement | null>(null);
  const cancelChatScrollPreservationRef = useRef<(() => void) | null>(null);
  const [terminalCwds, setTerminalCwds] = useState<string[]>([]);
  const [consoleOpen, setConsoleOpen] = useState(false);
  const [openDirectoryRequest, setOpenDirectoryRequest] = useState(0);
  const [mobileToolbarMoreOpen, setMobileToolbarMoreOpen] = useState(false);
  const [mobileSidebarReady, setMobileSidebarReady] = useState(false);
  const [appUpdate, setAppUpdate] = useState<AppUpdateResponse | null>(null);

  const checkAppUpdate = useCallback(async (refresh = false) => {
    try {
      const response = await fetch(`/api/app-update${refresh ? "?refresh=1" : ""}`, { cache: "no-store" });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const value = await response.json() as AppUpdateResponse;
      setAppUpdate(value);
    } catch {
      // Update checks are best-effort; don't disturb the rest of the shell.
    }
  }, []);

  useEffect(() => {
    void checkAppUpdate(true);
  }, [checkAppUpdate]);

  const openAppUpdate = useCallback(() => {
    if (!appUpdate?.updateAvailable) return;
    if (window.__PI_WEB_API_ORIGIN__) {
      void import("@tauri-apps/api/core")
        .then(({ invoke }) => invoke("open_release_url", { url: appUpdate.releaseUrl }))
        .catch(() => {
          window.location.href = appUpdate.releaseUrl;
        });
      return;
    }
    window.open(appUpdate.releaseUrl, "_blank", "noopener,noreferrer");
  }, [appUpdate]);

  useEffect(() => {
    setBannerEnabledState(getBannerEnabled());
  }, []);
  const sidebarWidthRef = useRef(SIDEBAR_DEFAULT_WIDTH);
  const rightPanelWidthRef = useRef(RIGHT_PANEL_FALLBACK_WIDTH);
  const getResponsiveRightPanelWidth = useCallback(
    () => typeof window === "undefined"
      ? RIGHT_PANEL_FALLBACK_WIDTH
      : getDefaultRightPanelWidth(window.innerWidth),
    [],
  );
  const getResponsiveSidebarMaxWidth = useCallback(
    () => typeof window === "undefined"
      ? SIDEBAR_MAX_WIDTH
      : getSidebarMaxWidth({
        viewportWidth: window.innerWidth,
        rightPanelOpen,
        rightPanelWidth: rightPanelWidthRef.current,
      }),
    [rightPanelOpen],
  );
  const getResponsiveRightPanelMaxWidth = useCallback(
    () => typeof window === "undefined"
      ? RIGHT_PANEL_MAX_WIDTH
      : getRightPanelMaxWidth({
        viewportWidth: window.innerWidth,
        sidebarOpen,
        sidebarWidth: sidebarWidthRef.current,
      }),
    [sidebarOpen],
  );
  const sidebarResizer = useResizablePanel({
    ariaLabel: translate("layout.resizeSidebar"),
    cssVariable: "--sidebar-width",
    defaultWidth: SIDEBAR_DEFAULT_WIDTH,
    getMaxWidth: getResponsiveSidebarMaxWidth,
    growthDirection: "right",
    maxWidth: SIDEBAR_MAX_WIDTH,
    minWidth: SIDEBAR_MIN_WIDTH,
    storageKey: "pi-sidebar-width",
    widthRef: sidebarWidthRef,
  });
  const rightPanelResizer = useResizablePanel({
    ariaLabel: translate("layout.resizeFilePanel"),
    cssVariable: "--right-panel-width",
    defaultWidth: RIGHT_PANEL_FALLBACK_WIDTH,
    getDefaultWidth: getResponsiveRightPanelWidth,
    getMaxWidth: getResponsiveRightPanelMaxWidth,
    growthDirection: "left",
    maxWidth: RIGHT_PANEL_MAX_WIDTH,
    minWidth: RIGHT_PANEL_MIN_WIDTH,
    storageKey: "pi-right-panel-width",
    widthRef: rightPanelWidthRef,
  });
  const reclampSidebarWidth = sidebarResizer.reclampWidth;
  const reclampRightPanelWidth = rightPanelResizer.reclampWidth;
  // On mobile the sidebar is an overlay drawer; hide it by default so the chat
  // is visible on load. Runs once the breakpoint resolves after hydration.
  useEffect(() => {
    if (isMobile) setSidebarOpen(false);
  }, [isMobile]);
  useEffect(() => {
    setMobileSidebarReady(true);
  }, []);
  useEffect(() => {
    if (!rightPanelOpen) return;
    reclampSidebarWidth();
    reclampRightPanelWidth();
  }, [reclampRightPanelWidth, reclampSidebarWidth, rightPanelOpen]);
  const chatInputRef = useRef<ChatInputHandle | null>(null);
  const handleChatScrollContainerChange = useCallback((container: HTMLDivElement | null) => {
    chatScrollContainerRef.current = container;
  }, []);

  useEffect(() => () => cancelChatScrollPreservationRef.current?.(), []);
  const topBarRef = useRef<HTMLDivElement>(null);
  const mobileToolbarRef = useRef<HTMLDivElement>(null);

  // Branch navigator state — populated by ChatWindow via onBranchDataChange
  const [branchTree, setBranchTree] = useState<SessionTreeNode[]>([]);
  const [branchActiveLeafId, setBranchActiveLeafId] = useState<string | null>(null);
  const branchLeafChangeFnRef = useRef<((leafId: string | null) => void) | null>(null);

  const handleBranchDataChange = useCallback((tree: SessionTreeNode[], activeLeafId: string | null, onLeafChange: (leafId: string | null) => void) => {
    setBranchTree(tree);
    setBranchActiveLeafId(activeLeafId);
    branchLeafChangeFnRef.current = onLeafChange;
  }, []);

  const handleBranchLeafChange = useCallback((leafId: string | null) => {
    branchLeafChangeFnRef.current?.(leafId);
  }, []);

  const [systemPrompt, setSystemPrompt] = useState<string | null>(null);
  const [systemPromptLoading, setSystemPromptLoading] = useState(false);
  const systemPromptLoaderRef = useRef<(() => Promise<void>) | null>(null);
  const systemPromptLoadIdRef = useRef(0);
  const systemBtnRef = useRef<HTMLButtonElement>(null);

  const handleSystemPromptChange = useCallback((prompt: string | null) => {
    setSystemPrompt(prompt);
    setSystemPromptLoading(false);
  }, []);

  const handleSystemPromptLoaderChange = useCallback((loader: (() => Promise<void>) | null) => {
    systemPromptLoadIdRef.current += 1;
    systemPromptLoaderRef.current = loader;
    setSystemPromptLoading(false);
  }, []);

  // Session stats (tokens + cost) — populated by ChatWindow, displayed in top bar
  const [sessionStats, setSessionStats] = useState<SessionStatsInfo | null>(null);
  const [autoNameStatus, setAutoNameStatus] = useState<AutoNameStatus>({ kind: "idle" });
  const autoNameTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const activeSessionIdRef = useRef<string | null>(selectedSession?.id ?? null);
  activeSessionIdRef.current = selectedSession?.id ?? null;
  const handleSessionStatsChange = useCallback((stats: SessionStatsInfo | null) => {
    setSessionStats(stats);
  }, []);
  const [copiedSessionField, setCopiedSessionField] = useState<SessionCopyField | null>(null);
  const sessionCopyTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const handleCopySessionField = useCallback((field: SessionCopyField, value: string) => {
    void copyText(value).then(() => {
      if (sessionCopyTimerRef.current) clearTimeout(sessionCopyTimerRef.current);
      setCopiedSessionField(field);
      sessionCopyTimerRef.current = setTimeout(() => setCopiedSessionField(null), 1400);
    });
  }, []);

  useEffect(() => {
    return () => {
      if (sessionCopyTimerRef.current) clearTimeout(sessionCopyTimerRef.current);
      if (autoNameTimerRef.current) clearTimeout(autoNameTimerRef.current);
    };
  }, []);

  // Context usage — populated by ChatWindow, displayed in top bar
  const [contextUsage, setContextUsage] = useState<{ percent: number | null; contextWindow: number; tokens: number | null } | null>(null);
  const handleContextUsageChange = useCallback((usage: { percent: number | null; contextWindow: number; tokens: number | null } | null) => {
    setContextUsage(usage);
  }, []);

  // Single active panel — only one dropdown open at a time
  const [activeTopPanel, setActiveTopPanel] = useState<"branches" | "system" | "session" | "language" | null>(null);
  const [topPanelPos, setTopPanelPos] = useState<{ top: number; left: number; width: number } | null>(null);

  const toggleTopPanel = useCallback((
    panel: "branches" | "system" | "session" | "language",
    keepMobileToolbarOpen = false,
  ) => {
    if (isMobile) setSidebarOpen(false);
    setActiveTopPanel((cur) => cur === panel ? null : panel);
    if (isMobile && keepMobileToolbarOpen) setMobileToolbarMoreOpen(true);
  }, [isMobile]);

  const handleSystemPromptToggle = useCallback((keepMobileToolbarOpen = false) => {
    const opening = activeTopPanel !== "system";
    toggleTopPanel("system", keepMobileToolbarOpen);
    if (!opening || systemPromptLoading) return;

    const load = systemPromptLoaderRef.current;
    if (!load) return;
    const loadId = ++systemPromptLoadIdRef.current;
    setSystemPromptLoading(true);
    void load().catch((error) => {
      console.error("Failed to load system prompt:", error);
    }).finally(() => {
      if (systemPromptLoadIdRef.current === loadId) {
        setSystemPromptLoading(false);
      }
    });
  }, [activeTopPanel, systemPromptLoading, toggleTopPanel]);

  const openSessionStatsPanel = useCallback(() => {
    if (isMobile) setSidebarOpen(false);
    setMobileToolbarMoreOpen(false);
    setActiveTopPanel("session");
  }, [isMobile]);

  const handleSidebarToggle = useCallback(() => {
    if (isMobile) {
      setActiveTopPanel(null);
      setMobileToolbarMoreOpen(false);
    }
    setSidebarOpen((open) => !open);
  }, [isMobile]);

  const handleSidebarShow = useCallback(() => {
    if (isMobile) {
      setActiveTopPanel(null);
      setMobileToolbarMoreOpen(false);
    }
    setSidebarOpen(true);
  }, [isMobile]);

  const handleMobileToolbarMoreToggle = useCallback(() => {
    setSidebarOpen(false);
    setActiveTopPanel(null);
    setMobileToolbarMoreOpen((open) => !open);
  }, []);

  const handleRightPanelToggle = useCallback(() => {
    if (isMobile) {
      setSidebarOpen(false);
      setActiveTopPanel(null);
      setMobileToolbarMoreOpen(false);
    }
    setRightPanelOpen((open) => !open);
  }, [isMobile]);

  useEffect(() => {
    if (!mobileToolbarMoreOpen) return;

    const handlePointerDown = (event: PointerEvent) => {
      const toolbar = mobileToolbarRef.current;
      if (toolbar && event.composedPath().includes(toolbar)) return;
      setMobileToolbarMoreOpen(false);
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      setMobileToolbarMoreOpen(false);
    };

    document.addEventListener("pointerdown", handlePointerDown, true);
    document.addEventListener("keydown", handleKeyDown, true);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown, true);
      document.removeEventListener("keydown", handleKeyDown, true);
    };
  }, [mobileToolbarMoreOpen]);

  useEffect(() => {
    setMobileToolbarMoreOpen(false);
  }, [isMobile, selectedSession?.id, newSessionDraftId]);

  useEffect(() => {
    if (!activeTopPanel || !topBarRef.current) return;
    const update = () => {
      const topBarRect = topBarRef.current!.getBoundingClientRect();
      setTopPanelPos({ top: topBarRect.bottom, left: topBarRect.left, width: topBarRect.width });
    };
    update();
    const ro = new ResizeObserver(update);
    ro.observe(topBarRef.current);
    return () => ro.disconnect();
  }, [activeTopPanel]);

  // Right panel — file tabs only
  const [fileTabs, setFileTabs] = useState<Tab[]>([]);
  const [activeFileTabId, setActiveFileTabId] = useState<string | null>(null);

  const handleFileViewerStateChange = useCallback((
    tabId: string,
    viewerRevision: number,
    viewerState: FileViewerState,
  ) => {
    setFileTabs((prev) => saveFileViewerState(prev, tabId, viewerRevision, viewerState));
  }, []);

  const handleFileTabDirtyChange = useCallback((tabId: string, dirty: boolean) => {
    setFileTabs((prev) => saveFileDirtyState(prev, tabId, dirty));
  }, []);

  const canRenamePath = useCallback((filePath: string) => (
    !fileTabs.some((tab) => (
      tab.kind !== "console"
      && tab.dirty
      && (tab.filePath === filePath || tab.filePath.startsWith(`${filePath}/`))
    ))
  ), [fileTabs]);

  const handleFileRenamed = useCallback((oldPath: string, newPath: string) => {
    const oldPrefix = `${oldPath}/`;
    const newPrefix = `${newPath}/`;
    setFileTabs((prev) => renameFileTab(prev, oldPath, newPath));
    setActiveFileTabId((current) => {
      if (!current) return current;
      if (current === `file:${oldPath}`) return `file:${newPath}`;
      if (current.startsWith(`file:${oldPrefix}`)) {
        return `file:${newPrefix}${current.slice(`file:${oldPrefix}`.length)}`;
      }
      return current;
    });
  }, []);

  useEffect(() => {
    setFileTabs((prev) => prev.map((tab) => (
      tab.kind === "console" ? { ...tab, label: translate("console.title") } : tab
    )));
  }, [translate]);

  // Same @mention format as the chat input's @ autocomplete, so the agent's
  // read tool resolves it the same way (it strips the @ prefix).
  const handleAtMention = useCallback((relativePath: string, isDir: boolean) => {
    chatInputRef.current?.insertText(buildAtMentionText(relativePath, isDir));
    if (isMobile) { setRightPanelOpen(false); setSidebarOpen(false); }
  }, [isMobile]);

  const handleAtMentions = useCallback((relativePaths: string[]) => {
    const mentions = buildFileAtMentionsText(relativePaths);
    if (mentions) chatInputRef.current?.insertText(mentions);
    if (isMobile) { setRightPanelOpen(false); setSidebarOpen(false); }
  }, [isMobile]);

  const handleFileLineMention = useCallback((relativePath: string, startLine: number, endLine: number) => {
    chatInputRef.current?.insertText(buildFileLineMentionText(relativePath, startLine, endLine));
    if (isMobile) { setRightPanelOpen(false); setSidebarOpen(false); }
  }, [isMobile]);

  const initialSessionId = initialNavigation.sessionId;
  const [activeCwd, setActiveCwd] = useState<string | null>(null);
  const activeProjectKeyRef = useRef<string | null>(null);
  // True once the initial ?session= URL param has been resolved (or confirmed absent)
  const [initialSessionRestored, setInitialSessionRestored] = useState<boolean>(() => !initialSessionId);
  const [sidebarInitialLoadDone, setSidebarInitialLoadDone] = useState(false);
  const [sidebarHasInitialProject, setSidebarHasInitialProject] = useState(false);
  const [initialWorkspaceRestoreDone, setInitialWorkspaceRestoreDone] = useState(false);
  // Suppresses sessionKey bump in handleCwdChange during the initial URL restore
  const suppressCwdBumpRef = useRef(false);
  // Guards the async workspace restore so a slow response from an earlier
  // switch cannot resurrect a session into a project the user already left.
  const workspaceRestoreTokenRef = useRef(0);

  const invalidateWorkspaceRestore = useCallback(() => {
    workspaceRestoreTokenRef.current += 1;
  }, []);

  // Persist every active-session transition, including new and forked sessions
  // that bypass the sidebar selection handler. Transient sessions do not yet
  // carry projectKey, so use the active project identity until hydration.
  useEffect(() => {
    if (!selectedSession) return;
    const projectKey = selectedSession.projectKey
      ?? activeProjectKeyRef.current
      ?? workspaceKeyOf(selectedSession);
    setLastOpenSession(projectKey, selectedSession.id);
  }, [selectedSession]);

  useEffect(() => {
    const requestedCwd = initialNavigation.requestedCwd;
    if (!requestedCwd) return;

    const controller = new AbortController();
    setInitialCwdStatus("validating");
    setInitialCwdError(null);

    void fetch("/api/cwd/validate", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ cwd: requestedCwd }),
      signal: controller.signal,
    })
      .then(async (response) => {
        const data = await response.json().catch(() => ({})) as { cwd?: string; error?: string };
        if (!response.ok || !data.cwd) {
          throw new Error(data.error ?? `HTTP ${response.status}`);
        }

        // The sidebar will notify us when it adopts this cwd. Avoid remounting
        // the just-created empty chat during that initial synchronization.
        suppressCwdBumpRef.current = true;
        const draftId = `initial:${requestedCwd}`;
        setNewSessionDraftId(draftId);
        activeNewSessionDraftKeyRef.current = `new:${draftId}:${data.cwd}`;
        setNewSessionCwd(data.cwd);
        setInitialCwdStatus("ready");
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) return;
        setInitialCwdError(error instanceof Error ? error.message : String(error));
        setInitialCwdStatus("error");
      });

    return () => controller.abort();
  }, [initialNavigation]);

  // Restore the workspace's last open session after switching to it. Called
  // from handleCwdChange once the outgoing context has been reset. The session
  // is looked up against the live list so a deleted or drifted session falls
  // back to the default welcome page instead of erroring.
  const restoreWorkspaceContext = useCallback((projectKey: string, onSettled?: () => void) => {
    const token = ++workspaceRestoreTokenRef.current;
    const lastOpenSessionId = getLastOpenSession(projectKey);
    if (!lastOpenSessionId) {
      onSettled?.();
      return;
    }
    void fetch("/api/sessions")
      .then((r) => (r.ok ? (r.json() as Promise<{ sessions: SessionInfo[] }>) : null))
      .then((d) => {
        if (token !== workspaceRestoreTokenRef.current) return; // stale switch
        const s = d?.sessions.find((x) => x.id === lastOpenSessionId);
        if (!s) {
          // The list loaded but the remembered session is gone — forget it.
          // When the list itself failed (d === null) keep the memory so a
          // later switch retries the restore.
          if (d) clearLastOpen(projectKey);
          return;
        }
        if (workspaceKeyOf(s) !== projectKey) {
          // Defensive: the remembered session drifted out of this workspace.
          clearLastOpen(projectKey);
          return;
        }
        // Selecting the session must remount the chat with the session
        // present: useAgentSession loads content in a mount-only effect, so
        // the null-session welcome mount from the switch would never load
        // the restored session's messages.
        setSelectedSession(s);
        setSessionKey((k) => k + 1);
        if (new URLSearchParams(window.location.search).get("session") !== s.id) {
          router.replace(`?session=${encodeURIComponent(s.id)}`, { scroll: false });
        }
      })
      .catch(() => {
        // Network hiccup: keep the remembered session for a later retry.
      })
      .finally(() => {
        if (token === workspaceRestoreTokenRef.current) onSettled?.();
      });
  }, [router]);

  const handleCwdChange = useCallback((
    cwd: string | null,
    projectRoot?: string | null,
    projectKey?: string | null,
  ) => {
    invalidateWorkspaceRestore();
    const isInitialProjectAdoption = activeProjectKeyRef.current === null;
    const currentFreshCwd = newSessionCwd ?? activeCwd;
    setActiveCwd(cwd);
    // Skip if cwd is null (initial mount).
    if (!cwd) return;
    const newProject = projectKey ?? projectRoot ?? cwd;
    const currentProject = activeProjectKeyRef.current
      ?? (selectedSession ? workspaceKeyOf(selectedSession) : null);
    activeProjectKeyRef.current = newProject;

    // Keep the project identity in sync during the initial URL restore without
    // remounting the just-created or restored chat.
    if (suppressCwdBumpRef.current) {
      suppressCwdBumpRef.current = false;
      if (isInitialProjectAdoption) setInitialWorkspaceRestoreDone(true);
      return;
    }
    // The server may hydrate a normalized key after a custom cwd is already
    // active. Updating identity for the exact same cwd is not a user switch.
    if (currentFreshCwd === cwd && currentProject !== newProject) {
      if (isInitialProjectAdoption) setInitialWorkspaceRestoreDone(true);
      return;
    }
    // Existing sessions stay open when the worktree selector moves within the
    // same project. A fresh composer must remount when its effective cwd moves,
    // otherwise its already-created runtime would keep sending to the old cwd.
    if (
      currentProject === newProject
      && (selectedSession !== null || currentFreshCwd === cwd)
    ) {
      if (isInitialProjectAdoption) setInitialWorkspaceRestoreDone(true);
      return;
    }
    // Close any session that belongs to a different project — it no longer
    // matches the selected project directory.
    const draftId = typeof crypto.randomUUID === "function"
      ? crypto.randomUUID()
      : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
    setNewSessionDraftId(draftId);
    activeNewSessionDraftKeyRef.current = `new:${draftId}:${cwd}`;
    setSelectedSession(null);
    setNewSessionCwd((prev) => {
      if (prev && prev !== cwd) return null;
      return prev;
    });
    setSessionKey((k) => k + 1);
    setBranchTree([]);
    setBranchActiveLeafId(null);
    setSystemPrompt(null);
    setSystemPromptLoading(false);
    setActiveTopPanel(null);
    if (currentProject !== newProject) {
      // File tabs are keyed by absolute path, so tabs opened in the previous
      // project must not linger. Same-project worktree switches keep them.
      setFileTabs([]);
      setActiveFileTabId(null);
      setRightPanelOpen(false);
      // Restore the workspace we switched to: its last open session, or keep
      // the default welcome page when none is remembered.
      restoreWorkspaceContext(
        newProject,
        isInitialProjectAdoption ? () => setInitialWorkspaceRestoreDone(true) : undefined,
      );
    }
    router.replace("/", { scroll: false });
  }, [activeCwd, invalidateWorkspaceRestore, newSessionCwd, router, selectedSession, restoreWorkspaceContext]);

  const handleSelectSession = useCallback((session: SessionInfo, isRestore = false) => {
    invalidateWorkspaceRestore();
    activeNewSessionDraftKeyRef.current = null;
    // Re-clicking the already-open session must not remount the chat and
    // re-run the full load/positioning cycle. Only skip when the effective
    // cwd context already matches — otherwise a pending cwd move still needs
    // the full re-select flow.
    if (!isRestore && selectedSession) {
      const sameProject =
        workspaceKeyOf(selectedSession) === workspaceKeyOf(session);
      if (selectedSession.id === session.id && sameProject) {
        if (isMobile) setSidebarOpen(false);
        return;
      }
    }
    setNewSessionCwd(null);
    setSelectedSession(session);
    setSessionKey((k) => k + 1);
    setSystemPrompt(null);
    setSystemPromptLoading(false);
    setInitialSessionRestored(true);
    // On mobile, collapse the overlay drawer so the chat is revealed after pick.
    if (isMobile && !isRestore) setSidebarOpen(false);
    if (isRestore) {
      // Suppress the redundant sessionKey bump that would come from the
      // onCwdChange effect firing after setSelectedCwd in the sidebar
      suppressCwdBumpRef.current = true;
    }
    // Skip router.replace when restoring from URL — the param is already correct
    // and calling replace in production Next.js triggers a Suspense remount loop
    if (!isRestore) {
      router.replace(`?session=${encodeURIComponent(session.id)}`, { scroll: false });
    }
  }, [invalidateWorkspaceRestore, router, isMobile, selectedSession]);

  const handleNewSession = useCallback((sessionId: string, cwd: string) => {
    invalidateWorkspaceRestore();
    const draftKey = `new:${sessionId}:${cwd}`;
    activeNewSessionDraftKeyRef.current = draftKey;
    setNewSessionDraftId(sessionId);
    setSelectedSession(null);
    setNewSessionCwd(cwd);
    setSessionKey((k) => k + 1);
    setBranchTree([]);
    setBranchActiveLeafId(null);
    setSystemPrompt(null);
    setSystemPromptLoading(false);
    setActiveTopPanel(null);
    if (isMobile) setSidebarOpen(false);
    router.replace("/", { scroll: false });
  }, [invalidateWorkspaceRestore, router, isMobile]);

  // Client-built transient SessionInfo (new session / fork) lacks the
  // server-computed projectKey, which the same-project check in
  // handleCwdChange relies on. Hydrate it from the session list so switching
  // worktrees right after creating a session doesn't close the chat.
  const hydrateSelectedSession = useCallback((sessionId: string) => {
    void fetch("/api/sessions", { cache: "no-store" })
      .then((r) => (r.ok ? (r.json() as Promise<{ sessions: SessionInfo[] }>) : null))
      .then((d) => {
        const full = d?.sessions.find((s) => s.id === sessionId);
        if (!full) return;
        setSelectedSession((prev) => {
          if (prev?.id !== sessionId) return prev;
          const merged = { ...prev, ...full, transient: full.transient ?? false };
          // `/api/sessions` 有 30 秒列表缓存，而且新会话的首条 user 消息要等
          // `prompt` 落盘后才出现在文件里：这段时间服务端只能给出占位的
          // `(no messages)`。发出消息的瞬间 `promoteNewSession` 已经把原文放进
          // 本地条目，别让占位文案盖掉它——否则顶栏标题会一直显示
          // `(no messages)`，直到某次缓存过期才恢复。
          if (
            isPlaceholderFirstMessage(merged.firstMessage)
            && !isPlaceholderFirstMessage(prev.firstMessage)
          ) {
            merged.firstMessage = prev.firstMessage;
            merged.messageCount = Math.max(merged.messageCount, prev.messageCount);
          }
          return merged;
        });
      })
      .catch(() => {});
  }, []);

  // Called by ChatWindow when a new session gets its real id from pi
  const handleSessionCreated = useCallback((session: SessionInfo, sourceDraftKey: string) => {
    setRefreshKey((k) => k + 1);
    if (activeNewSessionDraftKeyRef.current !== sourceDraftKey) return;
    invalidateWorkspaceRestore();
    activeNewSessionDraftKeyRef.current = null;
    setNewSessionCwd(null);
    setSelectedSession(session);
    hydrateSelectedSession(session.id);
    router.replace(`?session=${encodeURIComponent(session.id)}`, { scroll: false });
  }, [invalidateWorkspaceRestore, router, hydrateSelectedSession]);

  const deliverSessionNotification = useCallback(({
    targetSession,
    title,
    body,
    tag,
    icon = NOTIFICATION_ICON,
  }: {
    targetSession: SessionInfo | null;
    title: string;
    body: string;
    tag?: string;
    icon?: string;
  }) => {
    if (isDesktopShell()) {
      showDesktopNotification({ title, body });
      return;
    }
    if (!("Notification" in window)) return;

    const fire = () => {
      const sessionUrl = targetSession ? `/?session=${encodeURIComponent(targetSession.id)}` : "/";
      void showBrowserNotification({
        title,
        body,
        sessionUrl,
        tag,
        icon,
        onClick: () => {
          window.focus();
          if (targetSession) handleSelectSession(targetSession);
        },
      });
    };

    if (Notification.permission === "granted") {
      fire();
    } else if (Notification.permission === "default") {
      void Notification.requestPermission().then((p) => { if (p === "granted") fire(); });
    }
  }, [handleSelectSession]);

  const handleBackgroundTaskDone = useCallback((sessions: SessionInfo[]) => {
    if (soundEnabledRef.current) playDoneSound();
    if (!shouldShowBrowserNotification()) return;
    for (const session of sessions) {
      deliverSessionNotification({
        targetSession: session,
        title: session.name ?? translate("i18n.sessionComplete"),
        body: translate("i18n.taskFinished"),
        tag: `pi-session-complete:${session.id}`,
      });
    }
  }, [deliverSessionNotification, playDoneSound, soundEnabledRef, translate]);

  const handleAgentEnd = useCallback((recentPrompt: string | null) => {
    setRefreshKey((k) => k + 1);
    setExplorerRefreshKey((k) => k + 1);
    if (selectedSession) hydrateSelectedSession(selectedSession.id);

    if (!shouldShowBrowserNotification()) return;
    const targetSession = selectedSession;
    const promptSummary = summarizeNotificationPrompt(recentPrompt);
    deliverSessionNotification({
      targetSession,
      title: targetSession?.name ?? translate("i18n.sessionComplete"),
      body: promptSummary
        ? `${translate("i18n.taskFinished")} ${promptSummary}`
        : translate("i18n.taskFinished"),
      tag: targetSession ? `pi-session-complete:${targetSession.id}` : undefined,
    });
  }, [deliverSessionNotification, hydrateSelectedSession, selectedSession, translate]);

  const handleAttentionNeeded = useCallback((request: BlockingExtensionUiRequest) => {
    if (!shouldShowBrowserNotification()) return;
    if (!claimExtensionAttentionNotification(request, notifiedAttentionRequestIdsRef.current)) return;

    deliverSessionNotification({
      targetSession: selectedSession,
      title: translate("i18n.attentionNeeded"),
      body: request.method === "custom"
        ? translate("i18n.extensionInputNeeded")
        : request.title,
      tag: `pi-extension-ui:${request.id}`,
    });
  }, [deliverSessionNotification, selectedSession, translate]);

  const handleAutoName = useCallback(async (targetSessionId?: string) => {
    const sessionId = targetSessionId ?? selectedSession?.id;
    if (!sessionId || autoNameStatus.kind === "naming") return;
    if (autoNameTimerRef.current) clearTimeout(autoNameTimerRef.current);
    setActiveTopPanel(null);
    setAutoNameStatus({ kind: "naming", sessionId });

    try {
      const response = await fetch(`/api/sessions/${encodeURIComponent(sessionId)}/auto-name`, {
        method: "POST",
      });
      const body = (await response.json().catch(() => ({}))) as { title?: string; error?: string };
      if (!response.ok || !body.title) {
        throw new Error(body.error || `HTTP ${response.status}`);
      }

      const title = body.title.trim();
      setRefreshKey((key) => key + 1);
      if (activeSessionIdRef.current === sessionId) {
        setSelectedSession((current) => current?.id === sessionId ? { ...current, name: title } : current);
        setSessionStats((current) => current?.sessionId === sessionId ? { ...current, sessionName: title } : current);
      }
      setAutoNameStatus({ kind: "success", sessionId });
      autoNameTimerRef.current = setTimeout(() => setAutoNameStatus({ kind: "idle" }), 1800);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setAutoNameStatus({ kind: "error", sessionId, message });
      autoNameTimerRef.current = setTimeout(() => setAutoNameStatus({ kind: "idle" }), 5000);
    }
  }, [autoNameStatus.kind, selectedSession?.id]);

  const handleExplorerRefresh = useCallback(() => {
    setExplorerRefreshKey((k) => k + 1);
  }, []);

  const handleSessionForked = useCallback((newSessionId: string) => {
    invalidateWorkspaceRestore();
    activeNewSessionDraftKeyRef.current = null;
    setRefreshKey((k) => k + 1);
    setSessionKey((k) => k + 1);
    setNewSessionCwd(null);
    setSelectedSession((prev) => ({
      ...(prev ?? { path: "", cwd: "", created: "", modified: "", messageCount: 0, firstMessage: "" }),
      id: newSessionId,
      transient: false,
    }));
    hydrateSelectedSession(newSessionId);
    router.replace(`?session=${encodeURIComponent(newSessionId)}`, { scroll: false });
  }, [invalidateWorkspaceRestore, router, hydrateSelectedSession]);

  const handleInitialRestoreDone = useCallback((hasInitialProject: boolean) => {
    setInitialSessionRestored(true);
    setSidebarInitialLoadDone(true);
    setSidebarHasInitialProject(hasInitialProject);
  }, []);

  useEffect(() => {
    const cwdRestoreDone = initialNavigation.requestedCwd === null
      || initialCwdStatus === "ready"
      || initialCwdStatus === "error";
    const workspaceRestoreDone = !sidebarHasInitialProject
      || initialCwdStatus === "error"
      || initialWorkspaceRestoreDone;
    if (!sidebarInitialLoadDone || !initialSessionRestored || !cwdRestoreDone || !workspaceRestoreDone) return;

    let secondFrame = 0;
    const firstFrame = requestAnimationFrame(() => {
      secondFrame = requestAnimationFrame(() => {
        document.documentElement.dataset.piAppReady = "true";
        window.dispatchEvent(new Event("pi-app-ready"));
      });
    });
    return () => {
      cancelAnimationFrame(firstFrame);
      cancelAnimationFrame(secondFrame);
    };
  }, [
    initialCwdStatus,
    initialNavigation.requestedCwd,
    initialSessionRestored,
    initialWorkspaceRestoreDone,
    sidebarHasInitialProject,
    sidebarInitialLoadDone,
  ]);

  // 只要刷新会话列表，不动当前打开的会话（新会话发出第一条消息时用）。
  const handleSessionListRefresh = useCallback(() => {
    setRefreshKey((k) => k + 1);
  }, []);

  const handleSessionDeleted = useCallback((sessionId: string) => {
    invalidateWorkspaceRestore();
    setRefreshKey((k) => k + 1);
    if (selectedSession?.id === sessionId) {
      const cwd = selectedSession.cwd;
      const draftId = typeof crypto.randomUUID === "function"
        ? crypto.randomUUID()
        : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
      setNewSessionDraftId(draftId);
      activeNewSessionDraftKeyRef.current = cwd ? `new:${draftId}:${cwd}` : null;
      setSelectedSession(null);
      setNewSessionCwd(cwd ?? null);
      setSessionKey((k) => k + 1);
      setBranchTree([]);
      setBranchActiveLeafId(null);
      setSystemPrompt(null);
      setSystemPromptLoading(false);
      setActiveTopPanel(null);
      router.replace("/", { scroll: false });
    }
  }, [invalidateWorkspaceRestore, selectedSession, router]);

  const handleOpenFile = useCallback((
    filePath: string,
    fileName: string,
    options?: { sourceSessionId?: string | null; modeHint?: "diff"; line?: number },
  ) => {
    if (!rightPanelOpen && !isMobile && window.matchMedia("(min-width: 960px)").matches) {
      cancelChatScrollPreservationRef.current?.();
      const container = chatScrollContainerRef.current;
      cancelChatScrollPreservationRef.current = container
        ? preserveChatScrollDuringPanelTransition(container)
        : null;
    }
    const sourceSessionId = options?.sourceSessionId;
    const modeHint = options?.modeHint;
    const line = options?.line;
    const tabId = `file:${filePath}`;
    setFileTabs((prev) => openFileTab(prev, {
      fileName,
      filePath,
      modeHint,
      line,
      sourceSessionId,
      tabId,
    }));
    setActiveFileTabId(tabId);
    setRightPanelOpen(true);
    // On mobile the file panel is full-screen; close the drawer so it shows.
    if (isMobile) setSidebarOpen(false);
  }, [isMobile, rightPanelOpen]);

  const handleOpenLinkedFile = useCallback((filePath: string, location?: FileOpenLocation) => {
    handleOpenFile(filePath, getFileName(filePath), {
      sourceSessionId: selectedSession?.id ?? null,
      line: location?.line,
      modeHint: chatLinkModeHint({ hasLine: location?.line !== undefined }),
    });
  }, [handleOpenFile, selectedSession?.id]);

  const handleOpenConsole = useCallback(() => {
    const cwd = selectedSession?.cwd ?? newSessionCwd ?? activeCwd;
    if (!cwd) return;
    const hasTerminal = terminalCwds.includes(cwd);
    if (!hasTerminal) {
      setTerminalCwds((current) => current.includes(cwd) ? current : [...current, cwd]);
      setConsoleOpen(true);
      return;
    }
    setConsoleOpen((current) => !current);
  }, [activeCwd, newSessionCwd, selectedSession?.cwd, terminalCwds]);

  // Global keyboard shortcuts (handles Esc, Ctrl/Cmd+N, Ctrl/Cmd+B, Ctrl/Cmd+` etc.)
  useGlobalKeyboardShortcuts({
    onNewSession: (cwd: string) => handleNewSession(`kb-${Date.now()}`, cwd),
    onShowSidebar: handleSidebarShow,
    onToggleConsole: handleOpenConsole,
    onOpenDirectory: () => setOpenDirectoryRequest((value) => value + 1),
    activeCwd,
  });

  // `/new` 行走与 Ctrl/Cmd+N 完全相同的路径：纯前端 draft，不先行创建后端
  // 会话，也不去碰任何正在运行的会话（它们在 rpc-manager 里继续跑）。
  const handleNewSessionRequest = useCallback((): boolean => {
    if (!activeCwd) return false;
    handleNewSession(`kb-${Date.now()}`, activeCwd);
    return true;
  }, [activeCwd, handleNewSession]);

  const handleCloseFileTab = useCallback((tabId: string) => {
    setFileTabs((prev) => {
      const next = prev.filter((t) => t.id !== tabId);
      if (next.length === 0) setRightPanelOpen(false);
      return next;
    });
    setActiveFileTabId((cur) => {
      if (cur !== tabId) return cur;
      const remaining = fileTabs.filter((t) => t.id !== tabId);
      return remaining.length > 0 ? remaining[remaining.length - 1].id : null;
    });
  }, [fileTabs]);

  const handleViewFullHistory = useCallback(() => {
    if (!selectedSession) return;
    const exportPath = `/api/sessions/${encodeURIComponent(selectedSession.id)}/export?inline=1`;
    // 导出页会注入一个「返回对话」按钮，文案跟着界面语言走；`returnTo` 让按钮
    // 直接回跳当前地址（白名单见服务端 `allowed_return_to`）。
    const langQuery = `&lang=${encodeURIComponent(locale)}`;
    // 导出页是另一份 HTML，拿不到这里的 React 状态；只靠 `prefers-color-scheme`
    // 的话「系统深色 + 应用里选浅色」会让导出页永远是黑的。所以把**已解析**
    // 的主题（`light`/`dark`）明确传过去。
    const themeQuery = `&theme=${theme}`;
    // 覆盖式标题栏（macOS）下红绿灯浮在网页内容上，导出页的左侧大纲得往下让位，
    // 否则第一项正好压在三个按钮底下。这个标志只在桌面壳里为 true。
    const overlayQuery = window.__TOILER_OVERLAY_TITLEBAR__ ? "&overlayChrome=1" : "";
    // Tauri's macOS WebView does not create a child window for window.open().
    // The desktop shell injects its loopback API origin, so navigate this
    // WebView directly; the button (and Escape) on the export page returns to
    // the selected conversation.
    if (window.__PI_WEB_API_ORIGIN__) {
      // eslint-disable-next-line @next/next/no-location-assign-relative-destination -- loopback export page outside the Tauri app origin
      window.location.href = `${window.__PI_WEB_API_ORIGIN__.replace(/\/$/, "")}${exportPath}&desktop=1${langQuery}${themeQuery}${overlayQuery}&returnTo=${encodeURIComponent(window.location.href)}`;
      return;
    }
    window.open(`${exportPath}${langQuery}${themeQuery}`, "_blank", "noopener,noreferrer");
  }, [locale, selectedSession, theme]);

  // Show chat area if a session is selected, or if we have a cwd to start a new session in
  const effectiveNewSessionCwd = newSessionCwd ?? (selectedSession === null && activeCwd ? activeCwd : null);
  const newSessionDraftKey = selectedSession === null && effectiveNewSessionCwd
    ? `new:${newSessionDraftId}:${effectiveNewSessionCwd}`
    : null;
  useLayoutEffect(() => {
    activeNewSessionDraftKeyRef.current = newSessionDraftKey;
  }, [newSessionDraftKey]);
  const showChat = selectedSession !== null || effectiveNewSessionCwd !== null;

  /** 顶栏左侧的会话标题：手改过就用名字，否则用首条消息（与侧栏同一套回退顺序），
      再否则用 id 前 12 位。未保存的新会话没有标题，用「新会话」。 */
  const topBarTitle = useMemo(() => {
    if (!selectedSession) return translate("i18n.newSession");
    if (selectedSession.name) return selectedSession.name;
    const first = skillExpansionToCommand(selectedSession.firstMessage) ?? selectedSession.firstMessage;
    return (first || "").slice(0, 80) || selectedSession.id.slice(0, 12);
  }, [selectedSession, translate]);

  const projectTrustCwd = selectedSession?.cwd ?? effectiveNewSessionCwd;
  // While restoring initial session from URL, don't show the placeholder
  const showPlaceholder = initialSessionRestored && !showChat;

  useEffect(() => {
    setProjectTrust(null);
    setProjectTrustDialogOpen(false);
    setProjectTrustError(null);
    if (!projectTrustCwd) return;

    const controller = new AbortController();
    fetch(`/api/project-trust?cwd=${encodeURIComponent(projectTrustCwd)}`, {
      signal: controller.signal,
    })
      .then(async (response) => {
        const data = await response.json() as ProjectTrustStatus & { error?: string };
        if (!response.ok || data.error) throw new Error(data.error ?? `HTTP ${response.status}`);
        setProjectTrust(data);
      })
      .catch((error) => {
        if (error instanceof DOMException && error.name === "AbortError") return;
        console.error("Failed to load project trust:", error);
      });
    return () => controller.abort();
  }, [projectTrustCwd]);

  const handleTrustProject = useCallback(async () => {
    if (!projectTrustCwd || projectTrustBusy) return;
    setProjectTrustBusy(true);
    setProjectTrustError(null);
    try {
      const response = await fetch("/api/project-trust", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ cwd: projectTrustCwd }),
      });
      const data = await response.json() as ProjectTrustStatus & { error?: string };
      if (!response.ok || data.error) throw new Error(data.error ?? `HTTP ${response.status}`);
      setProjectTrust(data);
      setProjectTrustDialogOpen(false);
      setModelsRefreshKey((key) => key + 1);
      setSessionKey((key) => key + 1);
    } catch (error) {
      setProjectTrustError(error instanceof Error ? error.message : String(error));
    } finally {
      setProjectTrustBusy(false);
    }
  }, [projectTrustBusy, projectTrustCwd]);

  const activeFileTab = fileTabs.find((tab) => tab.id === activeFileTabId) ?? null;
  const activeCwdName = activeCwd ? getFileName(activeCwd) || activeCwd : null;
  const windowTitle = activeCwdName ? `${activeCwdName} - Toiler` : "Toiler";

  // Git 面板的目标仓库：默认跟随当前项目；从文件浏览器的子项目行打开时指向该子仓库。
  const gitPanelTargetCwd = gitPanelCwd ?? (activeProjectIsGit === true ? activeCwd : null);

  const closeGitPanel = useCallback(() => {
    setGitPanelOpen(false);
    setGitPanelCwd(null);
  }, []);

  // 侧边栏分支行 / 底部工具条的 Git 按钮：总是切回当前项目自己的仓库。
  const toggleGitPanel = useCallback(() => {
    setGitPanelCwd(null);
    setGitPanelOpen((open) => !open);
  }, []);

  const openRepositoryGitPanel = useCallback((repositoryPath: string) => {
    setGitPanelCwd(repositoryPath);
    setGitPanelOpen(true);
  }, []);

  useEffect(() => {
    // `null` means the sidebar is still resolving the current project. Keep
    // an already-open panel mounted until Git detection resolves explicitly
    // to a non-repository result — unless the panel is looking at a sub-project
    // repository, which is a repository regardless of the enclosing project.
    if (activeProjectIsGit === false && gitPanelCwd === null) setGitPanelOpen(false);
  }, [activeProjectIsGit, gitPanelCwd]);

  // 换项目后不再沿用上一个项目里的子仓库，避免面板指向已不可见/已删除的目录。
  useEffect(() => {
    setGitPanelCwd(null);
  }, [activeCwd]);

  useEffect(() => {
    const syncWindowTitle = () => {
      if (document.title !== windowTitle) document.title = windowTitle;
    };

    syncWindowTitle();
    const observer = new MutationObserver(syncWindowTitle);
    observer.observe(document.head, { childList: true, subtree: true, characterData: true });
    return () => observer.disconnect();
  }, [windowTitle]);

  const sidebarContent = (
    <>
      <SessionSidebar
        selectedSessionId={selectedSession?.id ?? null}
        onSelectSession={handleSelectSession}
        onNewSession={handleNewSession}
        initialSessionId={initialSessionId}
        skipInitialProjectSelection={initialNavigation.requestedCwd !== null}
        onInitialRestoreDone={handleInitialRestoreDone}
        refreshKey={refreshKey}
        onSessionDeleted={handleSessionDeleted}
        selectedCwd={selectedSession?.cwd ?? newSessionCwd ?? null}
        onCwdChange={handleCwdChange}
        onOpenFile={handleOpenFile}
        onFileRenamed={handleFileRenamed}
        canRenamePath={canRenamePath}
        explorerRefreshKey={explorerRefreshKey}
        onExplorerRefresh={handleExplorerRefresh}
        onAtMention={handleAtMention}
        onAtMentions={handleAtMentions}
        onBackgroundTaskDone={handleBackgroundTaskDone}
        onRunningSessionIdsChange={handleRunningSessionIdsChange}
        onProjectGitStateChange={setActiveProjectIsGit}
        onGenerateTitle={(sessionId) => void handleAutoName(sessionId)}
        titleGenerationStatus={autoNameStatus.kind === "idle" ? null : autoNameStatus}
        appUpdate={appUpdate}
        onAppUpdateClick={openAppUpdate}
        onAboutClick={() => setAboutOpen(true)}
        onHideSidebar={handleSidebarToggle}
        openDirectoryRequest={openDirectoryRequest}
        onOpenGitPanel={toggleGitPanel}
        onOpenRepositoryGitPanel={openRepositoryGitPanel}
        gitPanelOpen={gitPanelOpen}
      />
      <div style={{ padding: "8px", flexShrink: 0, display: "flex", justifyContent: "space-between", gap: 4 }}>
        {([
          {
            id: "settings",
            label: translate("common.settings"),
            onClick: () => setSettingsOpen(true),
            disabled: false,
            icon: (
                <svg viewBox="0 0 1024 1024" version="1.1" xmlns="http://www.w3.org/2000/svg" p-id="9191" width="14" height="14">
                  <path
                      d="M537.6 919.893333h-52.48c-24.32 0-45.226667-17.493333-49.066667-41.813333l-12.8-76.8a296.96 296.96 0 0 1-53.76-22.186667l-64 44.373334c-20.053333 14.08-46.933333 11.52-64-5.546667l-37.12-37.12c-17.493333-17.066667-19.626667-44.373333-5.12-64.426667l44.8-62.72c-9.386667-17.066667-16.64-35.413333-22.613333-54.186666l-76.373333-13.653334c-23.466667-4.266667-40.96-24.746667-40.96-49.066666v-52.48c0-24.32 17.493333-45.226667 41.813333-49.066667l75.946667-12.8c5.546667-18.773333 13.226667-36.693333 22.613333-54.186667L200.106667 304.64c-14.08-20.053333-11.52-46.933333 5.546666-64l37.12-36.693333c17.066667-17.493333 44.373333-19.626667 64.426667-5.12l63.146667 45.226666c17.066667-8.96 34.986667-16.64 53.76-22.186666l12.373333-76.373334c3.84-24.32 24.746667-41.813333 49.493333-41.813333h52.053334c24.32 0 45.226667 17.493333 49.066666 41.386667l13.653334 77.226666c18.346667 5.546667 36.693333 13.226667 53.76 22.186667l63.146666-45.653333c20.053333-14.506667 47.36-11.946667 64.426667 5.12l37.12 36.693333c17.066667 17.066667 19.626667 44.373333 5.546667 64l-45.226667 64.853333c8.96 17.066667 16.213333 34.986667 22.186667 53.333334l77.226666 12.8c24.32 3.84 41.813333 24.746667 41.813334 49.493333v52.053333c0 24.32-17.493333 45.226667-41.386667 49.066667l-77.653333 13.653333c-5.546667 18.346667-12.8 36.266667-21.76 53.333334l45.653333 63.573333c14.506667 19.626667 12.373333 47.36-5.12 64.426667l-36.693333 37.12c-17.066667 17.066667-44.373333 19.626667-64 5.546666l-64.426667-45.226666c-17.066667 9.386667-34.986667 16.64-53.76 22.613333l-13.653333 77.226667c-4.266667 23.893333-25.173333 41.386667-49.066667 40.96z m-170.666667-197.546666l13.226667 8.106666c21.76 13.226667 45.226667 22.613333 69.973333 29.013334l15.36 3.84 17.92 107.093333s0.853333 1.706667 1.706667 1.706667h52.48s1.706667-0.853333 1.706667-1.706667l19.2-107.52 14.933333-3.84a247.893333 247.893333 0 0 0 69.546667-29.013333l13.226666-8.106667 89.6 63.146667h2.56l36.693334-36.693334s0.426667-1.706667 0-2.56l-64-88.746666 8.106666-13.653334c13.226667-21.333333 22.613333-44.8 29.013334-69.546666l3.84-14.933334 107.946666-18.773333s1.706667-0.853333 1.706667-1.706667V486.4s-0.853333-1.706667-1.706667-1.706667l-107.946666-17.493333-3.84-15.36a251.733333 251.733333 0 0 0-28.586667-69.12l-8.106667-13.226667 63.573334-90.026666v-2.56l-36.693334-36.693334s-1.706667-0.426667-2.56 0L657.066667 303.786667l-13.653334-8.106667a247.893333 247.893333 0 0 0-69.546666-29.013333l-14.933334-3.84-18.773333-107.52s-0.853333-1.706667-1.706667-1.706667H486.4s-1.706667 0.853333-1.706667 1.706667l-17.493333 107.093333-15.36 3.84c-24.746667 5.973333-48.213333 15.786667-69.546667 28.586667l-13.653333 8.106666-88.32-63.146666s-1.706667-0.426667-2.56 0l-37.12 37.12s-0.426667 1.706667 0 2.56l61.866667 89.173333-8.106667 13.226667c-13.226667 21.76-23.04 45.226667-29.44 69.973333l-3.84 15.36-106.24 17.92s-1.706667 0.853333-1.706667 1.706667v52.053333s0.853333 1.706667 1.706667 1.706667l106.666667 18.773333 3.413333 14.933333c5.973333 24.746667 15.786667 48.213333 29.013333 69.973334l8.106667 13.653333-63.146667 87.893333s-0.426667 1.706667 0 2.56l36.693334 37.12s1.706667 0.426667 2.56 0l89.173333-61.866666z m145.066667-58.453334c-84.053333 0-151.893333-68.266667-151.893333-151.893333s68.266667-151.893333 151.893333-151.893333 151.893333 68.266667 151.893333 151.893333c0 84.053333-67.84 151.893333-151.893333 151.893333z m0-256c-57.6 0-104.106667 46.506667-104.106667 104.106667 0 57.6 46.506667 104.106667 104.106667 104.106667 57.6 0 104.106667-46.506667 104.106667-104.106667 0-57.6-46.506667-104.106667-104.106667-104.106667z"
                      fill="#333F48" p-id="9192"/>
                </svg>
            ),
          },
          {
            id: "git",
            label: translate("common.git"),
            onClick: toggleGitPanel,
            disabled: !activeCwd && !selectedSession?.cwd && !newSessionCwd,
            icon: (
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
                     strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <circle cx="6" cy="6" r="3"/>
                  <circle cx="18" cy="18" r="3"/>
                  <path d="M6 9v6a3 3 0 0 0 3 3h6"/>
                  <path d="M18 15V9a3 3 0 0 0-3-3H9"/>
                </svg>
            ),
          },
        ] as {
          id: string;
          label: string;
          title?: string;
          onClick: () => void;
          disabled: boolean;
          icon: React.ReactNode;
          iconOnly?: boolean;
          color?: string;
        }[]).filter(({id}) => id !== "git" || activeProjectIsGit === true).map(({
                                                                                  id,
                                                                                  label,
                                                                                  title,
                                                                                  onClick,
                                                                                  disabled,
                                                                                  icon,
                                                                                  iconOnly,
                                                                                  color
                                                                                }) => (
            <button
                key={id}
                type="button"
                onClick={onClick}
                disabled={disabled}
                title={title ?? label}
                aria-label={title ?? label}
                aria-pressed={id === "git" ? gitPanelOpen : undefined}
                style={{
                  flex: iconOnly ? "0 0 32px" : "0 0 auto",
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  gap: 6,
                  height: 32,
                  padding: "0 10px",
                  background: id === "git" && gitPanelOpen ? "var(--bg-selected)" : "transparent",
                  border: "1px solid transparent",
                  borderRadius: 6,
                  color: id === "git" && gitPanelOpen ? "var(--text)" : color ?? "var(--text-muted)",
                  cursor: disabled ? "default" : "pointer",
                  fontSize: 12,
                  opacity: disabled ? 0.35 : 1,
                  position: id === "git" && gitPanelOpen ? "relative" : undefined,
                  zIndex: id === "git" && gitPanelOpen ? 1200 : undefined,
                  transition: "background 0.12s, color 0.12s, border-color 0.12s",
                }}
                onMouseEnter={(e) => {
                  if (!disabled) {
                    e.currentTarget.style.background = "var(--bg-hover)";
                    e.currentTarget.style.color = "var(--text)";
                  }
                }}
                onMouseLeave={(e) => {
                  e.currentTarget.style.background = id === "git" && gitPanelOpen ? "var(--bg-selected)" : "transparent";
                  e.currentTarget.style.color = id === "git" && gitPanelOpen ? "var(--text)" : color ?? "var(--text-muted)";
                }}
            >
              {icon}
              {!iconOnly && label}
            </button>
        ))}
      </div>
    </>
  );

  const renderProjectTrustWarning = (mobileBanner: boolean) => {
    if (!bannerEnabled || !showChat || !projectTrust?.requiresTrust || projectTrust.trusted) return null;
    return (
      <button
        type="button"
        onClick={() => {
          setProjectTrustError(null);
          setProjectTrustDialogOpen(true);
        }}
        title={translate("trust.resourcesNotLoaded")}
        aria-label={translate("trust.resourcesNotLoaded")}
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: mobileBanner ? "flex-start" : "center",
          gap: 6,
          width: mobileBanner ? "100%" : undefined,
          minHeight: mobileBanner ? 32 : undefined,
          height: mobileBanner ? undefined : "100%",
          padding: mobileBanner ? "6px 12px" : "0 12px",
          background: mobileBanner ? "color-mix(in srgb, #d97706 8%, var(--bg-panel))" : "none",
          border: "none",
          borderRight: mobileBanner ? "none" : "1px solid var(--border)",
          borderBottom: mobileBanner ? "1px solid var(--border)" : "none",
          color: "#d97706",
          cursor: "pointer",
          flexShrink: 0,
          fontSize: 11,
          lineHeight: 1.35,
          textAlign: "left",
        }}
        data-mobile-trust-banner={mobileBanner ? "true" : undefined}
      >
        <svg
          width="13"
          height="13"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
          style={{ flexShrink: 0 }}
        >
          <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10Z" />
          <path d="M12 8v4" />
          <path d="M12 16h.01" />
        </svg>
        <span>{translate("trust.resourcesNotLoaded")}</span>
      </button>
    );
  };

  // 移动端悬浮工具栏的主题 / 语言入口：与 history / branches / system 同一排，
  // 点完保持工具层展开（否则每切一次都要重新点开三点菜单）。
  const renderThemeButton = (mobile: boolean) => (
    <button
      type="button"
      onClick={(event) => {
        const rect = event.currentTarget.getBoundingClientRect();
        toggleTheme({ x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 });
        if (mobile) setMobileToolbarMoreOpen(true);
      }}
      title={translate(themeLabelKey)}
      aria-label={translate(themeLabelKey)}
      style={{
        display: "flex", alignItems: "center", justifyContent: "center",
        width: TOP_BAR_ICON_BUTTON_SIZE, height: "100%", padding: 0,
        background: "none", border: "none", borderRight: "1px solid var(--border)",
        color: "var(--text-muted)", cursor: "pointer", flexShrink: 0, transition: "color 0.12s",
      }}
      onMouseEnter={(event) => { event.currentTarget.style.color = "var(--text)"; }}
      onMouseLeave={(event) => { event.currentTarget.style.color = "var(--text-muted)"; }}
      data-mobile-toolbar-action={mobile ? "theme" : undefined}
    >
      {preference === "light" ? (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <circle cx="12" cy="12" r="5" />
          <line x1="12" y1="1" x2="12" y2="3" />
          <line x1="12" y1="21" x2="12" y2="23" />
          <line x1="4.22" y1="4.22" x2="5.64" y2="5.64" />
          <line x1="18.36" y1="18.36" x2="19.78" y2="19.78" />
          <line x1="1" y1="12" x2="3" y2="12" />
          <line x1="21" y1="12" x2="23" y2="12" />
          <line x1="4.22" y1="19.78" x2="5.64" y2="18.36" />
          <line x1="18.36" y1="5.64" x2="19.78" y2="4.22" />
        </svg>
      ) : preference === "dark" ? (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
        </svg>
      ) : (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <rect x="2" y="3" width="20" height="14" rx="2" />
          <line x1="8" y1="21" x2="16" y2="21" />
          <line x1="12" y1="17" x2="12" y2="21" />
        </svg>
      )}
    </button>
  );

  const renderLanguageButton = (mobile: boolean) => (
    <button
      type="button"
      onClick={() => toggleTopPanel("language", mobile)}
      title={translate("common.language")}
      aria-label={translate("common.language")}
      aria-haspopup="menu"
      aria-expanded={activeTopPanel === "language"}
      aria-pressed={activeTopPanel === "language"}
      style={{
        display: "flex", alignItems: "center", justifyContent: "center",
        width: TOP_BAR_ICON_BUTTON_SIZE, height: "100%", padding: 0,
        background: activeTopPanel === "language" ? "var(--bg-selected)" : "none",
        border: "none", borderRight: "1px solid var(--border)",
        color: activeTopPanel === "language" ? "var(--text)" : "var(--text-muted)",
        cursor: "pointer", flexShrink: 0, transition: "color 0.12s",
      }}
      onMouseEnter={(event) => { event.currentTarget.style.color = "var(--text)"; }}
      onMouseLeave={(event) => {
        event.currentTarget.style.color = activeTopPanel === "language" ? "var(--text)" : "var(--text-muted)";
      }}
      data-mobile-toolbar-action={mobile ? "language" : undefined}
    >
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <path d="m5 8 6 6" />
        <path d="m4 14 6-6 2-3" />
        <path d="M2 5h12" />
        <path d="M7 2h1" />
        <path d="m22 22-5-10-5 10" />
        <path d="M14 18h6" />
      </svg>
    </button>
  );

  const renderChatToolbarActions = (mobile: boolean) => {
    if (!mobile && !showChat) return null;
    return (
      <div className="app-chrome" style={{ display: "flex", alignItems: "center", height: "100%", gap: 2, paddingRight: mobile ? 0 : 8 }}>
        <button
          type="button"
          onClick={() => {
            handleViewFullHistory();
            if (mobile) setMobileToolbarMoreOpen(true);
          }}
          disabled={!selectedSession}
          title={selectedSession ? translate("history.full") : translate("history.unsaved")}
          aria-label={translate("history.full")}
          className={mobile ? undefined : "app-toolbar-btn"}
          style={mobile ? {
            display: "flex", alignItems: "center", justifyContent: "center",
            width: TOP_BAR_ICON_BUTTON_SIZE, height: "100%", padding: 0,
            background: "none", border: "none", borderRight: "1px solid var(--border)",
            color: selectedSession ? "var(--text-muted)" : "var(--text-dim)",
            cursor: selectedSession ? "pointer" : "not-allowed",
            opacity: selectedSession ? 1 : 0.45, flexShrink: 0,
          } : undefined}
          onMouseEnter={(event) => {
            if (!mobile || !selectedSession) return;
            event.currentTarget.style.color = "var(--text)";
            event.currentTarget.style.background = "var(--bg-hover)";
          }}
          onMouseLeave={(event) => {
            if (!mobile) return;
            event.currentTarget.style.color = selectedSession ? "var(--text-muted)" : "var(--text-dim)";
            event.currentTarget.style.background = "none";
          }}
          data-mobile-toolbar-action={mobile ? "history" : undefined}
        >
          <svg
            width={mobile ? 12 : 14}
            height={mobile ? 12 : 14}
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            style={{ flexShrink: 0 }}
            aria-hidden="true"
          >
            <path d="M3 12a9 9 0 1 0 3-6.7L3 8" />
            <path d="M3 3v5h5" />
            <path d="M12 7v5l3 2" />
          </svg>
        </button>
        {mobile ? (
          <button
            type="button"
            onClick={() => toggleTopPanel("branches", true)}
            title={translate("i18n.branches")}
            aria-label={translate("i18n.branches")}
            aria-pressed={activeTopPanel === "branches"}
            style={{
              display: "flex", alignItems: "center", justifyContent: "center",
              width: TOP_BAR_ICON_BUTTON_SIZE, height: "100%", padding: 0,
              background: activeTopPanel === "branches" ? "var(--bg-selected)" : "none",
              border: "none",
              borderTop: activeTopPanel === "branches" ? "2px solid var(--accent)" : "2px solid transparent",
              borderRight: "1px solid var(--border)",
              color: activeTopPanel === "branches" ? "var(--text)" : "var(--text-muted)",
              cursor: "pointer", flexShrink: 0,
            }}
            data-mobile-toolbar-action="branches"
          >
            <ConversationBranchesIcon active={branchTree.length > 0} />
          </button>
        ) : (
          <BranchNavigator
            tree={branchTree}
            activeLeafId={branchActiveLeafId}
            onLeafChange={handleBranchLeafChange}
            inline
            compact
            containerRef={topBarRef}
            open={activeTopPanel === "branches"}
            onToggle={() => toggleTopPanel("branches")}
            hasSession
          />
        )}
        <button
          ref={systemBtnRef}
          type="button"
          onClick={() => handleSystemPromptToggle(mobile)}
          disabled={mobile && !showChat}
          title={translate("system.prompt")}
          aria-label={translate("system.prompt")}
          aria-pressed={activeTopPanel === "system"}
          className={mobile ? undefined : "app-toolbar-btn"}
          data-active={!mobile && activeTopPanel === "system" ? "true" : undefined}
          style={mobile ? {
            display: "flex", alignItems: "center", justifyContent: "center",
            width: TOP_BAR_ICON_BUTTON_SIZE, height: "100%", padding: 0,
            background: activeTopPanel === "system" ? "var(--bg-selected)" : "none",
            border: "none", borderRight: "1px solid var(--border)",
            color: activeTopPanel === "system" ? "var(--text)" : "var(--text-muted)",
            opacity: !showChat ? 0.45 : 1, flexShrink: 0,
          } : undefined}
          onMouseEnter={(event) => {
            if (!mobile || !showChat) return;
            event.currentTarget.style.color = "var(--text)";
          }}
          onMouseLeave={(event) => {
            if (!mobile) return;
            event.currentTarget.style.color = activeTopPanel === "system" ? "var(--text)" : "var(--text-muted)";
          }}
          data-mobile-toolbar-action={mobile ? "system" : undefined}
        >
          <svg width={mobile ? 12 : 14} height={mobile ? 12 : 14} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ color: systemPrompt ? "var(--accent)" : "currentColor", flexShrink: 0 }} aria-hidden="true">
            <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
            <polyline points="14 2 14 8 20 8" />
            <line x1="8" y1="13" x2="16" y2="13" />
            <line x1="8" y1="17" x2="13" y2="17" />
          </svg>
          {mobile && <span>{translate("system.label")}</span>}
        </button>
      </div>
    );
  };

  const renderSessionStatsButton = (mobile: boolean) => {
    if (!mobile && (!showChat || (!sessionStats && !contextUsage))) return null;

    const tokens = sessionStats?.tokens;
    const cost = sessionStats?.cost ?? 0;
    const formatCompact = (value: number) => value >= 1_000_000
      ? `${(value / 1_000_000).toFixed(1)}M`
      : value >= 1000
        ? `${(value / 1000).toFixed(0)}k`
        : String(value);
    const costText = cost > 0 ? (cost >= 0.01 ? `$${cost.toFixed(2)}` : `<$0.01`) : null;

    let contextColor = "var(--text-muted)";
    let desktopContextText: string | null = null;
    let mobileContextText: string | null = null;
    if (contextUsage?.contextWindow) {
      const percent = contextUsage.percent;
      if (percent !== null && percent > 90) contextColor = "#ef4444";
      else if (percent !== null && percent > 70) contextColor = "rgba(234,179,8,0.95)";
      desktopContextText = percent !== null
        ? `${percent.toFixed(0)}% / ${formatCompact(contextUsage.contextWindow)}`
        : `? / ${formatCompact(contextUsage.contextWindow)}`;
      mobileContextText = percent !== null ? `${percent.toFixed(0)}%` : null;
    }

    const tooltipParts: string[] = [];
    if (tokens) {
      tooltipParts.push(`${translate("session.tooltipInput")}: ${tokens.input.toLocaleString(locale)}`);
      tooltipParts.push(`${translate("session.tooltipOutput")}: ${tokens.output.toLocaleString(locale)}`);
      tooltipParts.push(`${translate("session.tooltipCacheRead")}: ${tokens.cacheRead.toLocaleString(locale)}`);
      tooltipParts.push(`${translate("session.tooltipCacheWrite")}: ${tokens.cacheWrite.toLocaleString(locale)}`);
      if (cost > 0) tooltipParts.push(`${translate("session.tooltipCost")}: $${cost.toFixed(4)}`);
    }
    if (contextUsage?.contextWindow) {
      const percent = contextUsage.percent;
      tooltipParts.push(translate("session.tooltipContext", {
        percent: percent !== null ? `${percent.toFixed(1)}%` : translate("i18n.unknown"),
        tokens: contextUsage.contextWindow.toLocaleString(locale),
      }));
    }
    const tooltip = tooltipParts.join("  |  ");
    const covered = mobile && mobileToolbarMoreOpen;
    const hasMobileValues = Boolean(
      (tokens && (tokens.input > 0 || tokens.output > 0))
      || costText
      || mobileContextText,
    );

    return (
      <button
        type="button"
        onClick={() => toggleTopPanel("session")}
        disabled={!showChat || covered}
        tabIndex={covered ? -1 : undefined}
        title={tooltip || translate("session.title")}
        aria-label={translate("session.title")}
        aria-pressed={activeTopPanel === "session"}
        aria-hidden={covered ? true : undefined}
        className={mobile ? "mobile-session-stats" : undefined}
        data-mobile-toolbar-stats={mobile ? "true" : undefined}
        style={{
          marginLeft: mobile ? 0 : 4,
          display: "flex", alignItems: "center", justifyContent: "flex-end",
          flex: mobile ? 1 : undefined,
          minWidth: 0,
          gap: mobile ? 7 : 10,
          padding: mobile ? 0 : "0 8px",
          paddingLeft: mobile ? 6 : 8,
          paddingRight: mobile ? 6 : 8,
          height: mobile ? "100%" : 26,
          borderRadius: mobile ? undefined : "var(--radius-sm)",
          overflow: "hidden",
          visibility: covered ? "hidden" : "visible",
          pointerEvents: covered ? "none" : "auto",
          background: activeTopPanel === "session" ? "var(--bg-selected)" : "none",
          border: "none",
          fontSize: mobile ? 11 : 10.5,
          color: activeTopPanel === "session" ? "var(--text)" : "var(--text-muted)",
          whiteSpace: "nowrap", cursor: showChat ? "pointer" : "default",
          fontVariantNumeric: "tabular-nums",
          transition: "background 120ms, color 120ms",
        }}
        onMouseEnter={(event) => {
          if (showChat && !covered) event.currentTarget.style.color = "var(--text)";
        }}
        onMouseLeave={(event) => {
          event.currentTarget.style.color = activeTopPanel === "session" ? "var(--text)" : "var(--text-muted)";
        }}
      >
        {mobile ? (
          <>
            {tokens && tokens.input > 0 && (
              <span className="mobile-session-stat-io" style={{ display: "flex", alignItems: "center", gap: 2, flexShrink: 0 }}>
                <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <line x1="5" y1="8.5" x2="5" y2="1.5" /><polyline points="2 4 5 1.5 8 4" />
                </svg>
                {formatCompact(tokens.input)}
              </span>
            )}
            {tokens && tokens.output > 0 && (
              <span className="mobile-session-stat-io" style={{ display: "flex", alignItems: "center", gap: 2, flexShrink: 0 }}>
                <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <line x1="5" y1="1.5" x2="5" y2="8.5" /><polyline points="2 6 5 8.5 8 6" />
                </svg>
                {formatCompact(tokens.output)}
              </span>
            )}
            {costText && (
              <span className="mobile-session-stat-cost" style={{ color: "var(--text)", fontWeight: 500, flexShrink: 0 }}>
                {costText}
              </span>
            )}
            {mobileContextText && (
              <span style={{ color: contextColor, flexShrink: 0 }}>
                {mobileContextText}
              </span>
            )}
            {!hasMobileValues && showChat && (
              <span style={{ overflow: "hidden", textOverflow: "ellipsis", color: "var(--text-dim)" }}>
                {translate("session.title")}
              </span>
            )}
          </>
        ) : (
          <>
            {tokens && tokens.input > 0 && (
              <span style={{ display: "flex", alignItems: "center", gap: 4 }}>
                <svg width="12" height="12" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <line x1="5" y1="8.5" x2="5" y2="1.5" /><polyline points="2 4 5 1.5 8 4" />
                </svg>
                {formatCompact(tokens.input)}
              </span>
            )}
            {tokens && tokens.output > 0 && (
              <span style={{ display: "flex", alignItems: "center", gap: 4 }}>
                <svg width="12" height="12" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <line x1="5" y1="1.5" x2="5" y2="8.5" /><polyline points="2 6 5 8.5 8 6" />
                </svg>
                {formatCompact(tokens.output)}
              </span>
            )}
            {tokens && tokens.cacheRead > 0 && (
              <span style={{ display: "flex", alignItems: "center", gap: 4 }}>
                <svg width="12" height="12" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M8.5 5a3.5 3.5 0 1 1-1-2.45" /><polyline points="6.5 1.5 8.5 2.5 7.5 4.5" />
                </svg>
                {formatCompact(tokens.cacheRead)}
              </span>
            )}
            {costText && (
              <span style={{ display: "flex", alignItems: "center", color: "var(--text)", fontWeight: 500 }}>
                {costText}
              </span>
            )}
            {desktopContextText && (
              <span style={{ display: "flex", alignItems: "center", gap: 4, color: contextColor }}>
                <svg width="12" height="12" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M1 9 L1 5 Q1 1 5 1 Q9 1 9 5 L9 9" /><line x1="1" y1="9" x2="9" y2="9" />
                </svg>
                {desktopContextText}
              </span>
            )}
          </>
        )}
      </button>
    );
  };

  const renderMainFileToggle = (mobile: boolean) => {
    if (rightPanelOpen) return null;
    const covered = mobile && mobileToolbarMoreOpen;
    return (
      <button
        type="button"
        onClick={handleRightPanelToggle}
        disabled={covered}
        tabIndex={covered ? -1 : undefined}
        aria-controls="file-panel"
        aria-expanded={rightPanelOpen}
        aria-hidden={covered ? true : undefined}
        title={rightPanelOpen ? translate("files.hidePanel") : translate("files.showPanel")}
        aria-label={rightPanelOpen ? translate("files.hidePanel") : translate("files.showPanel")}
        data-mobile-toolbar-file={mobile ? "true" : undefined}
        className={mobile ? undefined : "app-toolbar-btn"}
        data-active={!mobile && rightPanelOpen ? "true" : undefined}
        style={mobile ? {
          display: "flex", alignItems: "center", justifyContent: "center",
          width: TOP_BAR_ICON_BUTTON_SIZE, height: TOP_BAR_ICON_BUTTON_SIZE, padding: 0,
          visibility: covered ? "hidden" : "visible",
          pointerEvents: covered ? "none" : "auto",
          background: rightPanelOpen ? "var(--bg-selected)" : "none",
          border: "none", borderLeft: "1px solid var(--border)",
          color: rightPanelOpen ? "var(--text)" : "var(--text-muted)",
          cursor: "pointer", flexShrink: 0,
        } : { flexShrink: 0 }}
        onMouseEnter={(event) => { if (!mobile && !covered) event.currentTarget.style.color = "var(--text)"; }}
        onMouseLeave={(event) => { if (!mobile) event.currentTarget.style.color = rightPanelOpen ? "var(--text)" : "var(--text-muted)"; }}
      >
        <svg width={mobile ? 16 : 15} height={mobile ? 16 : 15} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <rect x="3" y="3" width="18" height="18" rx="2" /><line x1="15" y1="3" x2="15" y2="21" />
        </svg>
      </button>
    );
  };

  return (
    <>
    <style>{`
      @keyframes session-info-pop {
        0% {
          opacity: 0;
          transform: translateY(-24px);
          filter: blur(6px);
          box-shadow: 0 2px 8px rgba(0,0,0,0);
        }
        55% {
          opacity: 1;
          transform: translateY(0);
          filter: blur(0);
          background: color-mix(in srgb, var(--accent) 8%, var(--bg-panel));
          box-shadow: 0 18px 44px rgba(37,99,235,0.16);
        }
        100% {
          opacity: 1;
          transform: translateY(0);
          filter: blur(0);
          background: var(--bg-panel);
          box-shadow: 0 10px 28px rgba(0,0,0,0.10);
        }
      }
      @keyframes session-info-light-wash {
        0% {
          opacity: 0;
          transform: translateX(-110%) skewX(-16deg);
        }
        24% {
          opacity: 0.42;
        }
        100% {
          opacity: 0;
          transform: translateX(115%) skewX(-16deg);
        }
      }
      .session-info-popover {
        position: relative;
        overflow: hidden;
        transform-origin: top right;
        animation: session-info-pop 360ms ease-out both;
        will-change: transform, opacity, filter, background, box-shadow;
      }
      .session-info-popover::after {
        content: "";
        position: absolute;
        top: 0;
        bottom: 0;
        left: 0;
        width: 44%;
        pointer-events: none;
        background: linear-gradient(90deg, transparent, color-mix(in srgb, var(--accent) 24%, transparent), transparent);
        animation: session-info-light-wash 620ms ease-out both;
      }
      @media (prefers-reduced-motion: reduce) {
        .session-info-popover,
        .session-info-popover::after {
          animation: none;
        }
      }
      .mobile-session-stats {
        container-type: inline-size;
      }
      @container (max-width: 158px) {
        .mobile-session-stat-io {
          display: none !important;
        }
      }
      @container (max-width: 88px) {
        .mobile-session-stat-cost {
          display: none !important;
        }
      }
      @media (max-width: 640px) {
        .sidebar-overlay-backdrop.sidebar-mobile-pending {
          opacity: 0 !important;
          pointer-events: none !important;
        }
        .sidebar-container.sidebar-mobile-pending.sidebar-open {
          transform: translateX(calc(-100% - env(safe-area-inset-left)));
          box-shadow: none;
        }
      }
    `}</style>
    <div style={{
      display: "flex",
      width: "100%",
      height: "var(--app-viewport-height, 100dvh)",
      paddingLeft: "env(safe-area-inset-left)",
      paddingRight: "env(safe-area-inset-right)",
      overflow: "hidden",
      background: "var(--bg)",
    }}>
      {/* Mobile overlay backdrop */}
      <div
        className={`sidebar-overlay-backdrop${mobileSidebarReady ? "" : " sidebar-mobile-pending"}`}
        onClick={() => setSidebarOpen(false)}
        style={{
          position: "fixed",
          inset: 0,
          zIndex: 199,
          background: "rgba(0,0,0,0.4)",
          opacity: sidebarOpen ? 1 : 0,
          pointerEvents: sidebarOpen ? "auto" : "none",
          transition: "opacity 0.25s ease",
        }}
      />

      {/* Left sidebar */}
      <div
        ref={sidebarResizer.panelRef}
        id="session-sidebar"
        className={`sidebar-container${sidebarOpen ? " sidebar-open" : " sidebar-closed"}${mobileSidebarReady ? "" : " sidebar-mobile-pending"}${sidebarResizer.isResizing ? " sidebar-resizing" : ""}`}
        style={{
          "--sidebar-width": `${sidebarResizer.width}px`,
          /* 外壳面用 --bg-chrome：侧栏与顶部条是“应用壳”，不是内容。 */
          background: "var(--bg-chrome)",
          borderRight: "none",
          display: "flex",
          flexDirection: "column",
          flexShrink: 0,
          paddingTop: "env(safe-area-inset-top)",
          paddingBottom: "env(safe-area-inset-bottom)",
          zIndex: gitPanelOpen ? 1200 : 200,
        } as React.CSSProperties}
      >
        {sidebarContent}
      </div>
      {sidebarOpen && (
        <div
          {...sidebarResizer.separatorProps}
          aria-controls="session-sidebar"
          className={`panel-resize-handle sidebar-resize-handle${sidebarResizer.isResizing ? " is-resizing" : ""}`}
          data-resize-handle="sidebar"
          title={`${translate("layout.resizeSidebar")}: ${translate("layout.resizeHint")}`}
        />
      )}

      {/* Center: chat */}
      <div style={{ flex: 1, display: "flex", flexDirection: "column", overflow: "hidden", minWidth: 0 }}>
        {/* Top bar keeps the sidebar opener available after the sidebar is hidden. */}
        <div
          ref={topBarRef}
          className="app-chrome app-topbar"
          data-sidebar-open={sidebarOpen ? "true" : "false"}
          // drag-region="deep"：Tauri 的裸属性只在直接点中这个元素时生效，
          // 而这条带子里铺满了子节点（标题、间距），鼠标几乎不可能点中它本身。
          // deep 让子树里的空白区域也能拖，按钮/链接/输入框依旧会拦住拖拽。
          data-tauri-drag-region="deep"
          style={{ flexShrink: 0, background: "var(--bg-chrome)" }}
        >
        <div
          style={{ display: "flex", alignItems: "center", position: "relative", height: `calc(${CHAT_TOP_BAR_HEIGHT}px + env(safe-area-inset-top))`, paddingTop: "env(safe-area-inset-top)" }}
        >
          {!sidebarOpen && <button
            onClick={handleSidebarToggle}
             title={sidebarOpen ? translate("sidebar.hide") : translate("sidebar.show")}
             aria-label={sidebarOpen ? translate("sidebar.hide") : translate("sidebar.show")}
            className={isMobile ? undefined : "app-toolbar-btn"}
            style={isMobile ? {
              display: "flex", alignItems: "center", justifyContent: "center",
              width: TOP_BAR_ICON_BUTTON_SIZE, height: TOP_BAR_ICON_BUTTON_SIZE, padding: 0,
              background: "none", border: "none", borderRight: "1px solid var(--border)",
              color: "var(--text-muted)", cursor: "pointer", flexShrink: 0,
            } : { marginLeft: 8, flexShrink: 0 }}
            >
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <rect x="3" y="3" width="18" height="18" rx="2" /><line x1="9" y1="3" x2="9" y2="21" />
              </svg>
          </button>}
          {isMobile && (
            <div
              ref={mobileToolbarRef}
              data-mobile-toolbar="true"
              style={{
                position: "relative",
                display: "flex",
                alignItems: "stretch",
                flex: 1,
                minWidth: 0,
                height: "100%",
              }}
            >
            <button
              type="button"
              onClick={handleMobileToolbarMoreToggle}
              title={mobileToolbarMoreOpen ? translate("chat.close") : translate("chat.moreControls")}
              aria-label={mobileToolbarMoreOpen ? translate("chat.close") : translate("chat.moreControls")}
              aria-controls="mobile-toolbar-actions"
              aria-expanded={mobileToolbarMoreOpen}
              data-mobile-toolbar-more="true"
              style={{
                position: "relative",
                zIndex: mobileToolbarMoreOpen ? 21 : undefined,
                display: "flex", alignItems: "center", justifyContent: "center",
                width: TOP_BAR_ICON_BUTTON_SIZE, height: TOP_BAR_ICON_BUTTON_SIZE, padding: 0,
                background: mobileToolbarMoreOpen ? "var(--bg-selected)" : "none",
                border: "none", borderRight: "1px solid var(--border)",
                color: mobileToolbarMoreOpen ? "var(--text)" : "var(--text-muted)",
                cursor: "pointer", flexShrink: 0, transition: "color 0.12s, background 0.12s",
              }}>
                {mobileToolbarMoreOpen ? (
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true">
                    <line x1="5" y1="5" x2="19" y2="19" /><line x1="19" y1="5" x2="5" y2="19" />
                  </svg>
                ) : (
                  <svg width="17" height="17" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                    <circle cx="5" cy="12" r="1.5" /><circle cx="12" cy="12" r="1.5" /><circle cx="19" cy="12" r="1.5" />
                  </svg>
                )}
              </button>
              {renderSessionStatsButton(true)}
              {renderMainFileToggle(true)}
              {mobileToolbarMoreOpen && (
                <div
                  id="mobile-toolbar-actions"
                  role="toolbar"
                  aria-label={translate("chat.moreControls")}
                  data-mobile-toolbar-actions="true"
                  style={{
                    position: "absolute",
                    top: 0,
                    right: 0,
                    bottom: 0,
                    left: TOP_BAR_ICON_BUTTON_SIZE,
                    zIndex: 20,
                    display: "flex",
                    alignItems: "stretch",
                    background: "color-mix(in srgb, var(--bg-panel) 94%, var(--bg))",
                    boxShadow: "4px 0 18px rgba(0,0,0,0.12)",
                    backdropFilter: "blur(10px)",
                  }}
                >
                  {renderChatToolbarActions(true)}
                  {renderThemeButton(true)}
                  {renderLanguageButton(true)}
                </div>
              )}
            </div>
          )}
          {!isMobile && (
            <>
              {renderProjectTrustWarning(false)}
              {showChat && (
                <div className="app-topbar-title" title={topBarTitle}>
                  <span className="app-topbar-title-text">{topBarTitle}</span>
                </div>
              )}
              {renderChatToolbarActions(false)}
              {renderSessionStatsButton(false)}
            </>
          )}
          {!isMobile && renderMainFileToggle(false)}
          {isMobile && (
            <BranchNavigator
              tree={branchTree}
              activeLeafId={branchActiveLeafId}
              onLeafChange={handleBranchLeafChange}
              inline
              compact
              containerRef={topBarRef}
              open={activeTopPanel === "branches"}
              onToggle={() => toggleTopPanel("branches")}
              hasSession={showChat}
              hideInlineButton
            />
          )}
          {/* Top panel dropdown — shared, only one active at a time */}
          {activeTopPanel && topPanelPos && (
            <div
              // 下拉面板在标题栏的 drag-region 子树里（它挂在 topBarRef 下），
              // 不显式关掉的话，在面板里点一下按钮/拖动选择都会变成拖窗口。
              data-tauri-drag-region="false"
              style={{
              position: "fixed",
              top: topPanelPos.top,
              left: topPanelPos.left,
              width: topPanelPos.width,
              maxHeight: `calc(100dvh - ${topPanelPos.top}px)`,
              overflowY: "auto",
              zIndex: 500,
            }}>
              {activeTopPanel === "system" && (
                <SystemPromptPanel prompt={systemPrompt} loading={systemPromptLoading} />
              )}
              {activeTopPanel === "language" && (
                <div
                  role="menu"
                  style={{
                    background: "var(--bg-panel)",
                    borderBottom: "1px solid var(--border)",
                    boxShadow: "0 10px 28px rgba(0,0,0,0.10)",
                    padding: "6px 0",
                  }}
                >
                  {supportedLocales.map((item) => (
                    <button
                      key={item.id}
                      type="button"
                      role="menuitemradio"
                      aria-checked={item.id === locale}
                      onClick={() => {
                        setLocale(item.id as typeof locale);
                        setActiveTopPanel(null);
                      }}
                      style={{
                        display: "flex", alignItems: "center", justifyContent: "space-between",
                        gap: 12, width: "100%", padding: "8px 16px",
                        background: item.id === locale ? "var(--bg-selected)" : "none",
                        border: "none",
                        color: item.id === locale ? "var(--text)" : "var(--text-muted)",
                        cursor: "pointer", fontSize: 12, textAlign: "left",
                      }}
                    >
                      <span>{item.label}</span>
                      {item.id === locale && (
                        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="var(--accent)" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                          <polyline points="20 6 9 17 4 12" />
                        </svg>
                      )}
                    </button>
                  ))}
                </div>
              )}
              {activeTopPanel === "session" && (
                <div className="session-info-popover" style={{
                  background: "var(--bg-panel)",
                  borderBottom: "1px solid var(--border)",
                  boxShadow: "0 10px 28px rgba(0,0,0,0.10)",
                  padding: "12px 16px",
                }}>
                  {sessionStats ? (() => {
                    const formatDuration = (ms: number) => {
                      if (ms <= 0) return translate("session.durationSeconds", { count: 0 });
                      const totalSec = Math.floor(ms / 1000);
                      const h = Math.floor(totalSec / 3600);
                      const m = Math.floor((totalSec % 3600) / 60);
                      const s = totalSec % 60;
                      if (h > 0) return translate("session.durationHours", { hours: h, minutes: m });
                      if (m > 0) return translate("session.durationMinutes", { minutes: m, seconds: s });
                      return translate("session.durationSeconds", { count: s });
                    };
                    const totalActiveMs = sessionStats.totalActiveMs ?? 0;
                    const sessionRows = [
                       ...(sessionStats.sessionName ? [{ label: translate("session.name"), value: sessionStats.sessionName, copyField: null }] : []),
                       { label: translate("session.file"), value: sessionStats.sessionFile ?? translate("session.inMemory"), copyField: "file" as const },
                       { label: translate("session.id"), value: sessionStats.sessionId, copyField: "id" as const },
                       ...(totalActiveMs > 0 ? [{ label: translate("session.totalActive"), value: formatDuration(totalActiveMs), copyField: null }] : []),
                    ];
                    const messageRows = [
                       [translate("session.user"), sessionStats.userMessages.toLocaleString(locale)],
                       [translate("session.assistant"), sessionStats.assistantMessages.toLocaleString(locale)],
                       [translate("session.toolCalls"), sessionStats.toolCalls.toLocaleString(locale)],
                       [translate("session.toolResults"), sessionStats.toolResults.toLocaleString(locale)],
                       [translate("session.total"), sessionStats.totalMessages.toLocaleString(locale)],
                    ];
                    const tokenRows = [
                       [translate("session.input"), sessionStats.tokens.input.toLocaleString(locale)],
                       [translate("session.output"), sessionStats.tokens.output.toLocaleString(locale)],
                       ...(sessionStats.tokens.cacheRead > 0 ? [[translate("session.cacheRead"), sessionStats.tokens.cacheRead.toLocaleString(locale)]] : []),
                       ...(sessionStats.tokens.cacheWrite > 0 ? [[translate("session.cacheWrite"), sessionStats.tokens.cacheWrite.toLocaleString(locale)]] : []),
                       [translate("session.total"), sessionStats.tokens.total.toLocaleString(locale)],
                    ];
                    const ctx = contextUsage ?? sessionStats.contextUsage;
                    const formatCompact = (n: number) => n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : n >= 1000 ? `${(n / 1000).toFixed(0)}k` : String(n);
                    const extraTokenRows = [
                       ...(sessionStats.cost > 0 ? [[translate("session.cost"), `$${sessionStats.cost.toFixed(4)}`]] : []),
                       ...(ctx?.contextWindow ? [[translate("session.context"), `${ctx.percent !== null ? `${ctx.percent.toFixed(1)}%` : translate("i18n.unknown")} / ${formatCompact(ctx.contextWindow)}`]] : []),
                       // Cache hit rate = cache reads / (input + cache writes + cache reads) — the denominator covers all input-class tokens.
                       ...(sessionStats.tokens.cacheRead + sessionStats.tokens.cacheWrite > 0 && sessionStats.tokens.cacheRead + sessionStats.tokens.cacheWrite + sessionStats.tokens.input > 0
                         ? [[translate("session.cacheHitRate"), `${(sessionStats.tokens.cacheRead / (sessionStats.tokens.cacheRead + sessionStats.tokens.cacheWrite + sessionStats.tokens.input) * 100).toFixed(1)}%`]]
                         : []),
                    ];
                    const section = (
                      title: string,
                      sectionRows: string[][],
                      valueAlign: "left" | "right" = "left",
                      compact = false,
                    ) => (
                        <div style={{ minWidth: 0 }}>
                          <div style={{ fontSize: 11, fontWeight: 700, color: "var(--text)", marginBottom: 6 }}>{title}</div>
                          <div style={{
                            display: "grid",
                            gridTemplateColumns: compact ? "max-content max-content" : "auto minmax(0, 1fr)",
                            columnGap: compact ? 14 : 12,
                            rowGap: 4,
                            justifyContent: compact ? "start" : undefined,
                          }}>
                            {sectionRows.map(([label, value]) => (
                              <div key={`${title}:${label}`} style={{ display: "contents" }}>
                                <div style={{ color: "var(--text-dim)", whiteSpace: "nowrap" }}>{label}</div>
                                <div style={{
                                  color: "var(--text-muted)",
                                  minWidth: 0,
                                  overflowWrap: compact ? "normal" : "anywhere",
                                  textAlign: valueAlign,
                                  whiteSpace: valueAlign === "right" ? "nowrap" : "normal",
                                }}>{value}</div>
                              </div>
                            ))}
                          </div>
                        </div>
                      );
                    const copyButton = (field: SessionCopyField, value: string) => {
                      const copied = copiedSessionField === field;
                      return (
                        <button
                          type="button"
                           title={copied ? translate("session.copied") : translate(field === "file" ? "session.copyFile" : "session.copyId")}
                          onClick={() => handleCopySessionField(field, value)}
                          style={{
                            alignSelf: "start",
                            display: "inline-flex",
                            alignItems: "center",
                            justifyContent: "center",
                            width: 22,
                            height: 22,
                            marginTop: -2,
                            color: copied ? "var(--accent)" : "var(--text-dim)",
                            background: "transparent",
                            border: "1px solid var(--border)",
                            borderRadius: 4,
                            cursor: "pointer",
                            flex: "0 0 auto",
                            transition: "color 0.12s, border-color 0.12s, background 0.12s",
                          }}
                          onMouseEnter={(e) => {
                            e.currentTarget.style.color = "var(--accent)";
                            e.currentTarget.style.borderColor = "var(--accent)";
                            e.currentTarget.style.background = "var(--bg-hover)";
                          }}
                          onMouseLeave={(e) => {
                            e.currentTarget.style.color = copied ? "var(--accent)" : "var(--text-dim)";
                            e.currentTarget.style.borderColor = "var(--border)";
                            e.currentTarget.style.background = "transparent";
                          }}
                        >
                          {copied ? (
                            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                              <polyline points="20 6 9 17 4 12" />
                            </svg>
                          ) : (
                            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                              <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
                              <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
                            </svg>
                          )}
                        </button>
                      );
                    };
                    const sessionInfoSection = (
                      <div style={{ minWidth: 0 }}>
                         <div style={{ fontSize: 11, fontWeight: 700, color: "var(--text)", marginBottom: 6 }}>{translate("session.infoSection")}</div>
                        <div style={{ display: "grid", gridTemplateColumns: "auto minmax(0, 1fr) auto", columnGap: 12, rowGap: 8, alignItems: "start" }}>
                          {sessionRows.map((row) => (
                            <div key={`session-info:${row.label}`} style={{ display: "contents" }}>
                              <div style={{ color: "var(--text-dim)", whiteSpace: "nowrap" }}>{row.label}</div>
                              <div style={{
                                color: "var(--text-muted)",
                                minWidth: 0,
                                overflowWrap: "anywhere",
                                wordBreak: "break-word",
                                whiteSpace: "normal",
                              }}>{row.value}</div>
                              <div>{row.copyField ? copyButton(row.copyField, row.value) : null}</div>
                            </div>
                          ))}
                        </div>
                      </div>
                    );

                    return (
                      <div style={{
                        display: "grid",
                        gridTemplateColumns: isMobile
                          ? "1fr"
                          : "minmax(360px, 1.7fr) minmax(140px, 0.55fr) minmax(190px, 0.75fr)",
                        gap: isMobile ? 16 : 24,
                        fontSize: 12,
                        lineHeight: 1.5,
                        fontFamily: "var(--font-mono)",
                      }}>
                        {sessionInfoSection}
                         {section(translate("session.messages"), messageRows)}
                         {section(translate("session.tokens"), [...tokenRows, ...extraTokenRows], "right", true)}
                      </div>
                    );
                  })() : (
                    <div style={{ fontSize: 12, color: "var(--text-muted)", fontStyle: "italic" }}>
                       {translate("session.load")}
                    </div>
                  )}
                </div>
              )}
            </div>
          )}

        </div>
        {isMobile && renderProjectTrustWarning(true)}
        </div>

        {/* Chat content */}
        <div style={{ flex: 1, overflow: "hidden", position: "relative" }}>
          {showChat ? (
            <ChatWindow
              key={sessionKey}
              session={selectedSession}
              sessionRunning={Boolean(selectedSession && runningSessionIds.has(selectedSession.id))}
              newSessionCwd={effectiveNewSessionCwd}
              newSessionDraftKey={newSessionDraftKey}
              onAgentEnd={handleAgentEnd}
              onAttentionNeeded={handleAttentionNeeded}
              onSessionCreated={handleSessionCreated}
              onSessionListRefresh={handleSessionListRefresh}
              onSessionForked={handleSessionForked}
              modelsRefreshKey={modelsRefreshKey}
              chatInputRef={chatInputRef}
              onBranchDataChange={handleBranchDataChange}
              onSystemPromptChange={handleSystemPromptChange}
              onSystemPromptLoaderChange={handleSystemPromptLoaderChange}
              onSessionStatsChange={handleSessionStatsChange}
              onSessionStatsPanelOpen={openSessionStatsPanel}
              onNewSessionRequest={handleNewSessionRequest}
              onContextUsageChange={handleContextUsageChange}
              onScrollContainerChange={handleChatScrollContainerChange}
              onOpenFile={handleOpenLinkedFile}
              onOpenConsole={handleOpenConsole}
              consoleActive={consoleOpen && terminalCwds.includes(selectedSession?.cwd ?? effectiveNewSessionCwd ?? "")}
              soundEnabled={soundEnabled}
              playDoneSound={playDoneSound}
              unlockAudio={unlockAudio}
            />
          ) : initialCwdStatus === "validating" ? (
            <div
              role="status"
              style={{ height: "100%", display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", gap: 8, padding: 24, color: "var(--text-muted)", textAlign: "center" }}
            >
               <div style={{ fontSize: 14, color: "var(--text)" }}>{translate("workspace.opening")}</div>
              <div style={{ maxWidth: "min(720px, 100%)", overflowWrap: "anywhere", fontFamily: "var(--font-mono)", fontSize: 12 }}>
                {initialNavigation.requestedCwd}
              </div>
            </div>
          ) : initialCwdStatus === "error" ? (
            <div
              role="alert"
              style={{ height: "100%", display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", gap: 8, padding: 24, color: "var(--text-muted)", textAlign: "center" }}
            >
               <div style={{ fontSize: 14, color: "#dc2626" }}>{translate("workspace.unable")}</div>
              <div style={{ maxWidth: "min(720px, 100%)", overflowWrap: "anywhere", fontFamily: "var(--font-mono)", fontSize: 12 }}>
                {initialNavigation.requestedCwd}
              </div>
              <div style={{ maxWidth: 720, fontSize: 12 }}>{initialCwdError}</div>
            </div>
          ) : showPlaceholder ? (
            activeCwd ? (
              <div style={{ height: "100%", display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-muted)", fontSize: 15 }}>
                 {translate("workspace.selectSession")}
              </div>
            ) : null
          ) : null}
        </div>
        {terminalCwds.map((cwd) => (
          <TerminalPanel
            key={cwd}
            cwd={cwd}
            visible={consoleOpen && cwd === (selectedSession?.cwd ?? newSessionCwd ?? activeCwd)}
            onMinimize={() => setConsoleOpen(false)}
            onClose={() => {
              setTerminalCwds((current) => current.filter((value) => value !== cwd));
              setConsoleOpen(false);
            }}
          />
        ))}
      </div>

      {gitPanelOpen && gitPanelTargetCwd && (
        // One panel instance per repository: switching to another sub-project
        // must not carry over the previous repository's commit draft, credential
        // form, or discard dialog.
        <GitPanel key={gitPanelTargetCwd} cwd={gitPanelTargetCwd} sessionId={selectedSession?.id ?? null} onClose={closeGitPanel} onChanged={handleExplorerRefresh} onOpenFile={handleOpenFile} />
      )}

      <div
        aria-hidden="true"
        className={`right-panel-overlay-backdrop${rightPanelOpen ? " is-open" : ""}`}
        onClick={() => setRightPanelOpen(false)}
      />
      {rightPanelOpen && (
        <div
          {...rightPanelResizer.separatorProps}
          aria-controls="file-panel"
          className={`panel-resize-handle right-panel-resize-handle${rightPanelResizer.isResizing ? " is-resizing" : ""}`}
          data-resize-handle="right-panel"
          title={`${translate("layout.resizeFilePanel")}: ${translate("layout.resizeHint")}`}
        />
      )}

      {/* Right panel: files — width animated via CSS */}
      <div
        ref={rightPanelResizer.panelRef}
        id="file-panel"
        className={`right-panel-container${rightPanelOpen ? " right-panel-open" : " right-panel-closed"}${rightPanelResizer.isResizing ? " right-panel-resizing" : ""}`}
        style={{
          "--right-panel-width": `${rightPanelResizer.width}px`,
          display: "flex",
          flexDirection: "column",
          borderLeft: "1px solid var(--border)",
          background: "var(--bg)",
        } as React.CSSProperties}
      >
        {/* Right panel tab bar */}
        <div style={{
          display: "flex",
          /* 标签栏自己会被下面那个 42px 高度钉住，但它的子元素（标签条）
             要能沿交叉轴撑满，所以不能在这里写 align-items: center——
             那样标签条只会拿到内容高度，标签看起来就只占标题栏中间一条。 */
          alignItems: "stretch",
          flexShrink: 0,
          /* 与对话顶栏同高：三条竖区（侧栏 / 对话 / 文件面板）的横向分隔线
             必须在同一条 y 上，否则一眼就能看出是拼起来的。 */
          height: `calc(${CHAT_TOP_BAR_HEIGHT}px + env(safe-area-inset-top))`,
          paddingTop: "env(safe-area-inset-top)",
          background: "var(--bg-chrome)",
          borderBottom: "1px solid var(--border)",
        }}>
          {/* 这一层只负责横向占用与溢出裁剪；高度必须由它撑满，
             否则里层的 height: 100% 量到的是一个被内容顶开的盒。 */}
          <div style={{ flex: 1, minHeight: 0, overflow: "hidden", alignSelf: "stretch", display: "flex" }}>
            <TabBar
              tabs={fileTabs}
              activeTabId={activeFileTabId ?? ""}
              onSelectTab={setActiveFileTabId}
              onCloseTab={handleCloseFileTab}
            />
          </div>
          <button
            type="button"
            onClick={() => setRightPanelOpen(false)}
            aria-controls="file-panel"
            aria-expanded={rightPanelOpen}
            title={translate("files.hidePanel")}
            aria-label={translate("files.hidePanel")}
            className="app-toolbar-btn"
            style={{
              marginRight: 8, flexShrink: 0, alignSelf: "center",
            }}
            onMouseEnter={(event) => { event.currentTarget.style.color = "var(--accent)"; }}
            onMouseLeave={(event) => { event.currentTarget.style.color = ""; }}
          >
            <svg width={15} height={15} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <rect x="3" y="3" width="18" height="18" rx="2" /><line x1="15" y1="3" x2="15" y2="21" />
            </svg>
          </button>
        </div>

        {/* Only the active panel body is mounted. Lightweight per-tab state is restored on activation. */}
        <div style={{ flex: 1, overflow: "hidden", paddingBottom: "env(safe-area-inset-bottom)" }}>
          {activeFileTab?.filePath ? (
            <FileViewer
              key={`${activeFileTab.id}:${activeFileTab.viewerRevision ?? 0}`}
              filePath={activeFileTab.filePath}
              cwd={activeCwd ?? undefined}
              sourceSessionId={activeFileTab.sourceSessionId}
              gitRefreshKey={explorerRefreshKey}
              initialDisplayMode={activeFileTab.initialDisplayMode}
              initialState={activeFileTab.viewerState}
              initialLine={activeFileTab.revealLine}
              onRevealHandled={() => setFileTabs((prev) => markFileTabRevealHandled(prev, activeFileTab.id))}
              watchEnabled={rightPanelOpen}
              onStateChange={(viewerState) => handleFileViewerStateChange(
                activeFileTab.id,
                activeFileTab.viewerRevision ?? 0,
                viewerState,
              )}
              onMentionLines={rightPanelOpen ? handleFileLineMention : undefined}
              onAtMention={handleAtMention}
              onOpenFile={(filePath, location) => handleOpenFile(
                filePath,
                getFileName(filePath),
                { sourceSessionId: activeFileTab.sourceSessionId, line: location?.line },
              )}
              editable={true}
              tabId={activeFileTab.id}
              sessionId={selectedSession?.id}
              onDirtyChange={handleFileTabDirtyChange}
            />
          ) : (
            <div style={{ height: "100%", display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-dim)", fontSize: 12 }}>
               {translate("files.noneOpen")}
            </div>
          )}
        </div>
      </div>
    </div>
    {settingsOpen && (
      <SettingsPanel
        cwd={projectTrustCwd ?? null}
        hasProject={Boolean(projectTrustCwd)}
        projectTrusted={projectTrust?.trusted === true}
        sessionId={selectedSession?.id ?? null}
        onClose={() => setSettingsOpen(false)}
        onModelsSaved={() => setModelsRefreshKey((key) => key + 1)}
        onMcpConfigured={() => {
          setSessionKey((key) => key + 1);
          setExplorerRefreshKey((key) => key + 1);
        }}
        onSessionReloaded={() => setSessionKey((key) => key + 1)}
        soundEnabled={soundEnabled}
        onSoundToggle={onSoundToggle}
        bannerEnabled={bannerEnabled}
        onBannerToggle={() => {
          setBannerEnabledState((current) => {
            const next = !current;
            setBannerEnabled(next);
            return next;
          });
        }}
        themePreference={preference as "light" | "dark" | "auto"}
        onThemeChange={(next) => {
          if (next === preference) return;
          const nextTheme = resolveTheme(next);
          setThemeState(next, nextTheme, true);
        }}
        locale={locale}
        onLocaleChange={(next: string) => setLocale(next as typeof locale)}
        supportedLocales={supportedLocales.map((plugin) => ({ id: plugin.id, label: plugin.label }))}
      />
    )}
    {/* 独立入口：从侧边栏标题行的 ⓘ 打开（设置面板里另有一个嵌入实例）。 */}
    {aboutOpen && !settingsOpen && (
      <AboutDialog onClose={() => setAboutOpen(false)} />
    )}
    {projectTrustDialogOpen && projectTrustCwd && (
      <ProjectTrustDialog
        cwd={projectTrustCwd}
        busy={projectTrustBusy}
        error={projectTrustError}
        onCancel={() => {
          if (!projectTrustBusy) setProjectTrustDialogOpen(false);
        }}
        onConfirm={() => void handleTrustProject()}
      />
    )}
    </>
  );
}
