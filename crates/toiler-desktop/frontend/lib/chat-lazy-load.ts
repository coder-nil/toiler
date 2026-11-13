/**
 * Visible render units per screen, not messages. One unit is a whole completed
 * turn (prompt → process group → answer), so 25 units is roughly the 50 flat
 * items this page size used to mean before turns were paged as one unit.
 */
export const VISIBLE_PAGE_SIZE = 25;
export const CHAT_SCROLL_TAIL_TOLERANCE = 8;
export const CHAT_SCROLL_REATTACH_TOLERANCE = 96;
/**
 * 「回到底部」按钮的显示阈值。比贴底容忍度宽得多：打开会话时
 * 提示词锚点常让视图停在离底几十像素处，按 8px 判会把这种
 * “本来就看得到底部”的情况也算成离开底部。
 */
export const CHAT_SCROLL_JUMP_TOLERANCE = 96;
/**
 * 「回到最新」的收回阈值。只用一个阈值会在边界处抖：内容长高一像素、
 * 平滑滚动还差一像素，都会让按钮一下一下地闪（流式输出时特别明显）。
 * 进场用宽阈值（96px），出场用紧阈值（24px），中间这段就是滞回区。
 */
export const CHAT_SCROLL_JUMP_HIDE_TOLERANCE = 24;

export function getVisibleRenderWindow(totalCount: number, visibleCount: number): {
  startIndex: number;
  hasMore: boolean;
} {
  const clampedVisibleCount = Math.min(Math.max(visibleCount, 0), Math.max(totalCount, 0));
  const startIndex = Math.max(0, totalCount - clampedVisibleCount);
  return { startIndex, hasMore: startIndex > 0 };
}

export function getNextVisibleCount(currentVisibleCount: number, pageSize = VISIBLE_PAGE_SIZE): number {
  return currentVisibleCount + pageSize;
}

export function captureScrollDistance(scrollHeight: number, scrollTop: number): number {
  return scrollHeight - scrollTop;
}

export function restoreScrollTop(scrollHeight: number, savedDistance: number): number {
  return Math.max(0, scrollHeight - savedDistance);
}

export function isScrollAtTail(
  scrollTop: number,
  clientHeight: number,
  scrollHeight: number,
  tolerance = CHAT_SCROLL_TAIL_TOLERANCE,
): boolean {
  return scrollTop + clientHeight >= scrollHeight - tolerance;
}

export function getLiveFollowAttached(
  wasAttached: boolean,
  previousScrollTop: number,
  scrollTop: number,
  clientHeight: number,
  scrollHeight: number,
  reattachTolerance = CHAT_SCROLL_REATTACH_TOLERANCE,
): boolean {
  if (isScrollAtTail(scrollTop, clientHeight, scrollHeight)) return true;
  if (scrollTop < previousScrollTop) return false;
  if (
    !wasAttached
    && scrollTop > previousScrollTop
    && isScrollAtTail(scrollTop, clientHeight, scrollHeight, reattachTolerance)
  ) return true;
  return wasAttached;
}

/**
 * 读者是否已经离开底部足够远，值得出现「回到底部」。
 * 只做一次比较：贴底容忍度（8px）太紧，提示词锚点会让刚打开的会话就停在离底
 * 几十像素处，那种情况本来就看得见底部，不该弹按钮。
 */
export function shouldShowJumpToLatest(
  scrollTop: number,
  clientHeight: number,
  scrollHeight: number,
  tolerance = CHAT_SCROLL_JUMP_TOLERANCE,
): boolean {
  return !isScrollAtTail(scrollTop, clientHeight, scrollHeight, tolerance);
}

/**
 * 带滞回的可见性：已经可见时要贴得更近才收回去。
 * 不可见 → 离开 96px 才弹出；可见 → 回到 24px 以内才收回。
 * 两个阈值之间的窄带里保持原状，这就是滞回（和手势识别里的方向阈值同一个作用）。
 */
export function resolveJumpToLatestVisible(
  wasVisible: boolean,
  scrollTop: number,
  clientHeight: number,
  scrollHeight: number,
): boolean {
  return wasVisible
    ? !isScrollAtTail(scrollTop, clientHeight, scrollHeight, CHAT_SCROLL_JUMP_HIDE_TOLERANCE)
    : shouldShowJumpToLatest(scrollTop, clientHeight, scrollHeight);
}

export function getPromptAnchorSpacerHeight(
  targetTop: number,
  contentEnd: number,
  clientHeight: number,
): number {
  const clampedTargetTop = Math.max(0, targetTop);
  if (clampedTargetTop === 0) return 0;

  return Math.max(0, Math.ceil(
    clampedTargetTop + clientHeight - Math.max(0, contentEnd),
  ));
}
