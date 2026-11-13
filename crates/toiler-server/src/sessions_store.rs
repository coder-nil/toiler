//! 会话文件的原始读取与列表派生。
//!
//! 契约层（`/api/sessions*`）以 **pi 的原始条目 JSON** 为准，不经过 rpi 的
//! `Entry` 模型——这样 tree/context/paging 的形状与 pi 逐字一致。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use toiler_session::V3Header;

/// 列表缓存 TTL（pi：30s）。
const LIST_CACHE_TTL: Duration = Duration::from_secs(30);

/// 项目探测缓存 TTL（pi 的 `24923.n` 也是 60s）。
const HINT_CACHE_TTL: Duration = Duration::from_secs(60);

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
                    Value::String(s) => toiler_session::iso_to_ms(s),
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

/// 扫整个会话根，用多线程读文件。
///
/// 500+ 个会话、10 万+ 条目时，串行「读文件 + 解析 JSON」是首屏最大的一块开销
/// （实测 ~7.8s → 并收后 ~1s）。分片读、按分片下标归并，保证结果顺序与串行一致；
/// 单个文件读坏不影响整体（与原来的 `if let Ok(..)` 行为一致）。
pub fn read_all_sessions(root: &Path) -> Vec<RawSession> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let Ok(dirs) = std::fs::read_dir(root) else {
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
            paths.push(path);
        }
    }

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8)
        .min(paths.len().max(1));
    if workers <= 1 {
        return paths.iter().filter_map(|p| read_session(p).ok()).collect();
    }

    let chunk = paths.len().div_ceil(workers);
    let mut parts: Vec<(usize, Vec<RawSession>)> = Vec::with_capacity(workers);
    std::thread::scope(|scope| {
        let handles: Vec<_> = paths
            .chunks(chunk)
            .enumerate()
            .map(|(index, slice)| {
                scope.spawn(move || {
                    let items: Vec<RawSession> =
                        slice.iter().filter_map(|p| read_session(p).ok()).collect();
                    (index, items)
                })
            })
            .collect();
        for handle in handles {
            if let Ok(part) = handle.join() {
                parts.push(part);
            }
        }
    });
    parts.sort_by_key(|(index, _)| *index);
    parts.into_iter().flat_map(|(_, items)| items).collect()
}

/// 会话列表需要的字段（只有这五个）。
///
/// 列表接口**不需要整份 transcript**：`name` / `messageCount` / `firstMessage`
/// 都能边读边算，`cwd` / `created` / `parentSession` 在 header 里。
pub struct SessionSummary {
    pub path: PathBuf,
    pub header: V3Header,
    /// 最后一个 `session_info` 的名字（pi 的 `getSessionName`）。
    pub name: Option<String>,
    pub message_count: usize,
    pub first_message: String,
    /// header 里的 `parentSession`（路径），不在这里归一。
    pub parent_session: Option<String>,
    pub mtime_ms: i64,
}

