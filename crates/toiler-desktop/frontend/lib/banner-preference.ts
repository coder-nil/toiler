const STORAGE_KEY = "pi-banner-enabled";

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

export function getBannerEnabled(
  storage: StorageLike | null = getBrowserStorage(),
): boolean {
  if (!storage) return true;
  try {
    return storage.getItem(STORAGE_KEY) !== "false";
  } catch {
    return true;
  }
}

export function setBannerEnabled(
  enabled: boolean,
  storage: StorageLike | null = getBrowserStorage(),
): void {
  if (!storage) return;
  try {
    storage.setItem(STORAGE_KEY, String(enabled));
  } catch {
    // Browser storage is best-effort.
  }
}
