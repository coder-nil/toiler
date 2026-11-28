export const MOBILE_MAX_WIDTH = 640;
export const SPLIT_PANEL_MIN_WIDTH = 960;

export const SIDEBAR_DEFAULT_WIDTH = 260;
export const SIDEBAR_MIN_WIDTH = 180;
export const SIDEBAR_MAX_WIDTH = 480;

export const RIGHT_PANEL_FALLBACK_WIDTH = 560;
export const RIGHT_PANEL_MIN_WIDTH = 300;
export const RIGHT_PANEL_MAX_WIDTH = 1200;

export const GIT_WORKSPACE_SPLITTER_HEIGHT = 8;

/** 短窗口仍为两边保留空间，宽度改用标签页时不会影响纵向分隔值。 */
export function getGitWorkspaceSplitBounds(height: number) {
  const available = Math.max(0, Math.floor(height) - GIT_WORKSPACE_SPLITTER_HEIGHT);
  return {
    min: Math.min(230, Math.floor(available * 0.52)),
    max: available - Math.min(132, Math.floor(available * 0.34)),
    initial: Math.round(available * 0.53),
  };
}

export const GIT_HISTORY_REFS_DEFAULT_WIDTH = 180;
export const GIT_HISTORY_REFS_MIN_WIDTH = 122;
export const GIT_HISTORY_REFS_MAX_WIDTH = 480;

/** 详情列维持原宽度，分支栏加宽时给提交列表保留至少 260px。 */
export function getGitHistoryRefsMaxWidth(panelWidth: number): number {
  // 窄屏使用标签页，不应因分栏隐藏而把分支栏宽度压到最小。
  if (panelWidth < 680) return GIT_HISTORY_REFS_MAX_WIDTH;
  const detailWidth = Math.max(180, Math.min(panelWidth * 0.29, 390));
  return Math.min(GIT_HISTORY_REFS_MAX_WIDTH, Math.floor(panelWidth - detailWidth - 8 - 260));
}

export function getGitHistoryRefsDefaultWidth(panelWidth: number): number {
  return panelWidth < 680
    ? GIT_HISTORY_REFS_DEFAULT_WIDTH
    : clampPanelWidth(panelWidth * 0.19, GIT_HISTORY_REFS_MIN_WIDTH, 235);
}

/**
 * Chat minimap track width.
 *
 * minimap 现在是对话区域右边缘的一层浮层（不占布局宽度、没有自己的背景
 * 与边框），所以这个值只管「轨道多宽」，不再参与 flex 布局。窄窗口下它会
 * 盖住正文行末最多这么宽，因此比原来的一整列（36）收窄一档。
 */
export const CHAT_MINIMAP_WIDTH = 28;

/**
 * Height of the chat top bar.
 *
 * 42 而不是 36：这条东西在桌面壳里就是窗口的标题栏（macOS 覆盖式红绿灯就浮在
 * 它上面），它需要足够的垂直空间去容纳 26px 的工具按键与拖拽区。
 * 手机上它依旧是一条普通工具条。
 */
export const CHAT_TOP_BAR_HEIGHT = 42;

const COMPACT_CHAT_MIN_WIDTH = 320;
const DESKTOP_CHAT_MIN_WIDTH = 420;

export function clampPanelWidth(width: number, minWidth: number, maxWidth: number): number {
  const finiteWidth = Number.isFinite(width) ? width : minWidth;
  const effectiveMax = Math.max(minWidth, maxWidth);
  return Math.round(Math.max(minWidth, Math.min(effectiveMax, finiteWidth)));
}

export function getDefaultRightPanelWidth(viewportWidth: number): number {
  return clampPanelWidth(viewportWidth * 0.42, 360, 640);
}

export function getSidebarMaxWidth(options: {
  viewportWidth: number;
  rightPanelOpen: boolean;
  rightPanelWidth: number;
}): number {
  const { viewportWidth, rightPanelOpen, rightPanelWidth } = options;
  if (viewportWidth <= MOBILE_MAX_WIDTH) return SIDEBAR_MAX_WIDTH;

  const compact = viewportWidth < SPLIT_PANEL_MIN_WIDTH;
  const chatWidth = compact ? COMPACT_CHAT_MIN_WIDTH : DESKTOP_CHAT_MIN_WIDTH;
  const visibleRightPanelWidth = !compact && rightPanelOpen ? rightPanelWidth : 0;
  return Math.min(SIDEBAR_MAX_WIDTH, viewportWidth - chatWidth - visibleRightPanelWidth);
}

export function getRightPanelMaxWidth(options: {
  viewportWidth: number;
  sidebarOpen: boolean;
  sidebarWidth: number;
}): number {
  const { viewportWidth, sidebarOpen, sidebarWidth } = options;
  if (viewportWidth < SPLIT_PANEL_MIN_WIDTH) return RIGHT_PANEL_MAX_WIDTH;

  const visibleSidebarWidth = sidebarOpen ? sidebarWidth : 0;
  return Math.min(
    RIGHT_PANEL_MAX_WIDTH,
    viewportWidth - DESKTOP_CHAT_MIN_WIDTH - visibleSidebarWidth,
  );
}
