//! 会话文件的原始读取与列表派生。
//!
//! 契约层（`/api/sessions*`）以 **pi 的原始条目 JSON** 为准，不经过 rpi 的
//! `Entry` 模型——这样 tree/context/paging 的形状与 pi 逐字一致。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use workman_session::V3Header;
use serde_json::{json, Value};

use crate::session_view;

/// 列表缓存 TTL（pi：30s）。
const LIST_CACHE_TTL: Duration = Duration::from_secs(30);

/// 一个会话文件的原始视图。
pub struct RawSession {
    pub path: PathBuf,
    pub header_line: Value,
    pub header: V3Header,
    pub entries: Vec<Value>,
    pub mtime_ms: i64,
}

impl RawSession {
    /// 条目里的 `session_info` 名（最后一个生效，pi 的 getSessionName）。
    pub fn name(&self) -> Option<String> {
        let mut name = None;
        for entry in &self.entries {
            if entry.get("type").and_then(Value::as_str) == Some("session_info") {
                if let Some(n) = entry.get("name").and_then(Value::as_str) {
                    name = Some(n.to_string());
                }
            }
        }
        name
    }

    /// 分支标签（`label` 条目：`{type:"label", targetId, label}`）。
    pub fn labels(&self) -> Vec<(String, String, i64)> {
        let mut out = Vec::new();
        for entry in &self.entries {
            if entry.get("type").and_then(Value::as_str) != Some("label") {
                continue;
            }
            let Some(target) = entry.get("targetId").and_then(Value::as_str) else {
                continue;
            };
            let Some(label) = entry.get("label").and_then(Value::as_str) else {
                continue;
            };
            let ts = entry
                .get("timestamp")
                .and_then(|v| match v {
                    Value::String(s) => workman_session::iso_to_ms(s),
                    Value::Number(n) => n.as_i64(),
                    _ => None,
                })
                .unwrap_or(0);
            out.push((target.to_string(), label.to_string(), ts));
        }
        out
    }

    pub fn message_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.get("type").and_then(Value::as_str) == Some("message"))
            .count()
    }
}

/// 读一个 v3 会话文件（header + 全部条目，坏行跳过）。
pub fn read_session(path: &Path) -> anyhow::Result<RawSession> {
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header_line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("{} 是空文件", path.display()))?;
    let header_value: Value = serde_json::from_str(header_line)?;
    let header = V3Header::parse(header_line)?;

    let mut entries = Vec::new();
    for line in lines {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if v.get("type").and_then(Value::as_str) != Some("session") {
                entries.push(v);
            }
        }
    }

    let mtime_ms = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(header.created_at);

    Ok(RawSession {
        path: path.to_path_buf(),
        header_line: header_value,
        header,
        entries,
        mtime_ms,
    })
}

