const STORAGE_KEY = "pi-desktop:project-history";
const HIDDEN_STORAGE_KEY = "pi-desktop:hidden-project-history";
const MAX_PROJECT_HISTORY = 12;

interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function getBrowserStorage(): StorageLike | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function normalizePaths(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return [...new Set(value.filter((path): path is string => typeof path === "string" && path.trim().length > 0))]
    .slice(0, MAX_PROJECT_HISTORY);
}

export function getProjectHistory(storage: StorageLike | null = getBrowserStorage()): string[] {
  if (!storage) return [];
  try {
    const raw = storage.getItem(STORAGE_KEY);
    return raw ? normalizePaths(JSON.parse(raw)) : [];
  } catch {
    return [];
  }
}

export function addProjectHistory(path: string, storage: StorageLike | null = getBrowserStorage()): string[] {
  const normalized = path.trim();
  if (!normalized) return getProjectHistory(storage);
  const next = [normalized, ...getProjectHistory(storage).filter((entry) => entry !== normalized)]
    .slice(0, MAX_PROJECT_HISTORY);
  if (!storage) return next;
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(next));
    const hidden = getHiddenProjectHistory(storage).filter((entry) => entry !== normalized);
    if (hidden.length === 0) storage.removeItem(HIDDEN_STORAGE_KEY);
    else storage.setItem(HIDDEN_STORAGE_KEY, JSON.stringify(hidden));
  } catch {
    // History is a browser convenience and must not block project selection.
  }
  return next;
}

export function getHiddenProjectHistory(storage: StorageLike | null = getBrowserStorage()): string[] {
  if (!storage) return [];
  try {
    const raw = storage.getItem(HIDDEN_STORAGE_KEY);
    return raw ? normalizePaths(JSON.parse(raw)) : [];
  } catch {
    return [];
  }
}

/** Hide a project from the selector without deleting its files or sessions. */
export function hideProjectHistory(path: string, storage: StorageLike | null = getBrowserStorage()): string[] {
  const normalized = path.trim();
  if (!normalized) return getHiddenProjectHistory(storage);
  const next = [normalized, ...getHiddenProjectHistory(storage).filter((entry) => entry !== normalized)]
    .slice(0, MAX_PROJECT_HISTORY);
  if (!storage) return next;
  try {
    storage.setItem(HIDDEN_STORAGE_KEY, JSON.stringify(next));
  } catch {
    // History is a browser convenience and must not block project selection.
  }
  return next;
}


export function removeProjectHistory(path: string, storage: StorageLike | null = getBrowserStorage()): string[] {
  const next = getProjectHistory(storage).filter((entry) => entry !== path);
  if (!storage) return next;
  try {
    if (next.length === 0) storage.removeItem(STORAGE_KEY);
    else storage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    // Ignore unavailable or full browser storage.
  }
  return next;
}
