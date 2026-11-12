// Chat reading surface preferences (content width + type size).
//
// The two values are browser preferences, not session content: they live in
// localStorage and are published as CSS custom properties, so every consumer
// (message list, composer, settings sliders) reads one source of truth without
// a React context. Kept free of DOM globals so the clamping and the restore
// path stay unit-testable.

export const CHAT_CONTENT_WIDTH_DEFAULT = 820;
export const CHAT_CONTENT_WIDTH_MIN = 820;
export const CHAT_CONTENT_WIDTH_MAX = 2000;
export const CHAT_CONTENT_WIDTH_STORAGE_KEY = "pi-chat-content-width";

export const CHAT_CONTENT_FONT_SIZE_DEFAULT = 14;
export const CHAT_CONTENT_FONT_SIZE_MIN = 12;
export const CHAT_CONTENT_FONT_SIZE_MAX = 24;
export const CHAT_CONTENT_FONT_SIZE_STORAGE_KEY = "pi-chat-content-font-size";

export const CHAT_CONTENT_MAX_WIDTH_VARIABLE = "--chat-content-max-width";
export const CHAT_CONTENT_FONT_SIZE_VARIABLE = "--chat-content-font-size";

export interface ChatAppearance {
  width: number;
  fontSize: number;
}

export const DEFAULT_CHAT_APPEARANCE: ChatAppearance = {
  width: CHAT_CONTENT_WIDTH_DEFAULT,
  fontSize: CHAT_CONTENT_FONT_SIZE_DEFAULT,
};

export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export interface StyleTargetLike {
  style: { setProperty(name: string, value: string): void };
}

export function clampChatContentWidth(value: unknown): number {
  if (value === null || value === undefined || value === "") return CHAT_CONTENT_WIDTH_DEFAULT;
  const width = Number(value);
  if (!Number.isFinite(width)) return CHAT_CONTENT_WIDTH_DEFAULT;
  return Math.max(CHAT_CONTENT_WIDTH_MIN, Math.min(CHAT_CONTENT_WIDTH_MAX, Math.round(width)));
}

export function clampChatContentFontSize(value: unknown): number {
  // 未存储/空值必须落回默认值，而不是 Number(null) → 0 再被夹成最小值，
  // 否则首次打开就会拿到最小的字号。
  if (value === null || value === undefined || value === "") return CHAT_CONTENT_FONT_SIZE_DEFAULT;
  const size = Number(value);
  if (!Number.isFinite(size)) return CHAT_CONTENT_FONT_SIZE_DEFAULT;
  return Math.max(CHAT_CONTENT_FONT_SIZE_MIN, Math.min(CHAT_CONTENT_FONT_SIZE_MAX, Math.round(size)));
}

function getBrowserStorage(): StorageLike | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function readStoredNumber(storage: StorageLike, key: string): string | null {
  try {
    return storage.getItem(key);
  } catch {
    return null;
  }
}

/** Read the stored appearance, falling back to the defaults per value. */
export function readChatAppearance(storage: StorageLike | null = getBrowserStorage()): ChatAppearance {
  if (!storage) return { ...DEFAULT_CHAT_APPEARANCE };
  return {
    width: clampChatContentWidth(readStoredNumber(storage, CHAT_CONTENT_WIDTH_STORAGE_KEY)),
    fontSize: clampChatContentFontSize(readStoredNumber(storage, CHAT_CONTENT_FONT_SIZE_STORAGE_KEY)),
  };
}

/** Persist one value and return the resulting appearance. */
export function writeChatAppearance(
  current: ChatAppearance,
  key: keyof ChatAppearance,
  value: unknown,
  storage: StorageLike | null = getBrowserStorage(),
): ChatAppearance {
  const next: ChatAppearance = key === "width"
    ? { ...current, width: clampChatContentWidth(value) }
    : { ...current, fontSize: clampChatContentFontSize(value) };
  if (storage) {
    try {
      storage.setItem(
        key === "width" ? CHAT_CONTENT_WIDTH_STORAGE_KEY : CHAT_CONTENT_FONT_SIZE_STORAGE_KEY,
        String(next[key]),
      );
    } catch {
      // Browser storage is best-effort.
    }
  }
  return next;
}

/**
 * First-paint restore of the two custom properties.
 *
 * Runs before React hydrates (same reason the theme script does), so the stored
 * type size does not visibly jump once the chat mounts. Values are clamped
 * inline because the script cannot import the helpers above.
 */
export const CHAT_APPEARANCE_INIT_SCRIPT = `(function(){try{var r=document.documentElement;var clamp=function(value,min,max,fallback){if(value===null||value==="")return fallback;var n=Number(value);if(!isFinite(n))return fallback;n=Math.round(n);return n<min?min:(n>max?max:n)};var w=localStorage.getItem(${JSON.stringify(CHAT_CONTENT_WIDTH_STORAGE_KEY)});if(w!==null)r.style.setProperty(${JSON.stringify(CHAT_CONTENT_MAX_WIDTH_VARIABLE)},clamp(w,${CHAT_CONTENT_WIDTH_MIN},${CHAT_CONTENT_WIDTH_MAX},${CHAT_CONTENT_WIDTH_DEFAULT})+"px");var f=localStorage.getItem(${JSON.stringify(CHAT_CONTENT_FONT_SIZE_STORAGE_KEY)});if(f!==null)r.style.setProperty(${JSON.stringify(CHAT_CONTENT_FONT_SIZE_VARIABLE)},clamp(f,${CHAT_CONTENT_FONT_SIZE_MIN},${CHAT_CONTENT_FONT_SIZE_MAX},${CHAT_CONTENT_FONT_SIZE_DEFAULT})+"px")}catch(e){}})();`;

/** Publish the appearance as CSS custom properties for the whole document. */
export function applyChatAppearance(
  appearance: ChatAppearance,
  target: StyleTargetLike | null = typeof document === "undefined" ? null : document.documentElement,
): void {
  if (!target) return;
  target.style.setProperty(CHAT_CONTENT_MAX_WIDTH_VARIABLE, `${appearance.width}px`);
  target.style.setProperty(CHAT_CONTENT_FONT_SIZE_VARIABLE, `${appearance.fontSize}px`);
}