/// 每个文件只读到这五样东西，而不是把整份 transcript 解成 `Vec<Value>`。
///
/// 为什么要单独开一条路径：`/api/sessions` 是首屏接口，而真实会话根里
/// 596 个文件共 813MB（单个最大 53MB）。走 [`read_session`] 会把每一行都
/// 反序列化一遍（包括巨大的 `content` 数组），实测 `?force=1` 要 ~2.0s；
/// 而列表只用到五个字段。
///
/// 做法是逐行只抽需要的那几个键：仍然用 `serde_json::Value` 解析（不手写
/// JSON 扫描器，坏行的容错语义与 [`read_session`] 完全一致），但不保留
/// 任何内容，堆占用是常数级。
fn read_summary(path: &Path) -> Option<SessionSummary> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header_line = lines.next()?;
    let header_value: Value = serde_json::from_str(header_line).ok()?;
    let header = V3Header::parse(header_line).ok()?;
    let parent_session = header_value
        .get("parentSession")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut name: Option<String> = None;
    let mut message_count = 0usize;
    let mut first_message: Option<String> = None;

    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("session") => continue,
            Some("session_info") => {
                if let Some(n) = value.get("name").and_then(Value::as_str) {
                    name = Some(n.to_string());
                }
            }
            Some("message") => {
                message_count += 1;
                // 只看第一条 user 消息（与 `first_message_text` 同语义）。
                if first_message.is_none() {
                    if let Some(message) = value.get("message") {
                        if message.get("role").and_then(Value::as_str) == Some("user") {
                            first_message = Some(extract_text(message.get("content")));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let mtime_ms = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(header.created_at);

    Some(SessionSummary {
        path: path.to_path_buf(),
        header,
        name,
        message_count,
        first_message: first_message.unwrap_or_else(|| "(no messages)".to_string()),
        parent_session,
        mtime_ms,
    })
}

/// 与 `session_view::first_message_text` 同语义：字符串直接用，数组取第一个 text 块。
fn extract_text(content: Option<&Value>) -> String {
    let text = match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .find(|i| i.get("type").and_then(Value::as_str) == Some("text"))
            .and_then(|i| i.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    };
    if text.is_empty() {
        "(no messages)".to_string()
    } else {
        text
    }
}

/// [`read_all_sessions`] 的列表版：只收五个字段，不分片保留 transcript。
/// 分片与归并顺序与 [`read_all_sessions`] 一致。
pub fn read_all_summaries(root: &Path) -> Vec<SessionSummary> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let Ok(dirs) = std::fs::read_dir(root) else {
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
            paths.push(path);
        }
    }

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8)
        .min(paths.len().max(1));
    if workers <= 1 {
        return paths.iter().filter_map(|p| read_summary(p)).collect();
    }

    let chunk = paths.len().div_ceil(workers);
    let mut parts: Vec<(usize, Vec<SessionSummary>)> = Vec::with_capacity(workers);
    std::thread::scope(|scope| {
        let handles: Vec<_> = paths
            .chunks(chunk)
            .enumerate()
            .map(|(index, slice)| {
                scope.spawn(move || {
                    let items: Vec<SessionSummary> =
                        slice.iter().filter_map(|p| read_summary(p)).collect();
                    (index, items)
                })
            })
            .collect();
        for handle in handles {
            if let Ok(part) = handle.join() {
                parts.push(part);
            }
        }
    });
    parts.sort_by_key(|(index, _)| *index);
    parts.into_iter().flat_map(|(_, items)| items).collect()
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
    /// 扫描互斥：并发请求（例如界面首屏同时发两个请求）只扫一遍，
    /// 后来的直接命中前一个算出的缓存，不重复读 500+ 个会话文件。
    scan_lock: Mutex<()>,
}