/// posix 归一化 + 去尾部分隔符（复刻 `74244.z`）。
pub fn normalize_path(path: &str) -> String {
    let mut out = path.to_string();
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    let mut parts: Vec<&str> = Vec::new();
    for seg in out.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    if out.starts_with('/') {
        format!("/{}", parts.join("/"))
    } else if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// 会话列表（带 30 秒缓存，复刻 pi 的 `dQ`）。
pub struct SessionList {
    root: PathBuf,
    cache: Mutex<Option<(Instant, Vec<Value>)>>,
}

impl SessionList {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cache: Mutex::new(None),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `force` = 跳过缓存（`?force=1`）。
    pub fn list(&self, force: bool) -> Vec<Value> {
        if !force {
            let guard = self.cache.lock().expect("cache");
            if let Some((at, items)) = guard.as_ref() {
                if at.elapsed() < LIST_CACHE_TTL {
                    return items.clone();
                }
            }
        }
        let items = self.scan();
        *self.cache.lock().expect("cache") = Some((Instant::now(), items.clone()));
        items
    }

    /// 失效缓存（写入 / 删除后调用，对应 pi 的 `mD()`）。
    pub fn invalidate(&self) {
        *self.cache.lock().expect("cache") = None;
    }

    fn scan(&self) -> Vec<Value> {
        let mut sessions: Vec<RawSession> = Vec::new();
        let Ok(dirs) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        for dir in dirs.flatten() {
            let dir_path = dir.path();
            if !dir_path.is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&dir_path) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().map_or(true, |e| e != "jsonl") {
                    continue;
                }
                if let Ok(session) = read_session(&path) {
                    sessions.push(session);
                }
            }
        }

        // parentSessionId：header.parentSession（路径）→ 该路径对应会话的 id
        let by_path: HashMap<String, String> = sessions
            .iter()
            .map(|s| (normalize_path(&s.path.to_string_lossy()), s.header.id.clone()))
            .collect();

        let mut items: Vec<Value> = sessions
            .iter()
            .map(|s| {
                let parent = s
                    .header_line
                    .get("parentSession")
                    .and_then(Value::as_str)
                    .and_then(|p| by_path.get(&normalize_path(p)).cloned());
                let project_root = project_root(&s.header.cwd);
                let mut obj = serde_json::Map::new();
                obj.insert(
                    "path".into(),
                    json!(s.path.to_string_lossy().to_string()),
                );
                obj.insert("id".into(), json!(s.header.id));
                obj.insert("cwd".into(), json!(s.header.cwd));
                obj.insert("name".into(), json!(s.name()));
                obj.insert("created".into(), json!(workman_session::ms_to_iso(s.header.created_at)));
                obj.insert("modified".into(), json!(workman_session::ms_to_iso(s.mtime_ms)));
                obj.insert("messageCount".into(), json!(s.message_count()));
                obj.insert(
                    "firstMessage".into(),
                    json!(session_view::first_message_text(&s.entries)),
                );
                obj.insert("parentSessionId".into(), json!(parent));
                obj.insert("transient".into(), json!(false));
                obj.insert("projectRoot".into(), json!(project_root));
                obj.insert("projectKey".into(), json!(normalize_path(&project_root)));
                Value::Object(obj)
            })
            .collect();

        // pi：按 modified 字符串倒序
        items.sort_by(|a, b| {
            let am = a.get("modified").and_then(Value::as_str).unwrap_or("");
            let bm = b.get("modified").and_then(Value::as_str).unwrap_or("");
            bm.cmp(am)
        });
        items
    }

    /// 按 id 找文件（复刻 `40402.os`）。
    pub fn path_of(&self, id: &str) -> Option<PathBuf> {
        let Ok(dirs) = std::fs::read_dir(&self.root) else {
            return None;
        };
        for dir in dirs.flatten() {
            let dir_path = dir.path();
            if !dir_path.is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&dir_path) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().map_or(true, |e| e != "jsonl") {
                    continue;
                }
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if workman_session::home::session_id_from_file_name(name) == Some(id) {
                        return Some(path);
                    }
                }
            }
        }
        None
    }

    /// 读会话（按 id）。
    pub fn session(&self, id: &str) -> Option<RawSession> {
        let path = self.path_of(id)?;
        read_session(&path).ok()
    }
}

/// 项目根：阶段 2 直接取 cwd（git 探测属于阶段 3 的 `/api/git`）。
pub fn project_root(cwd: &str) -> String {
    if cwd.is_empty() {
        return String::new();
    }
    normalize_path(cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_paths_like_pi() {
        assert_eq!(normalize_path("/a/b/"), "/a/b");
        assert_eq!(normalize_path("/a//b/./c"), "/a/b/c");
        assert_eq!(normalize_path("/a/b/../c"), "/a/c");
        assert_eq!(normalize_path("/"), "/");
    }

    #[test]
    fn reads_a_real_session_shape() {
        let dir = std::env::temp_dir().join(format!("workman-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let session_dir = dir.join("--tmp-demo--");
        std::fs::create_dir_all(&session_dir).unwrap();
        let file = session_dir.join("2026-11-10T06-41-44-431Z_sess1.jsonl");
        std::fs::write(
            &file,
            concat!(
                r#"{"type":"session","version":3,"id":"sess1","timestamp":"2026-11-10T06:41:44.431Z","cwd":"/tmp/demo"}"#,
                "\n",
                r#"{"type":"message","id":"a","parentId":null,"timestamp":"2026-11-10T06:41:45.000Z","message":{"role":"user","content":[{"type":"text","text":"你好"}],"timestamp":1794292905000}}"#,
                "\n",
                r#"{"type":"session_info","id":"b","parentId":"a","timestamp":"2026-11-10T06:41:46.000Z","name":"演示"}"#,
                "\n"
            ),
        )
        .unwrap();

        let list = SessionList::new(&dir);
        let items = list.list(true);
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item["id"], "sess1");
        assert_eq!(item["name"], "演示");
        assert_eq!(item["messageCount"], 1);
        assert_eq!(item["firstMessage"], "你好");
        assert_eq!(item["cwd"], "/tmp/demo");
        assert_eq!(item["projectKey"], "/tmp/demo");
        assert!(item["created"].as_str().unwrap().ends_with('Z'));

        assert!(list.path_of("sess1").is_some());
        assert!(list.session("sess1").is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
