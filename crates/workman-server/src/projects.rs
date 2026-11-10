//! 项目列表与「上次选中的项目」——与 pi-desktop **共用同一个 SQLite 库**
//! （`<agentDir>/pi.sqlite`，表 `added_projects` / `app_preferences`）。
//!
//! 共用库的好处是 pi 与 workman 的项目列表、星标、选中项天然一致；坏处是
//! 要跟 pi 的 schema 完全对齐。表结构（模块 `58641`）：
//!
//! ```sql
//! CREATE TABLE added_projects (
//!   project_key TEXT PRIMARY KEY,
//!   project_root TEXT NOT NULL,
//!   cwd TEXT NOT NULL,
//!   added_at TEXT NOT NULL,
//!   starred INTEGER NOT NULL DEFAULT 0      -- 后加的列
//! );
//! CREATE TABLE app_preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);
//! ```
//!
//! `app_preferences` 里 `last_selected_project` 存 `{projectKey, projectRoot}`。
//!
//! 读写都开 WAL + `busy_timeout=3000`，与 pi 一致；每次操作打开一个连接，
//! 用完就关（量很小，连接池反而更复杂）。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::paths;

/// 一条项目记录（`58641.l`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub project_key: String,
    pub project_root: String,
    pub cwd: String,
    pub added_at: String,
    pub starred: bool,
}

impl Project {
    pub fn to_json(&self) -> Value {
        json!({
            "projectKey": self.project_key,
            "projectRoot": self.project_root,
            "cwd": self.cwd,
            "addedAt": self.added_at,
            "starred": self.starred,
        })
    }
}

/// 项目库。
#[derive(Debug, Clone)]
pub struct ProjectStore {
    db: PathBuf,
}

impl Default for ProjectStore {
    fn default() -> Self {
        Self::at(paths::agent_file("pi.sqlite"))
    }
}

impl ProjectStore {
    pub fn at(db: impl Into<PathBuf>) -> Self {
        Self { db: db.into() }
    }

    pub fn path(&self) -> &Path {
        &self.db
    }

    /// 打开连接并确保表存在（对应 pi 的 `j()`）。
    fn open(&self) -> Result<Connection> {
        if let Some(dir) = self.db.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(&self.db)
            .with_context(|| format!("打开 {} 失败", self.db.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
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
             );",
        )?;
        // 老库没有 starred 列 → 补上（pi 也是这么迁移的）
        let has_starred = {
            let mut stmt = conn.prepare("PRAGMA table_info(added_projects)")?;
            let names = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .filter_map(Result::ok)
                .collect::<Vec<_>>();
            names.iter().any(|n| n == "starred")
        };
        if !has_starred {
            conn.execute_batch(
                "ALTER TABLE added_projects ADD COLUMN starred INTEGER NOT NULL DEFAULT 0",
            )?;
        }
        Ok(conn)
    }