impl SessionList {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cache: Mutex::new(None),
            scan_lock: Mutex::new(()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `force` = 跳过缓存（`?force=1`）。
    pub fn list(&self, force: bool) -> Vec<Value> {
        if !force {
            if let Some(items) = self.cached(force) {
                return items;
            }
        }
        // 慢路径串行化：第一个线程扫完，其余线程拿到锁后直接复用结果。
        let _guard = self.scan_lock.lock().unwrap_or_else(|e| e.into_inner());
        if !force {
            if let Some(items) = self.cached(force) {
                return items;
            }
        }
        let items = self.scan();
        *self.cache.lock().expect("cache") = Some((Instant::now(), items.clone()));
        items
    }

    /// 命中缓存就返回副本（`force` 为 true 时一定不命中）。
    fn cached(&self, force: bool) -> Option<Vec<Value>> {
        if force {
            return None;
        }
        let guard = self.cache.lock().expect("cache");
        let (at, items) = guard.as_ref()?;
        if at.elapsed() < LIST_CACHE_TTL {
            Some(items.clone())
        } else {
            None
        }
    }

    /// 失效缓存（写入 / 删除后调用，对应 pi 的 `mD()`）。
    pub fn invalidate(&self) {
        *self.cache.lock().expect("cache") = None;
    }

    fn scan(&self) -> Vec<Value> {
        // 列表只要五个字段，所以走轻量扫描（`read_all_summaries`）。
        // 以前这里调 `read_all_sessions`，会把 596 个文件（共 813MB）的每一行
        // 都解成 `Value` 再丢掉 —— `?force=1` 因此要 ~2.0s。
        let sessions = read_all_summaries(&self.root);

        // parentSessionId：header.parentSession（路径）→ 该路径对应会话的 id
        let by_path: HashMap<String, String> = sessions
            .iter()
            .map(|s| {
                (
                    normalize_path(&s.path.to_string_lossy()),
                    s.header.id.clone(),
                )
            })
            .collect();

        // 同一个 cwd 只做一次 git 探测
        let hints: HashMap<String, ProjectHint> = sessions
            .iter()
            .map(|s| s.header.cwd.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|cwd| (cwd.clone(), project_hint(&cwd)))
            .collect();

        let mut items: Vec<Value> = sessions
            .iter()
            .map(|s| {
                let parent = s
                    .parent_session
                    .as_deref()
                    .and_then(|p| by_path.get(&normalize_path(p)).cloned());
                let hint = hints.get(&s.header.cwd).cloned().unwrap_or(ProjectHint {
                    root: normalize_path(&s.header.cwd),
                    worktree_branch: None,
                });
                let project_root = hint.root.clone();
                let mut obj = serde_json::Map::new();
                obj.insert("path".into(), json!(s.path.to_string_lossy().to_string()));
                obj.insert("id".into(), json!(s.header.id));
                obj.insert("cwd".into(), json!(s.header.cwd));
                obj.insert("name".into(), json!(s.name));
                obj.insert(
                    "created".into(),
                    json!(toiler_session::ms_to_iso(s.header.created_at)),
                );
                obj.insert(
                    "modified".into(),
                    json!(toiler_session::ms_to_iso(s.mtime_ms)),
                );
                obj.insert("messageCount".into(), json!(s.message_count));
                obj.insert("firstMessage".into(), json!(s.first_message));
                obj.insert("parentSessionId".into(), json!(parent));
                obj.insert("transient".into(), json!(false));
                obj.insert("projectRoot".into(), json!(project_root));
                obj.insert("projectKey".into(), json!(normalize_path(&project_root)));
                if let Some(branch) = hint.worktree_branch.clone() {
                    obj.insert("worktreeBranch".into(), json!(branch));
                }
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
                    if toiler_session::home::session_id_from_file_name(name) == Some(id) {
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

/// 会话所属项目：`projectRoot` + （worktree 才有）`worktreeBranch`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectHint {
    pub root: String,
    pub worktree_branch: Option<String>,
}

/// 项目探测缓存：cwd → 提示，60 秒（pi 的 `24923.n` 用同样的 TTL）。
fn hint_cache() -> &'static Mutex<HashMap<String, (Instant, ProjectHint)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, ProjectHint)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// git 探测项目根（`projectRoot` 不再等于 `cwd`）。
///
/// 不是 git 仓库时 `root = cwd`；是 worktree 时 `root = 主仓库根`，
/// 并带上分支名（pi 的 `worktreeBranch`）。
pub fn project_hint(cwd: &str) -> ProjectHint {
    if cwd.is_empty() {
        return ProjectHint {
            root: String::new(),
            worktree_branch: None,
        };
    }
    let key = normalize_path(cwd);
    {
        let cache = hint_cache().lock().expect("hint cache");
        if let Some((at, hint)) = cache.get(&key) {
            if at.elapsed() < HINT_CACHE_TTL {
                return hint.clone();
            }
        }
    }
    let (root, branch) = crate::gitsvc::project_hint(&key);
    let hint = ProjectHint {
        root: normalize_path(&root),
        worktree_branch: branch,
    };
    hint_cache()
        .lock()
        .expect("hint cache")
        .insert(key, (Instant::now(), hint.clone()));
    hint
}

/// 项目根（只要 `projectRoot` 的调用方用这个）。
pub fn project_root(cwd: &str) -> String {
    project_hint(cwd).root
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

    /// 轻量扫描必须与整份回放给出**同样的**列表字段。
    ///
    /// 这条用例守的是性能优化不能改变语义：`read_all_summaries` 只抽五个字段，
    /// 而 `read_all_sessions` 把每行都解成 `Value`。两者在 `id` / `name` /
    /// `messageCount` / `firstMessage` / `parentSessionId` / `modified` 上
    /// 必须逐条相等——否则首屏列表会悄悄少名字、少消息数。
    #[test]
    fn lightweight_scan_matches_the_full_replay() {
        let dir = std::env::temp_dir().join(format!("toiler-summary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let session_dir = dir.join("--tmp-demo--");
        std::fs::create_dir_all(&session_dir).unwrap();

        // 1) 有名字、有 user 消息、带 parentSession
        std::fs::write(
            session_dir.join("2026-11-10T06-41-44-431Z_a.jsonl"),
            concat!(
                r#"{"type":"session","version":3,"id":"a","timestamp":"2026-11-10T06:41:44.431Z","cwd":"/tmp/demo","parentSession":"/tmp/demo/x.jsonl"}"#,
                "\n",
                r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-11-10T06:41:45.000Z","message":{"role":"user","content":[{"type":"text","text":"第一条"}],"timestamp":1794292905000}}"#,
                "\n",
                r#"{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-11-10T06:41:46.000Z","message":{"role":"assistant","content":[{"type":"text","text":"回答"}],"timestamp":1794292906000}}"#,
                "\n",
                r#"{"type":"session_info","id":"m3","parentId":"m2","timestamp":"2026-11-10T06:41:47.000Z","name":"第一次改名"}"#,
                "\n",
                r#"{"type":"session_info","id":"m4","parentId":"m3","timestamp":"2026-11-10T06:41:48.000Z","name":"最后一次生效"}"#,
                "\n",
            ),
        )
        .unwrap();

        // 2) 没有名字、没有 user 消息（firstMessage 要落到 `(no messages)`）
        std::fs::write(
            session_dir.join("2026-11-10T07-00-00-000Z_b.jsonl"),
            concat!(
                r#"{"type":"session","version":3,"id":"b","timestamp":"2026-11-10T07:00:00.000Z","cwd":"/tmp/other"}"#,
                "\n",
                r#"{"type":"message","id":"n1","parentId":null,"timestamp":"2026-11-10T07:00:01.000Z","message":{"role":"assistant","content":[{"type":"text","text":"先说话的是助手"}],"timestamp":1794293001000}}"#,
                "\n",
            ),
        )
        .unwrap();

        // 3) 坏行要在两条路径上都被跳过且不影响其余字段
        std::fs::write(
            session_dir.join("2026-11-10T08-00-00-000Z_c.jsonl"),
            concat!(
                r#"{"type":"session","version":3,"id":"c","timestamp":"2026-11-10T08:00:00.000Z","cwd":"/tmp/demo"}"#,
                "\n",
                "{ this is not json ",
                "\n",
                r#"{"type":"message","id":"p1","parentId":null,"timestamp":"2026-11-10T08:00:01.000Z","message":{"role":"user","content":"纯字符串内容","timestamp":1794293201000}}"#,
                "\n",
            ),
        )
        .unwrap();

        // 4) 首行不是 session header 的文件：两条路径都应丢弃
        std::fs::write(
            session_dir.join("2026-11-10T09-00-00-000Z_broken.jsonl"),
            r#"{"type":"message","id":"z","parentId":null,"timestamp":"2026-11-10T09:00:00.000Z","message":{"role":"user","content":"没有 header"},"timestamp":1794293401000}"#,
        )
        .unwrap();

        let full = read_all_sessions(&dir);
        let light = read_all_summaries(&dir);
        assert_eq!(
            full.len(),
            light.len(),
            "两条路径的会话数必须一致（都应为 3，坏 header 的文件被丢弃）"
        );
        assert_eq!(full.len(), 3);

        let mut by_id: std::collections::HashMap<String, &SessionSummary> =
            std::collections::HashMap::new();
        for s in &light {
            by_id.insert(s.header.id.clone(), s);
        }
        for full_session in &full {
            let light_session = by_id
                .get(&full_session.header.id)
                .unwrap_or_else(|| panic!("轻量扫描漏了会话 {}", full_session.header.id));
            assert_eq!(light_session.header.cwd, full_session.header.cwd);
            assert_eq!(
                light_session.header.created_at,
                full_session.header.created_at
            );
            assert_eq!(light_session.mtime_ms, full_session.mtime_ms);
            assert_eq!(
                light_session.name,
                full_session.name(),
                "name 不一致：{}",
                full_session.header.id
            );
            assert_eq!(
                light_session.message_count,
                full_session.message_count(),
                "messageCount 不一致：{}",
                full_session.header.id
            );
            assert_eq!(
                light_session.first_message,
                crate::session_view::first_message_text(&full_session.entries),
                "firstMessage 不一致：{}",
                full_session.header.id
            );
            assert_eq!(
                light_session.parent_session.as_deref(),
                full_session
                    .header_line
                    .get("parentSession")
                    .and_then(Value::as_str),
                "parentSession 不一致：{}",
                full_session.header.id
            );
        }

        // 具体值也钉一下，避免「两边都错」时用例仍然通过。
        assert_eq!(by_id["a"].name.as_deref(), Some("最后一次生效"));
        assert_eq!(by_id["a"].message_count, 2);
        assert_eq!(by_id["a"].first_message, "第一条");
        assert_eq!(
            by_id["a"].parent_session.as_deref(),
            Some("/tmp/demo/x.jsonl")
        );
        assert_eq!(by_id["b"].name, None);
        assert_eq!(by_id["b"].message_count, 1);
        assert_eq!(by_id["b"].first_message, "(no messages)");
        assert_eq!(by_id["c"].message_count, 1);
        assert_eq!(by_id["c"].first_message, "纯字符串内容");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_a_real_session_shape() {
        let dir = std::env::temp_dir().join(format!("toiler-list-{}", std::process::id()));
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
