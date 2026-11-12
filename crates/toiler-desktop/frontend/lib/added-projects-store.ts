import { mkdirSync } from "fs";
import { DatabaseSync } from "node:sqlite";
import { dirname } from "path";
import { getPiDatabasePath } from "./skills-store";

export interface AddedProject {
  projectKey: string;
  projectRoot: string;
  cwd: string;
  addedAt: string;
  /** Starred projects are listed first; the flag lives on the project record. */
  starred: boolean;
}

export interface SelectedProject {
  projectKey: string;
  projectRoot: string;
}

interface AddedProjectRow {
  project_key: string;
  project_root: string;
  cwd: string;
  added_at: string;
  starred: number | null;
}

function openDatabase(databasePath = getPiDatabasePath()): DatabaseSync {
  mkdirSync(dirname(databasePath), { recursive: true });
  const database = new DatabaseSync(databasePath);
  database.exec(`
    PRAGMA journal_mode = WAL;
    PRAGMA busy_timeout = 3000;
    CREATE TABLE IF NOT EXISTS added_projects (
      project_key TEXT PRIMARY KEY,
      project_root TEXT NOT NULL,
      cwd TEXT NOT NULL,
      added_at TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS added_projects_added_at_idx
      ON added_projects(added_at DESC);
    CREATE TABLE IF NOT EXISTS app_preferences (
      key TEXT PRIMARY KEY,
      value TEXT NOT NULL,
      updated_at TEXT NOT NULL
    );
  `);
  ensureStarredColumn(database);
  return database;
}

/**
 * Records written before starring existed have no `starred` column, and SQLite
 * has no `ADD COLUMN IF NOT EXISTS` — so the migration is a table_info probe.
 */
function ensureStarredColumn(database: DatabaseSync): void {
  const columns = database.prepare("PRAGMA table_info(added_projects)").all() as unknown as Array<{ name?: unknown }>;
  if (columns.some((column) => column.name === "starred")) return;
  database.exec("ALTER TABLE added_projects ADD COLUMN starred INTEGER NOT NULL DEFAULT 0");
}

export function listAddedProjects(databasePath?: string): AddedProject[] {
  const database = openDatabase(databasePath);
  try {
    const rows = database.prepare(`
      SELECT project_key, project_root, cwd, added_at, starred
      FROM added_projects
      ORDER BY starred DESC, added_at DESC
    `).all() as unknown as AddedProjectRow[];
    return rows.map(rowToAddedProject);
  } finally {
    database.close();
  }
}

function rowToAddedProject(row: AddedProjectRow): AddedProject {
  return {
    projectKey: row.project_key,
    projectRoot: row.project_root,
    cwd: row.cwd,
    addedAt: row.added_at,
    starred: Number(row.starred ?? 0) === 1,
  };
}

export function saveAddedProject(
  project: Omit<AddedProject, "addedAt" | "starred">,
  databasePath?: string,
): AddedProject {
  const addedAt = new Date().toISOString();
  const database = openDatabase(databasePath);
  try {
    // Re-adding an existing project refreshes its position but keeps the star:
    // the flag is a user preference for that directory, not part of the record
    // being written.
    database.prepare(`
      INSERT INTO added_projects(project_key, project_root, cwd, added_at, starred)
      VALUES (?, ?, ?, ?, 0)
      ON CONFLICT(project_key) DO UPDATE SET
        project_root = excluded.project_root,
        cwd = excluded.cwd,
        added_at = excluded.added_at
    `).run(project.projectKey, project.projectRoot, project.cwd, addedAt);
    const row = database.prepare(`
      SELECT project_key, project_root, cwd, added_at, starred
      FROM added_projects WHERE project_key = ?
    `).get(project.projectKey) as unknown as AddedProjectRow | undefined;
    return row ? rowToAddedProject(row) : { ...project, addedAt, starred: false };
  } finally {
    database.close();
  }
}

/** Stars or unstars a recorded project. Returns false when the record is gone. */
export function setAddedProjectStarred(
  projectKey: string,
  starred: boolean,
  databasePath?: string,
): boolean {
  const database = openDatabase(databasePath);
  try {
    const result = database
      .prepare("UPDATE added_projects SET starred = ? WHERE project_key = ?")
      .run(starred ? 1 : 0, projectKey);
    return result.changes > 0;
  } finally {
    database.close();
  }
}

/** Removes only the app's directory-list record; it never touches the directory or sessions. */
export function removeAddedProject(projectKey: string, databasePath?: string): boolean {
  const database = openDatabase(databasePath);
  try {
    const result = database.prepare("DELETE FROM added_projects WHERE project_key = ?").run(projectKey);
    return result.changes > 0;
  } finally {
    database.close();
  }
}

const SELECTED_PROJECT_PREFERENCE_KEY = "last_selected_project";

export function getSelectedProject(databasePath?: string): SelectedProject | null {
  const database = openDatabase(databasePath);
  try {
    const row = database.prepare(
      "SELECT value FROM app_preferences WHERE key = ?",
    ).get(SELECTED_PROJECT_PREFERENCE_KEY) as { value?: unknown } | undefined;
    if (typeof row?.value !== "string") return null;
    const parsed: unknown = JSON.parse(row.value);
    if (
      parsed === null
      || typeof parsed !== "object"
      || Array.isArray(parsed)
      || !("projectKey" in parsed)
      || !("projectRoot" in parsed)
      || typeof parsed.projectKey !== "string"
      || typeof parsed.projectRoot !== "string"
      || !parsed.projectKey
      || !parsed.projectRoot
    ) {
      return null;
    }
    return { projectKey: parsed.projectKey, projectRoot: parsed.projectRoot };
  } catch {
    return null;
  } finally {
    database.close();
  }
}

export function saveSelectedProject(project: SelectedProject, databasePath?: string): void {
  const database = openDatabase(databasePath);
  try {
    database.prepare(`
      INSERT INTO app_preferences(key, value, updated_at)
      VALUES (?, ?, ?)
      ON CONFLICT(key) DO UPDATE SET
        value = excluded.value,
        updated_at = excluded.updated_at
    `).run(SELECTED_PROJECT_PREFERENCE_KEY, JSON.stringify(project), new Date().toISOString());
  } finally {
    database.close();
  }
}