    /// 全部项目（星标优先，其次按加入时间倒序）。
    pub fn list(&self) -> Result<Vec<Project>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT project_key, project_root, cwd, added_at, starred
             FROM added_projects
             ORDER BY starred DESC, added_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Project {
                project_key: row.get(0)?,
                project_root: row.get(1)?,
                cwd: row.get(2)?,
                added_at: row.get(3)?,
                starred: row.get::<_, i64>(4)? == 1,
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// 加入 / 更新项目（父目录变化时以新值为准）。
    pub fn upsert(&self, project_key: &str, project_root: &str, cwd: &str) -> Result<Project> {
        let now = crate::workman_now_iso();
        let conn = self.open()?;
        conn.execute(
            "INSERT INTO added_projects(project_key, project_root, cwd, added_at, starred)
             VALUES (?1, ?2, ?3, ?4, 0)
             ON CONFLICT(project_key) DO UPDATE SET
               project_root = excluded.project_root,
               cwd = excluded.cwd,
               added_at = excluded.added_at",
            params![project_key, project_root, cwd, now],
        )?;
        let mut stmt = conn.prepare(
            "SELECT project_key, project_root, cwd, added_at, starred
             FROM added_projects WHERE project_key = ?1",
        )?;
        let project = stmt.query_row(params![project_key], |row| {
            Ok(Project {
                project_key: row.get(0)?,
                project_root: row.get(1)?,
                cwd: row.get(2)?,
                added_at: row.get(3)?,
                starred: row.get::<_, i64>(4)? == 1,
            })
        })?;
        Ok(project)
    }

    /// 改星标；返回是否命中一行。
    pub fn set_starred(&self, project_key: &str, starred: bool) -> Result<bool> {
        let conn = self.open()?;
        let changed = conn.execute(
            "UPDATE added_projects SET starred = ?1 WHERE project_key = ?2",
            params![i64::from(starred), project_key],
        )?;
        Ok(changed > 0)
    }

    /// 移除项目；返回是否命中一行。
    pub fn remove(&self, project_key: &str) -> Result<bool> {
        let conn = self.open()?;
        let changed = conn.execute(
            "DELETE FROM added_projects WHERE project_key = ?1",
            params![project_key],
        )?;
        Ok(changed > 0)
    }

    /// 上次选中的项目（`58641.NL`）。
    pub fn selected(&self) -> Result<Option<(String, String)>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare("SELECT value FROM app_preferences WHERE key = ?1")?;
        let raw: Option<String> = stmt
            .query_row(params!["last_selected_project"], |row| row.get(0))
            .ok();
        let Some(raw) = raw else {
            return Ok(None);
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            return Ok(None);
        };
        let key = value.get("projectKey").and_then(Value::as_str);
        let root = value.get("projectRoot").and_then(Value::as_str);
        match (key, root) {
            (Some(key), Some(root)) if !key.is_empty() && !root.is_empty() => {
                Ok(Some((key.to_string(), root.to_string())))
            }
            _ => Ok(None),
        }
    }

    /// 写选中项（`58641.Ey`）。
    pub fn set_selected(&self, project_key: &str, project_root: &str) -> Result<()> {
        let now = crate::workman_now_iso();
        let value = json!({ "projectKey": project_key, "projectRoot": project_root });
        let conn = self.open()?;
        conn.execute(
            "INSERT INTO app_preferences(key, value, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params!["last_selected_project", value.to_string(), now],
        )?;
        Ok(())
    }

    /// 选中项的 JSON 形式（`null` / `{projectKey, projectRoot}`）。
    pub fn selected_json(&self) -> Value {
        match self.selected() {
            Ok(Some((key, root))) => json!({ "projectKey": key, "projectRoot": root }),
            _ => Value::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> (PathBuf, ProjectStore) {
        let dir = std::env::temp_dir().join(format!(
            "workman-projects-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = ProjectStore::at(dir.join("pi.sqlite"));
        (dir, store)
    }

    #[test]
    fn upsert_star_and_remove() {
        let (dir, store) = store("crud");
        let p = store.upsert("/tmp/a", "/tmp/a", "/tmp/a/sub").unwrap();
        assert_eq!(p.project_key, "/tmp/a");
        assert!(!p.starred);
        assert_eq!(store.list().unwrap().len(), 1);

        assert!(store.set_starred("/tmp/a", true).unwrap());
        assert!(store.list().unwrap()[0].starred);
        assert!(!store.set_starred("/tmp/nope", true).unwrap());

        // 幂等 upsert：不会再插一条
        store.upsert("/tmp/a", "/tmp/a", "/tmp/a/sub").unwrap();
        assert_eq!(store.list().unwrap().len(), 1);

        assert!(store.remove("/tmp/a").unwrap());
        assert!(store.list().unwrap().is_empty());
        assert!(!store.remove("/tmp/a").unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn selected_project_roundtrip() {
        let (dir, store) = store("selected");
        assert!(store.selected().unwrap().is_none());
        store.set_selected("/tmp/a", "/tmp/a").unwrap();
        assert_eq!(
            store.selected().unwrap(),
            Some(("/tmp/a".to_string(), "/tmp/a".to_string()))
        );
        assert_eq!(store.selected_json()["projectRoot"], "/tmp/a");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
