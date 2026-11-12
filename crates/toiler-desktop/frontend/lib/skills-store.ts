import { mkdirSync } from "fs";
import { DatabaseSync } from "node:sqlite";
import { dirname, join } from "path";
import { getAgentDir } from "@earendil-works/pi-coding-agent";
import type { SkillInfo, SkillsResponse } from "@/lib/api-types";

interface SkillUsageRow {
  skill_path: string;
  use_count: number;
  last_used_at: string;
}

interface SkillCacheRow {
  response_json: string;
}

export interface SkillInvocation {
  name: string;
  filePath: string;
}

export function getPiDatabasePath(agentDir = getAgentDir()): string {
  return join(agentDir, "pi.sqlite");
}

function openDatabase(databasePath = getPiDatabasePath()): DatabaseSync {
  mkdirSync(dirname(databasePath), { recursive: true });
  const database = new DatabaseSync(databasePath);
  database.exec(`
    PRAGMA journal_mode = WAL;
    PRAGMA busy_timeout = 3000;
    CREATE TABLE IF NOT EXISTS app_metadata (
      key TEXT PRIMARY KEY,
      value TEXT NOT NULL,
      updated_at TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS skill_cache (
      cwd TEXT PRIMARY KEY,
      response_json TEXT NOT NULL,
      refreshed_at TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS skill_usage (
      skill_path TEXT PRIMARY KEY,
      skill_name TEXT NOT NULL,
      use_count INTEGER NOT NULL DEFAULT 0 CHECK (use_count >= 0),
      last_used_at TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS skill_usage_rank_idx
      ON skill_usage(use_count DESC, last_used_at DESC);
  `);
  const now = new Date().toISOString();
  database.prepare(`
    INSERT INTO app_metadata(key, value, updated_at) VALUES ('schema_version', '1', ?)
    ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at
  `).run(now);
  return database;
}

export function getCachedSkills(cwd: string, databasePath?: string): SkillsResponse | null {
  const database = openDatabase(databasePath);
  try {
    const row = database.prepare(
      "SELECT response_json FROM skill_cache WHERE cwd = ?",
    ).get(cwd) as SkillCacheRow | undefined;
    if (!row) return null;
    const response = JSON.parse(row.response_json) as Partial<SkillsResponse>;
    if (!Array.isArray(response.skills)) return null;
    return {
      skills: response.skills,
      diagnostics: response.diagnostics ?? [],
      projectResourcesLoaded: response.projectResourcesLoaded ?? true,
    };
  } catch {
    return null;
  } finally {
    database.close();
  }
}

export function replaceCachedSkills(
  cwd: string,
  response: SkillsResponse,
  databasePath?: string,
): void {
  const database = openDatabase(databasePath);
  try {
    database.prepare(`
      INSERT INTO skill_cache(cwd, response_json, refreshed_at) VALUES (?, ?, ?)
      ON CONFLICT(cwd) DO UPDATE SET
        response_json = excluded.response_json,
        refreshed_at = excluded.refreshed_at
    `).run(cwd, JSON.stringify(response), new Date().toISOString());
  } finally {
    database.close();
  }
}

export function clearSkillsCache(databasePath?: string): void {
  const database = openDatabase(databasePath);
  try {
    database.exec("DELETE FROM skill_cache");
  } finally {
    database.close();
  }
}

export function recordSkillUsage(
  invocation: SkillInvocation,
  databasePath?: string,
): void {
  const database = openDatabase(databasePath);
  try {
    database.prepare(`
      INSERT INTO skill_usage(skill_path, skill_name, use_count, last_used_at)
      VALUES (?, ?, 1, ?)
      ON CONFLICT(skill_path) DO UPDATE SET
        skill_name = excluded.skill_name,
        use_count = skill_usage.use_count + 1,
        last_used_at = excluded.last_used_at
    `).run(invocation.filePath, invocation.name, new Date().toISOString());
  } finally {
    database.close();
  }
}

export function enrichSkillsWithUsage(
  response: SkillsResponse,
  databasePath?: string,
): SkillsResponse {
  if (response.skills.length === 0) return response;
  const database = openDatabase(databasePath);
  try {
    const rows = database.prepare(
      "SELECT skill_path, use_count, last_used_at FROM skill_usage",
    ).all() as unknown as SkillUsageRow[];
    const usage = new Map(rows.map((row) => [row.skill_path, row]));
    const skills = response.skills.map((skill) => {
      const record = usage.get(skill.filePath);
      return record
        ? { ...skill, usageCount: record.use_count, lastUsedAt: record.last_used_at }
        : skill;
    }).sort((left, right) =>
      (right.usageCount ?? 0) - (left.usageCount ?? 0)
      || (right.lastUsedAt ?? "").localeCompare(left.lastUsedAt ?? "")
      || left.name.localeCompare(right.name),
    );
    return { ...response, skills };
  } finally {
    database.close();
  }
}

export function findSkillInvocation(
  text: string,
  skills: ReadonlyArray<Pick<SkillInfo, "name" | "filePath">>,
): SkillInvocation | null {
  const match = /^\/skill:([^\s]+)/.exec(text);
  if (!match) return null;
  const skill = skills.find((item) => item.name === match[1]);
  return skill ? { name: skill.name, filePath: skill.filePath } : null;
}
