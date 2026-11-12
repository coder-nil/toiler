import type { FileViewerState } from "@/lib/file-viewer-state";

function normalizeFilePathSlashes(filePath: string): string {
  return /^[a-zA-Z]:[\\/]/.test(filePath) || filePath.startsWith("\\\\")
    ? filePath.replace(/\\/g, "/")
    : filePath;
}

function getFileName(filePath: string): string {
  const normalized = normalizeFilePathSlashes(filePath).replace(/\/+$/, "");
  return normalized.split("/").pop() ?? normalized;
}
import type { Tab } from "./TabBar";

interface OpenFileTabInput {
  fileName: string;
  filePath: string;
  modeHint?: "diff";
  /** 1-based line the link pointed at, revealed once by the viewer. */
  line?: number;
  sourceSessionId?: string | null;
  tabId: string;
}

export function openFileTab(tabs: Tab[], input: OpenFileTabInput): Tab[] {
  const existing = tabs.find((tab) => tab.id === input.tabId);
  if (!existing) {
    return [...tabs, {
      id: input.tabId,
      label: input.fileName,
      filePath: input.filePath,
      kind: "file",
      sourceSessionId: input.sourceSessionId,
      initialDisplayMode: input.modeHint,
      revealLine: input.line,
      viewerState: input.modeHint ? {
        displayMode: input.modeHint,
        wrapLines: false,
        scrollTop: 0,
        scrollLeft: 0,
      } : undefined,
      viewerRevision: 0,
    }];
  }

  const sourceChanged = Boolean(
    input.sourceSessionId && existing.sourceSessionId !== input.sourceSessionId,
  );
  // 有未写入磁盘的草稿时忽略显示模式提示：重挂载会丢掉草稿，而点回一个正在改的文件
  // 的人要的是接着改，不是突然看到一个 diff。
  const modeHint = existing.dirty ? undefined : input.modeHint;
  const wantsReveal = input.line !== undefined;
  if (!sourceChanged && !modeHint && !wantsReveal) return tabs;

  return tabs.map((tab) => {
    if (tab.id !== input.tabId) return tab;
    const next: Tab = { ...tab };
    if (sourceChanged) next.sourceSessionId = input.sourceSessionId;
    if (modeHint) {
      next.initialDisplayMode = modeHint;
      next.viewerState = {
        displayMode: modeHint,
        wrapLines: tab.viewerState?.wrapLines ?? false,
        scrollTop: 0,
        scrollLeft: 0,
      };
    }
    if (wantsReveal) next.revealLine = input.line;
    // 定位要用新的初始行重挂载一次；单纯换数据源或切换模式也要重建。
    if (sourceChanged || modeHint || wantsReveal) {
      next.viewerRevision = (tab.viewerRevision ?? 0) + 1;
    }
    return next;
  });
}

function pathMatchesOrContains(candidatePath: string, parentPath: string): boolean {
  const candidate = normalizeFilePathSlashes(candidatePath).replace(/\/+$/, "");
  const parent = normalizeFilePathSlashes(parentPath).replace(/\/+$/, "");
  const caseInsensitive = /^[a-zA-Z]:\//.test(candidate) || candidate.startsWith("//");
  const left = caseInsensitive ? candidate.toLowerCase() : candidate;
  const right = caseInsensitive ? parent.toLowerCase() : parent;
  return left === right || left.startsWith(`${right}/`);
}

export function renameFileTab(
  tabs: Tab[],
  oldPath: string,
  newPath: string,
): Tab[] {
  let changed = false;
  const next = tabs.map((tab) => {
    if (tab.kind === "console" || !pathMatchesOrContains(tab.filePath, oldPath)) return tab;
    changed = true;
    const suffix = normalizeFilePathSlashes(tab.filePath).slice(normalizeFilePathSlashes(oldPath).length);
    const filePath = `${normalizeFilePathSlashes(newPath)}${suffix}`;
    return {
      ...tab,
      id: `file:${filePath}`,
      label: getFileName(filePath),
      filePath,
      viewerRevision: (tab.viewerRevision ?? 0) + 1,
    };
  });
  return changed ? next : tabs;
}

/** 查看器已经跳到目标行，丢掉待跳标记，这样切走再切回来不会重复跳。 */
export function markFileTabRevealHandled(tabs: Tab[], tabId: string): Tab[] {
  const index = tabs.findIndex((tab) => tab.id === tabId);
  if (index === -1 || tabs[index].revealLine === undefined) return tabs;
  const next = [...tabs];
  const tab = { ...next[index] };
  delete tab.revealLine;
  next[index] = tab;
  return next;
}

export function saveFileViewerState(
  tabs: Tab[],
  tabId: string,
  viewerRevision: number,
  viewerState: FileViewerState,
): Tab[] {
  const index = tabs.findIndex((tab) => tab.id === tabId);
  if (index === -1 || (tabs[index].viewerRevision ?? 0) !== viewerRevision) return tabs;

  const currentState = tabs[index].viewerState;
  if (
    currentState?.displayMode === viewerState.displayMode &&
    currentState.wrapLines === viewerState.wrapLines &&
    currentState.scrollTop === viewerState.scrollTop &&
    currentState.scrollLeft === viewerState.scrollLeft
  ) {
    return tabs;
  }

  const next = [...tabs];
  next[index] = { ...next[index], viewerState };
  return next;
}

/**
 * 同步查看器的未保存标记。
 *
 * 编辑里的草稿只存在于 FileViewer 内部，dirty 是唯一流到 Tab 层的信号：
 * 它驱动标签上的圆点与关闭前的确认。false 是已保存后的常态，重复
 * 写同一个值不会产生新数组，避免每次自动保存都重渲染整个 Tab 栏。
 */
export function saveFileDirtyState(tabs: Tab[], tabId: string, dirty: boolean): Tab[] {
  const index = tabs.findIndex((tab) => tab.id === tabId);
  if (index === -1 || Boolean(tabs[index].dirty) === dirty) return tabs;
  const next = [...tabs];
  next[index] = dirty ? { ...next[index], dirty: true } : (() => {
    const tab = { ...next[index] };
    delete tab.dirty;
    return tab;
  })();
  return next;
}
