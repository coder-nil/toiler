//! 项目列表与「上次选中的项目」—— toiler **自己的** SQLite 库
//! （`<appDir>/work.sqlite`，即 `~/.agents/toiler/work.sqlite`）。
//!
//! 早先直接读 pi 的 `~/.pi/agent/pi.sqlite`。共用库的好处是项目列表、星标、
//! 选中项与 pi 天然一致，坏处是要跟 pi 的 schema 完全对齐，而且两个工具各自
//! 读写同一份「自己的偏好」（星标、选中项）必然会互相覆盖。所以改成独立库：
//! schema 仍沿用这两张表（迁移方便），但之后不再跟 pi 耦合。
//!
//! 表结构：
//!
//! ```sql
//! CREATE TABLE added_projects (
//!   project_key TEXT PRIMARY KEY,
//!   project_root TEXT NOT NULL,
//!   cwd TEXT NOT NULL,
//!   added_at TEXT NOT NULL,
//!   starred INTEGER NOT NULL DEFAULT 0
//! );
//! CREATE TABLE app_preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);
//! ```
//!
//! `app_preferences` 里 `last_selected_project` 存 `{projectKey, projectRoot}`。
//!
//! 读写都开 WAL + `busy_timeout=3000`；每次操作打开一个连接，用完就关
//! （量很小，连接池反而更复杂）。
//!
//! 迁移：首次打开时若 `work.sqlite` 不存在而旧的 `pi.sqlite` 在，会把
//! `added_projects` 与 `last_selected_project` **复制**过来（只读旧库）。
//! 详见 [`ProjectStore::migrate_from_pi_if_needed`]。

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
        Self::at(paths::project_db())
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
        migrate_expanded_prefixes(&conn)?;
        Ok(conn)
    }

    /// 全部项目（星标优先，其次按加入时间倒序）。
    ///
    /// 返回前统一去掉 Windows 扩展路径前缀（读侧也做，老库里已有的带前缀行
    /// 不用等到下次写入就能正常显示）。
    pub fn list(&self) -> Result<Vec<Project>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT project_key, project_root, cwd, added_at, starred
             FROM added_projects
             ORDER BY starred DESC, added_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Project {
                project_key: paths::simplified(&row.get::<_, String>(0)?),
                project_root: paths::simplified(&row.get::<_, String>(1)?),
                cwd: paths::simplified(&row.get::<_, String>(2)?),
                added_at: row.get(3)?,
                starred: row.get::<_, i64>(4)? == 1,
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// 加入 / 更新项目（父目录变化时以新值为准）。
    ///
    /// 三个路径参数先过 [`paths::simplified`]：路由层可能把 `canonicalize` 的
    /// 结果（Windows 上是 `\\?\C:\…`）直接传进来，存带前缀的版本会让界面
    /// 显示得很奇怪，并且与用户自己输入的写法成两行。
    pub fn upsert(&self, project_key: &str, project_root: &str, cwd: &str) -> Result<Project> {
        let now = crate::toiler_now_iso();
        let project_key = paths::simplified(project_key);
        let project_root = paths::simplified(project_root);
        let cwd = paths::simplified(cwd);
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
        let project_key = paths::simplified(project_key);
        let conn = self.open()?;
        let changed = conn.execute(
            "UPDATE added_projects SET starred = ?1 WHERE project_key = ?2",
            params![i64::from(starred), project_key],
        )?;
        Ok(changed > 0)
    }

    /// 移除项目；返回是否命中一行。
    pub fn remove(&self, project_key: &str) -> Result<bool> {
        let project_key = paths::simplified(project_key);
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
                Ok(Some((paths::simplified(key), paths::simplified(root))))
            }
            _ => Ok(None),
        }
    }

    /// 写选中项（`58641.Ey`）—— 同样去扩展前缀再存。
    pub fn set_selected(&self, project_key: &str, project_root: &str) -> Result<()> {
        let now = crate::toiler_now_iso();
        let value = json!({
            "projectKey": paths::simplified(project_key),
            "projectRoot": paths::simplified(project_root),
        });
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

    /// 只读取上次选中的项目根，**绝不写盘**（不建目录、不建表、不迁移）。
    ///
    /// 为什么不能直接用 [`Self::selected`]：它走 [`Self::open`]，那里会
    /// `create_dir_all` 并 `CREATE TABLE IF NOT EXISTS`。而
    /// [`Self::migrate_from_pi_if_needed`] 的前提是「当前库还不存在」，一旦
    /// 被抢先在启动早期建出空库，用户从 `pi.sqlite` 搬过来的项目列表就永远
    /// 导不进来了。这个方法只在一个只读连接里读一行。
    pub fn selected_read_only(&self) -> Option<(String, String)> {
        if !self.db.is_file() {
            return None;
        }
        let conn =
            Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .ok()?;
        let raw: String = conn
            .query_row(
                "SELECT value FROM app_preferences WHERE key = 'last_selected_project'",
                [],
                |row| row.get(0),
            )
            .ok()?;
        let value: Value = serde_json::from_str(&raw).ok()?;
        let key = value.get("projectKey").and_then(Value::as_str)?;
        let root = value.get("projectRoot").and_then(Value::as_str)?;
        (!key.is_empty() && !root.is_empty())
            .then(|| (paths::simplified(key), paths::simplified(root)))
    }

    /// 首次建库时从旧的 `pi.sqlite` 导入项目与选中项。
    ///
    /// 触发条件（三者必须同时满足）：
    ///
    /// 1. 当前库（`work.sqlite`）**还不存在**——一旦建过就再不导入，
    ///    否则用户删掉的项目会在下次启动时“复活”；
    /// 2. `legacy`（旧的 `pi.sqlite`）存在；
    /// 3. 能打开它并读到 `added_projects`。
    ///
    /// **只读旧库**：不写它、不删它、不改 schema。旧库里的其它表（skills、
    /// plugin 等）一律不碰——它们属于 pi，不属于 toiler。
    ///
    /// 返回 `Some(导入的项目数)`；没触发时返回 `None`。
    pub fn migrate_from_pi_if_needed(&self, legacy: &Path) -> Result<Option<usize>> {
        if self.db.exists() {
            return Ok(None);
        }
        if !legacy.exists() {
            return Ok(None);
        }

        // 先读旧库（只读打开，避免意外写入甚至建出空的 -wal 文件）。
        let legacy_projects = {
            let source =
                Connection::open_with_flags(legacy, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .with_context(|| format!("只读打开旧库 {} 失败", legacy.display()))?;
            let has_table = source
                .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='added_projects'")
                .and_then(|mut stmt| stmt.exists([]))?;
            if !has_table {
                return Ok(None);
            }
            let mut stmt = source.prepare(
                "SELECT project_key, project_root, cwd, added_at, starred FROM added_projects",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(Project {
                    project_key: row.get(0)?,
                    project_root: row.get(1)?,
                    cwd: row.get(2)?,
                    added_at: row.get(3)?,
                    starred: row.get::<_, i64>(4).unwrap_or(0) == 1,
                })
            })?;
            let projects: Vec<Project> = rows.filter_map(Result::ok).collect();

            // 选中项也一并搬（取不到就是没有）
            let selected: Option<String> = source
                .prepare("SELECT value FROM app_preferences WHERE key = 'last_selected_project'")
                .and_then(|mut stmt| stmt.query_row([], |row| row.get::<_, String>(0)))
                .ok();
            (projects, selected)
        };

        let (projects, selected) = legacy_projects;
        if projects.is_empty() && selected.is_none() {
            return Ok(None);
        }

        // 写入新库。连接一建就落了 `work.sqlite`，所以迁移失败不会重复跑。
        let conn = self.open()?;
        let tx = conn.unchecked_transaction()?;
        for project in &projects {
            tx.execute(
                "INSERT OR IGNORE INTO added_projects(
                   project_key, project_root, cwd, added_at, starred)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    project.project_key,
                    project.project_root,
                    project.cwd,
                    project.added_at,
                    i64::from(project.starred)
                ],
            )?;
        }
        if let Some(value) = selected {
            tx.execute(
                "INSERT OR REPLACE INTO app_preferences(key, value, updated_at)
                 VALUES (?1, ?2, ?3)",
                params!["last_selected_project", value, crate::toiler_now_iso()],
            )?;
        }
        tx.commit()?;
        Ok(Some(projects.len()))
    }

    /// 用默认路径建库，并做一次迁移尝试（启动路径上调用）。
    ///
    /// 迁移失败不影响启动：项目列表是「锦上添花」的 UI 状态，读不到就空列表，
    /// 不该拦住用户开工。
    pub fn init_default() -> Self {
        let store = Self::default();
        match store.migrate_from_pi_if_needed(&paths::legacy_project_db()) {
            Ok(Some(count)) => {
                tracing::info!(
                    "已从 {} 导入 {} 个项目到 {}",
                    paths::legacy_project_db().display(),
                    count,
                    store.db.display()
                );
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!("迁移旧项目库失败（忽略，用空列表）：{error:#}");
            }
        }
        store
    }
}

