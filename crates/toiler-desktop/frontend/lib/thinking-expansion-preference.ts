// "Thinking is expanded by default" is a browser preference, not session
// content: the transcript keeps storing collapsed blocks, only the initial
// render state of a thinking block changes.
//
// Already-mounted blocks listen for the broadcast event so flipping the switch
// in settings updates the open conversation too.

const STORAGE_KEY = "pi-thinking-expanded";

export const THINKING_EXPANDED_EVENT = "pi-thinking-expanded-changed";

interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

function getBrowserStorage(): StorageLike | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function isThinkingExpandedByDefault(
  storage: StorageLike | null = getBrowserStorage(),
): boolean {
  if (!storage) return false;
  try {
    return storage.getItem(STORAGE_KEY) === "true";
  } catch {
    return false;
  }
}

export function setThinkingExpandedByDefault(
  expanded: boolean,
  storage: StorageLike | null = getBrowserStorage(),
  dispatch: (() => void) | null = typeof window === "undefined"
    ? null
    : () => window.dispatchEvent(new Event(THINKING_EXPANDED_EVENT)),
): void {
  if (!storage) return;
  try {
    storage.setItem(STORAGE_KEY, String(expanded));
  } catch {
    // Browser storage is best-effort.
  }
  dispatch?.();
}
