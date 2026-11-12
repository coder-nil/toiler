import { mkdirSync } from "fs";
import { DatabaseSync } from "node:sqlite";
import { dirname } from "path";
import { getPiDatabasePath } from "./skills-store";

/**
 * 插件目录使用量（npm 下载量）缓存。
 *
 * 官网 registry 只会在联网搜索时返回 `downloads`，所以把每次成功搜索里
 * 拿到的下载量持久化下来，让无搜索的默认列表也能按使用量倒序展示。
 */
export interface StoredPluginDownloads {
  /** 插件名（npm package name）→ 最近 30 天下载量。 */
  monthly: Record<string, number>;
  /** 插件名 → 最近 7 天下载量。 */
  weekly: Record<string, number>;
  /** Unix 毫秒时间戳，最后一次成功写入缓存的时刻。 */
  fetchedAt: number;
}

function openDatabase(databasePath = getPiDatabasePath()): DatabaseSync {
  mkdirSync(dirname(databasePath), { recursive: true });
  const database = new DatabaseSync(databasePath);
  database.exec(`
    PRAGMA journal_mode = WAL;
    PRAGMA busy_timeout = 3000;
    CREATE TABLE IF NOT EXISTS plugin_downloads (
      id INTEGER PRIMARY KEY CHECK (id = 1),
      monthly TEXT NOT NULL,
      weekly TEXT NOT NULL,
      fetched_at TEXT NOT NULL
    );
  `);
  return database;
}

function parseCounts(payload: string): Record<string, number> {
  try {
    const parsed = JSON.parse(payload) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const counts: Record<string, number> = {};
    for (const [name, value] of Object.entries(parsed as Record<string, unknown>)) {
      const count = typeof value === "number" ? value : Number.parseInt(String(value ?? ""), 10);
      if (name && Number.isFinite(count) && count >= 0) counts[name] = count;
    }
    return counts;
  } catch {
    return {};
  }
}

/** 读取缓存的下载量。缓存缺失或损坏时返回 null，调用方继续走原有逻辑。 */
export function readPluginDownloads(databasePath?: string): StoredPluginDownloads | null {
  try {
    const database = openDatabase(databasePath);
    try {
      const row = database.prepare(
        "SELECT monthly, weekly, fetched_at FROM plugin_downloads WHERE id = 1",
      ).get() as { monthly: string; weekly: string; fetched_at: string } | undefined;
      if (!row) return null;

      const monthly = parseCounts(row.monthly);
      const fetchedAt = Date.parse(row.fetched_at);
      if (Object.keys(monthly).length === 0 || !Number.isFinite(fetchedAt)) return null;

      return { monthly, weekly: parseCounts(row.weekly), fetchedAt };
    } finally {
      database.close();
    }
  } catch {
    return null;
  }
}

/** 合并写入下载量；空数据不写，避免用失败结果顶掉可用缓存。 */
export function writePluginDownloads(
  counts: { monthly: Record<string, number>; weekly?: Record<string, number> },
  databasePath?: string,
): void {
  if (Object.keys(counts.monthly).length === 0) return;
  const existing = readPluginDownloads(databasePath);
  const monthly = { ...existing?.monthly, ...counts.monthly };
  const weekly = { ...existing?.weekly, ...counts.weekly };
  try {
    const database = openDatabase(databasePath);
    try {
      database.prepare(`
        INSERT INTO plugin_downloads(id, monthly, weekly, fetched_at) VALUES (1, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
          monthly = excluded.monthly,
          weekly = excluded.weekly,
          fetched_at = excluded.fetched_at
      `).run(JSON.stringify(monthly), JSON.stringify(weekly), new Date().toISOString());
    } finally {
      database.close();
    }
  } catch {
    // 写缓存失败不影响本次返回的目录。
  }
}

export function clearPluginDownloads(databasePath?: string): void {
  const database = openDatabase(databasePath);
  try {
    database.exec("DELETE FROM plugin_downloads");
  } finally {
    database.close();
  }
}
