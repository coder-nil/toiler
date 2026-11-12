import { mkdirSync } from "fs";
import { DatabaseSync } from "node:sqlite";
import { dirname } from "path";
import { getPiDatabasePath } from "./skills-store";
import type { ModelCatalogEntry } from "./model-catalog";

export interface StoredModelsDevCatalog {
  entries: ModelCatalogEntry[];
  /** Unix 毫秒时间戳，对应最后一次成功写入缓存的时刻。 */
  fetchedAt: number;
}

interface CatalogRow {
  payload: string;
  fetched_at: string;
}

function openDatabase(databasePath = getPiDatabasePath()): DatabaseSync {
  mkdirSync(dirname(databasePath), { recursive: true });
  const database = new DatabaseSync(databasePath);
  database.exec(`
    PRAGMA journal_mode = WAL;
    PRAGMA busy_timeout = 3000;
    CREATE TABLE IF NOT EXISTS models_dev_catalog (
      id INTEGER PRIMARY KEY CHECK (id = 1),
      payload TEXT NOT NULL,
      fetched_at TEXT NOT NULL
    );
  `);
  return database;
}

/**
 * 读取上次缓存的 models.dev 目录。
 *
 * 缓存只是加速手段：数据库缺失、损坏或内容不合法时都返回 null，
 * 让调用方回退到网络请求，绝不能因此中断目录加载。
 */
export function readModelsDevCatalog(databasePath?: string): StoredModelsDevCatalog | null {
  try {
    const database = openDatabase(databasePath);
    try {
      const row = database.prepare(
        "SELECT payload, fetched_at FROM models_dev_catalog WHERE id = 1",
      ).get() as CatalogRow | undefined;
      if (!row) return null;

      const parsed = JSON.parse(row.payload) as unknown;
      if (!Array.isArray(parsed) || parsed.length === 0) return null;

      const fetchedAt = Date.parse(row.fetched_at);
      if (!Number.isFinite(fetchedAt)) return null;

      return { entries: parsed as ModelCatalogEntry[], fetchedAt };
    } finally {
      database.close();
    }
  } catch {
    return null;
  }
}

/** 覆盖缓存的 models.dev 目录。空目录不写，避免用失败结果顶掉可用缓存。 */
export function writeModelsDevCatalog(
  entries: readonly ModelCatalogEntry[],
  databasePath?: string,
): void {
  if (entries.length === 0) return;
  try {
    const database = openDatabase(databasePath);
    try {
      database.prepare(`
        INSERT INTO models_dev_catalog(id, payload, fetched_at) VALUES (1, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
          payload = excluded.payload,
          fetched_at = excluded.fetched_at
      `).run(JSON.stringify(entries), new Date().toISOString());
    } finally {
      database.close();
    }
  } catch {
    // 写缓存失败不影响本次返回的目录。
  }
}

export function clearModelsDevCatalog(databasePath?: string): void {
  const database = openDatabase(databasePath);
  try {
    database.exec("DELETE FROM models_dev_catalog");
  } finally {
    database.close();
  }
}
