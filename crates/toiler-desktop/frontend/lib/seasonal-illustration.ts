export const SEASONAL_IMAGES = {
  spring: "/illustrations/seasons/spring.webp",
  summer: "/illustrations/seasons/summer.webp",
  autumn: "/illustrations/seasons/autumn.webp",
  winter: "/illustrations/seasons/winter.webp",
} as const;

export type Season = keyof typeof SEASONAL_IMAGES;

export interface SeasonSnapshot {
  season: Season;
  serverTime: number;
  nextChangeAt: number;
}

export function parseSeasonSnapshot(value: unknown): SeasonSnapshot | null {
  if (!value || typeof value !== "object") return null;
  const data = value as Record<string, unknown>;
  if (typeof data.season !== "string" || !Object.hasOwn(SEASONAL_IMAGES, data.season)) return null;
  if (typeof data.serverTime !== "number" || !Number.isSafeInteger(data.serverTime)) return null;
  if (typeof data.nextChangeAt !== "number" || !Number.isSafeInteger(data.nextChangeAt)) return null;
  if (data.serverTime < 0 || data.nextChangeAt <= data.serverTime) return null;
  return data as unknown as SeasonSnapshot;
}

/** 以服务器时间差定时，避免手机时区或时钟与桌面不同而错过交节。 */
export function seasonRefreshDelay(snapshot: SeasonSnapshot, requestElapsed: number): number {
  return Math.max(250, Math.min(60_000, snapshot.nextChangeAt - snapshot.serverTime - requestElapsed));
}