/// 把历史行里的 Windows 扩展前缀（`\\?\`）归一成用户习惯的写法。
///
/// 早期版本把 `canonicalize` 的结果直接存进了项目库，于是界面上显示
/// `\\?\C:\…`。这里在 [`ProjectStore::open`] 里顺手改掉：只碰真的带前缀
/// 的行，没有这种行时零写入。
///
/// 归一后可能撞上已有的同名行（用户已经在正常写法下重新加过同一个项目），
/// 合并时保留星标、保留更早的 `added_at`，避免出现两行指向同一个目录。
fn migrate_expanded_prefixes(conn: &Connection) -> Result<()> {
    // 绝大多数库（macOS/Linux、以及已经迁过的 Windows 库）一条都不用改，
    // 先用一条 LIKE 判存在，避开每次 `open()` 都扫全表。
    let any = {
        let mut stmt = conn.prepare(
            "SELECT 1 FROM added_projects
             WHERE project_key LIKE '\\\\?\\%' OR project_root LIKE '\\\\?\\%'
                OR cwd LIKE '\\\\?\\%'
                OR project_key LIKE '/?/%' OR project_root LIKE '/?/%' OR cwd LIKE '/?/%'
             LIMIT 1",
        )?;
        stmt.exists([])?
    };
    if !any {
        return Ok(());
    }
    let rows: Vec<(String, String, String, String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT project_key, project_root, cwd, added_at, starred FROM added_projects",
        )?;
        let mapped = stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        mapped.filter_map(Result::ok).collect()
    };
    let stale: Vec<_> = rows
        .into_iter()
        .filter(|(key, root, cwd, _, _)| {
            let plain = (
                paths::simplified(key),
                paths::simplified(root),
                paths::simplified(cwd),
            );
            plain.0 != *key || plain.1 != *root || plain.2 != *cwd
        })
        .collect();
    if stale.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for (key, root, cwd, added_at, starred) in stale {
        let new_key = paths::simplified(&key);
        let new_root = paths::simplified(&root);
        let new_cwd = paths::simplified(&cwd);
        tx.execute(
            "DELETE FROM added_projects WHERE project_key = ?1",
            params![key],
        )?;
        // `ON CONFLICT` 保留星标与更早的加入时间（两条都指向同一目录）。
        tx.execute(
            "INSERT INTO added_projects(project_key, project_root, cwd, added_at, starred)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project_key) DO UPDATE SET
               project_root = excluded.project_root,
               cwd = excluded.cwd,
               added_at = MIN(added_projects.added_at, excluded.added_at),
               starred = MAX(added_projects.starred, excluded.starred)",
            params![new_key, new_root, new_cwd, added_at, starred],
        )?;
    }
    // 选中项里也可能存着带前缀的版本。
    let selected: Option<String> = tx
        .query_row(
            "SELECT value FROM app_preferences WHERE key = 'last_selected_project'",
            [],
            |row| row.get(0),
        )
        .ok();
    if let Some(raw) = selected {
        if let Ok(mut value) = serde_json::from_str::<Value>(&raw) {
            let mut changed = false;
            for field in ["projectKey", "projectRoot"] {
                if let Some(text) = value.get(field).and_then(Value::as_str) {
                    let plain = paths::simplified(text);
                    if plain != text {
                        value[field] = json!(plain);
                        changed = true;
                    }
                }
            }
            if changed {
                tx.execute(
                    "UPDATE app_preferences SET value = ?1, updated_at = ?2
                     WHERE key = 'last_selected_project'",
                    params![value.to_string(), crate::toiler_now_iso()],
                )?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> (PathBuf, ProjectStore) {
        let dir =
            std::env::temp_dir().join(format!("toiler-projects-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        let store = ProjectStore::at(dir.join("work.sqlite"));
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

    /// Windows 上 `canonicalize` 的结果带 `\\?\` 前缀，存进来就会原样显示给用户。
    /// 写入时就去掉；已存在的带前缀旧行也要在 `open()` 里被改掉。
    #[test]
    fn stores_and_migrates_plain_windows_paths() {
        let (dir, store) = store("prefix");
        let prefixed = r"\\?\C:\Users\me\proj";
        let plain = r"C:\Users\me\proj";

        // 写入：带前缀的入参落库时就是普通写法
        let project = store.upsert(prefixed, prefixed, prefixed).unwrap();
        assert_eq!(project.project_key, plain);
        assert_eq!(store.list().unwrap()[0].project_root, plain);

        // 选中项同理
        store.set_selected(prefixed, prefixed).unwrap();
        assert_eq!(
            store.selected().unwrap(),
            Some((plain.to_string(), plain.to_string()))
        );

        // 已是普通写法时，带前缀的键也能命中同一行（前端可能回传任一写法）
        assert!(store.set_starred(prefixed, true).unwrap());
        assert!(store.remove(prefixed).unwrap());
        assert!(store.list().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 老库里已经有带前缀的行：下次打开库时被就地改成普通写法，星标不丢。
    #[test]
    fn open_rewrites_legacy_prefixed_rows() {
        let dir =
            std::env::temp_dir().join(format!("toiler-projects-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("work.sqlite");
        {
            let store = ProjectStore::at(&db);
            store.open().unwrap();
        }
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute(
                "INSERT INTO added_projects(project_key, project_root, cwd, added_at, starred)
                 VALUES (?1, ?1, ?1, '2026-01-01T00:00:00.000Z', 1)",
                params![r"\\?\C:\Users\me\proj"],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO app_preferences(key, value, updated_at) VALUES ('last_selected_project', ?1, 'x')",
                params![r#"{"projectKey":"\\\\?\\C:\\Users\\me\\proj","projectRoot":"\\\\?\\C:\\Users\\me\\proj"}"#],
            )
            .unwrap();
        }

        let store = ProjectStore::at(&db);
        let projects = store.list().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].project_key, r"C:\Users\me\proj");
        assert!(projects[0].starred, "迁移不能丢星标");
        assert_eq!(
            store.selected().unwrap(),
            Some((
                r"C:\Users\me\proj".to_string(),
                r"C:\Users\me\proj".to_string()
            ))
        );
        // 落盘的也是普通写法，不只是读侧修的
        {
            let conn = Connection::open(&db).unwrap();
            let stored: String = conn
                .query_row("SELECT project_key FROM added_projects", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(stored, r"C:\Users\me\proj");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 只读查询不能建库：建出来就会让旧库迁移的「当前库还不存在」前提失效。
    ///
    /// 这是桌面壳启动时选默认项目的那条路径（`server::remembered_project_root`）。
    #[test]
    fn read_only_selection_lookup_never_creates_the_database() {
        let dir = std::env::temp_dir().join(format!("toiler-projects-ro-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = dir.join("work.sqlite");
        let store = ProjectStore::at(&db);

        assert_eq!(store.selected_read_only(), None, "没有库就是没有");
        assert!(!dir.exists(), "不能建目录");
        assert!(!db.exists(), "不能建库");

        // 写进去之后能读回；库里存的是什么就读出什么。
        store.set_selected("/tmp/x", "/tmp/x").unwrap();
        assert_eq!(
            store.selected_read_only(),
            Some(("/tmp/x".to_string(), "/tmp/x".to_string()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 旧库（`pi.sqlite`）里的项目与选中项要能完整搬到新库，且旧库不能被写。
    #[test]
    fn migrates_projects_and_selection_from_the_legacy_database() {
        let dir = std::env::temp_dir().join(format!("toiler-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("pi.sqlite");

        // 造一个「旧库」：两张表 + 两条项目 + 选中项 + 一张 toiler 不关心的表
        {
            let conn = Connection::open(&legacy).unwrap();
            conn.execute_batch(
                "CREATE TABLE added_projects (
                   project_key TEXT PRIMARY KEY, project_root TEXT NOT NULL,
                   cwd TEXT NOT NULL, added_at TEXT NOT NULL,
                   starred INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE app_preferences (
                   key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);
                 CREATE TABLE skill_cache (name TEXT);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO added_projects VALUES ('/w/a','/w/a','/w/a','2026-01-01T00:00:00.000Z',1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO added_projects VALUES ('/w/b','/w/b','/w/b/sub','2026-02-01T00:00:00.000Z',0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO app_preferences VALUES (
                   'last_selected_project','{\"projectKey\":\"/w/b\",\"projectRoot\":\"/w/b\"}',
                   '2026-02-01T00:00:00.000Z')",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO skill_cache VALUES ('x')", [])
                .unwrap();
        }
        let legacy_bytes_before = std::fs::read(&legacy).unwrap();

        let store = ProjectStore::at(dir.join("work.sqlite"));
        let imported = store.migrate_from_pi_if_needed(&legacy).unwrap();
        assert_eq!(imported, Some(2), "两条项目都要搬过来");

        // 项目、星标、选中项都到位
        let projects = store.list().unwrap();
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].project_key, "/w/a", "星标的排前面");
        assert!(projects[0].starred);
        assert_eq!(projects[1].cwd, "/w/b/sub", "cwd 原样保留");
        assert_eq!(
            store.selected().unwrap(),
            Some(("/w/b".to_string(), "/w/b".to_string()))
        );

        // 旧库必须一字未改（只读打开）
        assert_eq!(
            std::fs::read(&legacy).unwrap(),
            legacy_bytes_before,
            "迁移不能写旧库"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 新库一旦建过就不再导入：否则用户删掉的项目会在下次启动时「复活」。
    #[test]
    fn migration_runs_only_once() {
        let dir = std::env::temp_dir().join(format!("toiler-migrate-once-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("pi.sqlite");
        {
            let conn = Connection::open(&legacy).unwrap();
            conn.execute_batch(
                "CREATE TABLE added_projects (
                   project_key TEXT PRIMARY KEY, project_root TEXT NOT NULL,
                   cwd TEXT NOT NULL, added_at TEXT NOT NULL,
                   starred INTEGER NOT NULL DEFAULT 0);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO added_projects VALUES ('/w/a','/w/a','/w/a','2026-01-01T00:00:00.000Z',0)",
                [],
            )
            .unwrap();
        }

        let store = ProjectStore::at(dir.join("work.sqlite"));
        assert_eq!(store.migrate_from_pi_if_needed(&legacy).unwrap(), Some(1));

        // 用户删掉这个项目
        assert!(store.remove("/w/a").unwrap());
        // 再启动一次：库已存在，不重导
        assert_eq!(store.migrate_from_pi_if_needed(&legacy).unwrap(), None);
        assert!(store.list().unwrap().is_empty(), "删掉的项目不能复活");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 没有旧库 / 旧库没有那张表 / 旧库坏了 → 都安静地不迁移。
    #[test]
    fn migration_is_a_noop_without_a_usable_legacy_database() {
        let dir = std::env::temp_dir().join(format!("toiler-migrate-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = ProjectStore::at(dir.join("work.sqlite"));

        // 旧库根本不存在
        assert_eq!(
            store
                .migrate_from_pi_if_needed(&dir.join("missing.sqlite"))
                .unwrap(),
            None
        );
        assert!(!dir.join("work.sqlite").exists(), "没东西可导就不该建库");

        // 旧库存在但不是 SQLite
        let junk = dir.join("junk.sqlite");
        std::fs::write(&junk, b"not a database").unwrap();
        assert!(
            store.migrate_from_pi_if_needed(&junk).is_err(),
            "坏库应报错，由调用方兜底"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 默认路径必须是 `<toilerHome>/work.sqlite`，而不是 pi 的库。
    #[test]
    fn default_database_lives_in_the_app_directory() {
        let path = paths::project_db();
        assert_eq!(path.file_name().unwrap(), "work.sqlite");
        assert_eq!(path.parent().unwrap(), paths::toiler_home());
        assert!(
            path.to_string_lossy().ends_with("toiler/work.sqlite")
                || std::env::var_os("TOILER_HOME").is_some()
                || std::env::var_os("TOILER_AGENT_DIR").is_some()
                || std::env::var_os("AGENTS_HOME").is_some(),
            "{} 应在 <agents>/toiler/ 下",
            path.display()
        );
    }
}
