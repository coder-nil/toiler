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
