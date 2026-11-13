//! 会话视图派生 —— 复刻 pi-desktop `core/session-*`、`core/api/*` 的纯函数。
//!
//! 每个函数都对应产物里的一个压缩模块，注释里标了出处：
//! - [`list_item`]        ← `40402.p()` / `40402.n()`
//! - [`tree_projection`]  ← `14858.g()`
//! - [`page_tail`] / [`page_until`] ← `37665.h()` / `37665.i()`
//! - [`total_active_ms`]  ← `89691.c()`
//!
//! 这里只做「纯计算」，文件读取留给调用方。

use serde_json::{json, Map, Value};

/// `firstMessage` 摘要的字符上限（pi：41 字节内 + 省略号）。
const PREVIEW_LIMIT: usize = 41;

/// 复刻 `14858.e()`：在 41 字符内累积预览文本。
fn append_preview(acc: &str, text: &str) -> String {
    if acc.chars().count() > 40 {
        return acc.to_string();
    }
    let normalized: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return acc.to_string();
    }
    let sep = if acc.is_empty() { 0 } else { 1 };
    let used = acc.chars().count() + sep;
    if used >= PREVIEW_LIMIT {
        return format!("{acc} ");
    }
    let room = PREVIEW_LIMIT - used;
    let taken: String = normalized.chars().take(room).collect();
    if acc.is_empty() {
        taken
    } else {
        format!("{acc} {taken}")
    }
}

/// 复刻 `14858.f()`：一个条目的分支预览 `{role?, text}`；system 与空消息返回 `None`。
pub fn branch_preview(entry: &Value) -> Option<Value> {
    if entry.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let message = entry.get("message")?.as_object()?;
    let role = message.get("role").and_then(Value::as_str)?;
    if role == "system" {
        return None;
    }

    let mut text = String::new();
    let mut has_image = false;
    match message.get("content") {
        Some(Value::String(s)) => text = append_preview(&text, s),
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(ty) = item.get("type").and_then(Value::as_str) {
                    if ty == "image" {
                        has_image = true;
                    }
                    if ty == "text" {
                        if let Some(t) = item.get("text").and_then(Value::as_str) {
                            text = append_preview(&text, t);
                        }
                    }
                }
                if text.chars().count() > 40 {
                    break;
                }
            }
        }
        _ => {}
    }

    if text.chars().count() > 40 {
        let head: String = text.chars().take(40).collect();
        text = format!("{head}…");
    } else if text.is_empty() {
        text = if has_image {
            "[image]".to_string()
        } else if role == "assistant" {
            "[assistant]".to_string()
        } else {
            "message".to_string()
        };
    }

    let mut out = Map::new();
    if role == "user" || role == "assistant" {
        out.insert("role".into(), json!(role));
    }
    out.insert("text".into(), json!(text));
    Some(Value::Object(out))
}

/// 从条目里取第一条 user 消息的文本（pi：`"string" | blocks.find(text)`）。
pub fn first_message_text(entries: &[Value]) -> String {
    for entry in entries {
        if entry.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(message) = entry.get("message") else {
            continue;
        };
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let content = message.get("content");
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
        return if text.is_empty() {
            "(no messages)".to_string()
        } else {
            text
        };
    }
    "(no messages)".to_string()
}

/// 树节点：`{entry, children[], label?, labelTimestamp?}`（见 SessionManager.getTree）。
#[derive(Debug, Clone)]
pub struct TreeNode {
    pub entry: Value,
    pub children: Vec<TreeNode>,
    pub label: Option<String>,
    pub label_timestamp: Option<i64>,
}

/// 由条目列表构建树：`parentId` 为 null/自身/父缺失的条目都是根。
pub fn build_tree(entries: &[Value], labels: &[(String, String, i64)]) -> Vec<TreeNode> {
    let label_of = |id: &str| {
        labels
            .iter()
            .find(|(target, _, _)| target == id)
            .map(|(_, text, ts)| (text.clone(), *ts))
    };

    let nodes: Vec<TreeNode> = entries
        .iter()
        .map(|entry| {
            let id = entry.get("id").and_then(Value::as_str).unwrap_or_default();
            let (label, label_timestamp) = match label_of(id) {
                Some((text, ts)) => (Some(text), Some(ts)),
                None => (None, None),
            };
            TreeNode {
                entry: entry.clone(),
                children: Vec::new(),
                label,
                label_timestamp,
            }
        })
        .collect();

    let index: std::collections::HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| {
            n.entry
                .get("id")
                .and_then(Value::as_str)
                .map(|id| (id.to_string(), i))
        })
        .collect();

    // children 按 parentId 归位；记录每个节点的父 index
    let mut parent_of: Vec<Option<usize>> = vec![None; nodes.len()];
    for i in 0..nodes.len() {
        let parent = nodes[i]
            .entry
            .get("parentId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let self_id = nodes[i]
            .entry
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let Some(parent_id) = parent else { continue };
        if parent_id == self_id {
            continue;
        }
        if let Some(&pi) = index.get(&parent_id) {
            parent_of[i] = Some(pi);
        }
    }

    // 自底向上组装（条目按时间递增，父一定在子之前，但不要依赖它）
    let mut child_lists: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (i, parent) in parent_of.iter().enumerate() {
        if let Some(p) = parent {
            child_lists[*p].push(i);
        }
    }
    let mut built: Vec<Option<TreeNode>> = nodes.into_iter().map(Some).collect();
    // 迭代式后序：先算叶子
    let order = post_order(&child_lists);
    for i in order {
        let children = std::mem::take(&mut child_lists[i]);
        let mut node = built[i].take().expect("node present");
        node.children = children
            .into_iter()
            .filter_map(|c| built[c].take())
            .collect();
        node.children.sort_by_key(|c| timestamp_of(&c.entry));
        built[i] = Some(node);
    }

    let mut roots: Vec<TreeNode> = (0..built.len())
        .filter(|i| parent_of[*i].is_none())
        .filter_map(|i| built[i].take())
        .collect();
    roots.sort_by_key(|n| timestamp_of(&n.entry));
    roots
}

fn post_order(children: &[Vec<usize>]) -> Vec<usize> {
    let mut out = Vec::with_capacity(children.len());
    let mut visited = vec![false; children.len()];
    for start in 0..children.len() {
        if visited[start] {
            continue;
        }
        let mut stack = vec![(start, false)];
        while let Some((node, expanded)) = stack.pop() {
            if expanded {
                if !visited[node] {
                    visited[node] = true;
                    out.push(node);
                }
                continue;
            }
            if visited[node] {
                continue;
            }
            stack.push((node, true));
            for &c in &children[node] {
                if !visited[c] {
                    stack.push((c, false));
                }
            }
        }
    }
    out
}

fn timestamp_of(entry: &Value) -> i64 {
    match entry.get("timestamp") {
        Some(Value::String(iso)) => toiler_session::iso_to_ms(iso).unwrap_or(0),
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        _ => 0,
    }
}

/// 复刻 `14858.g()`：把树投影成前端形状，深度超 200 折叠进 `compressedEntryIds`。
pub fn tree_projection(roots: &[TreeNode]) -> Value {
    fn project(node: &TreeNode, compressed: Vec<String>, preview: Option<Value>) -> Value {
        let mut obj = Map::new();
        obj.insert("entry".into(), node.entry.clone());
        obj.insert("children".into(), json!([]));
        if let Some(label) = &node.label {
            obj.insert("label".into(), json!(label));
            if let Some(ts) = node.label_timestamp {
                obj.insert("labelTimestamp".into(), json!(ts));
            }
        }
        if !compressed.is_empty() {
            obj.insert("compressedEntryIds".into(), json!(compressed));
        }
        if let Some(p) = preview {
            obj.insert("branchPreview".into(), p);
        }
        Value::Object(obj)
    }

    // depth 上限：pi 是 200
    const MAX_DEPTH: usize = 200;

    // 迭代式深度优先，避免深树爆栈
    struct Frame {
        node: TreeNode,
        depth: usize,
        out: Value,
    }

    let mut roots_out: Vec<Value> = Vec::new();
    let mut stack: Vec<Frame> = roots
        .iter()
        .map(|r| Frame {
            node: r.clone(),
            depth: 1,
            out: project(r, Vec::new(), branch_preview(&r.entry)),
        })
        .collect();

    // 先放占位根，出栈时回填
    let mut pending: Vec<(Vec<usize>, Value)> = Vec::new();
    let _ = &mut pending;

    while let Some(frame) = stack.pop() {
        let mut children_out: Vec<Value> = Vec::new();
        for child in &frame.node.children {
            if frame.depth >= MAX_DEPTH {
                children_out.push(json!({
                    "entry": child.entry.clone(),
                    "children": [],
                    "compressedEntryIds": child.children.iter()
                        .map(|c| c.entry.get("id").cloned().unwrap_or(Value::Null))
                        .collect::<Vec<_>>(),
                }));
                continue;
            }
            let preview = branch_preview(&child.entry).or_else(|| {
                child
                    .children
                    .first()
                    .and_then(|c| branch_preview(&c.entry))
            });
            let projected = project(child, Vec::new(), preview.clone());
            stack.push(Frame {
                node: child.clone(),
                depth: frame.depth + 1,
                out: projected.clone(),
            });
            children_out.push(projected);
        }
        // 回填：把 out 的 children 换成 children_out（这里重建对象）
        let mut rebuilt = frame.out;
        if let Some(obj) = rebuilt.as_object_mut() {
            obj.insert("children".into(), Value::Array(children_out));
        }
        if frame.depth == 1 {
            roots_out.push(rebuilt);
        }
    }

    Value::Array(roots_out)
}

// ------------------------------------------------------------------ 分页

/// pi 的默认页大小。
pub const DEFAULT_PAGE_SIZE: usize = 120;
/// pi 的页大小上限。
pub const MAX_PAGE_SIZE: usize = 400;

/// 复刻 `37665.e()`：`limit` 解析（非正/NaN → 默认值，夹到 400）。
pub fn parse_limit(raw: Option<&str>, default: usize) -> usize {
    let parsed = raw.and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(0);
    if parsed <= 0 {
        default
    } else {
        (parsed as usize).min(MAX_PAGE_SIZE)
    }
}

fn is_boundary(item: &Value) -> bool {
    match item.get("role").and_then(Value::as_str) {
        Some("user") => true,
        Some("custom") => item.get("customType").and_then(Value::as_str) == Some("compaction"),
        _ => false,
    }
}

/// 复刻 `37665.g()`：页起点向前对齐到消息边界，页尾夹到 `end`。
///
/// 参数顺序与 pi 一致：`(messages, from, end, page_size)`——注意 pi 里
/// 第二个参数是**起点** `from`（对齐的对象），第三个才是页尾。
fn slice_page(messages: &[Value], from: usize, end: usize, page_size: usize) -> Page {
    let total = messages.len();

    // 复刻 37665 的内层对齐函数：从 `from` 往前最多退 page_size 条找 user 边界
    let align = |from: usize| -> usize {
        let clamped = from.min(total);
        if clamped == 0 {
            return 0;
        }
        if messages.get(clamped).is_some_and(is_boundary) {
            return clamped;
        }
        let floor = clamped.saturating_sub(page_size.max(1));
        let mut i = clamped;
        while i > floor {
            i -= 1;
            if messages.get(i).is_some_and(is_boundary) {
                return i;
            }
        }
        clamped
    };

    let start = align(from);
    let stop = start.max(end.min(total));
    Page {
        start,
        end: stop,
        total,
    }
}

/// 一页的索引区间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub start: usize,
    pub end: usize,
    pub total: usize,
}

impl Page {
    pub fn has_earlier(&self) -> bool {
        self.start > 0
    }
    pub fn start_index(&self) -> usize {
        self.start
    }
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 复刻 `37665.h()`：取最新一页。
pub fn page_tail(messages: &[Value], limit: Option<&str>) -> Page {
    let page_size = parse_limit(limit, DEFAULT_PAGE_SIZE);
    let total = messages.len();
    slice_page(messages, total.saturating_sub(page_size), total, page_size)
}

/// 复刻 `37665.i()`：取到某个条目为止的一页。
pub fn page_until(
    messages: &[Value],
    entry_ids: &[String],
    entry_id: &str,
    limit: Option<&str>,
) -> Page {
    let total = messages.len();
    let Some(index) = entry_ids.iter().position(|id| id == entry_id) else {
        return Page {
            start: 0,
            end: 0,
            total,
        };
    };
    let page_size = parse_limit(limit, DEFAULT_PAGE_SIZE);
    slice_page(
        messages,
        index.saturating_sub(page_size),
        index + 1,
        page_size,
    )
}

// ---------------------------------------------------------- totalActiveMs

/// 复刻 `89691.c()`：把「user / bashExecution → 下一条」的间隔累加为活跃时长。
pub fn total_active_ms(entries: &[Value]) -> i64 {
    let mut total = 0i64;
    let mut pending: Option<i64> = None;
    for entry in entries {
        let ty = entry.get("type").and_then(Value::as_str).unwrap_or("");
        if !matches!(
            ty,
            "message" | "compaction" | "branch_summary" | "custom_message"
        ) {
            continue;
        }
        let Some(ts) = entry.get("timestamp").and_then(|v| match v {
            Value::String(iso) => toiler_session::iso_to_ms(iso),
            Value::Number(n) => n.as_i64(),
            _ => None,
        }) else {
            continue;
        };
        let role = if ty == "message" {
            entry
                .get("message")
                .and_then(|m| m.get("role"))
                .and_then(Value::as_str)
        } else {
            None
        };
        if matches!(role, Some("user") | Some("bashExecution")) {
            pending = Some(ts);
            continue;
        }
        if let Some(start) = pending {
            if ts > start {
                total += ts - start;
            }
        }
        pending = Some(ts);
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, parent: Option<&str>, role: &str, text: &str, ts: &str) -> Value {
        json!({
            "type": "message",
            "id": id,
            "parentId": parent,
            "timestamp": ts,
            "message": { "role": role, "content": [{ "type": "text", "text": text }] }
        })
    }

    #[test]
    fn preview_truncates_like_pi() {
        let long = msg(
            "a",
            None,
            "user",
            &"x".repeat(80),
            "2026-01-01T00:00:00.000Z",
        );
        let p = branch_preview(&long).unwrap();
        let text = p.get("text").and_then(Value::as_str).unwrap();
        assert!(text.ends_with('…'), "{text}");
        assert_eq!(text.chars().count(), 41);
        assert_eq!(p.get("role").and_then(Value::as_str), Some("user"));
    }

    #[test]
    fn preview_skips_system_and_falls_back() {
        let sys = msg("a", None, "system", "x", "2026-01-01T00:00:00.000Z");
        assert!(branch_preview(&sys).is_none());

        let empty = msg("b", None, "assistant", "", "2026-01-01T00:00:00.000Z");
        let p = branch_preview(&empty).unwrap();
        assert_eq!(p.get("text").and_then(Value::as_str), Some("[assistant]"));
    }

    #[test]
    fn first_message_text_picks_first_user() {
        let entries = vec![
            msg("a", None, "system", "sys", "2026-01-01T00:00:00.000Z"),
            msg(
                "b",
                Some("a"),
                "user",
                "  你好  ",
                "2026-01-01T00:00:01.000Z",
            ),
        ];
        assert_eq!(first_message_text(&entries), "  你好  ");
        assert_eq!(first_message_text(&[]), "(no messages)");
    }

    #[test]
    fn tree_groups_children_and_orders_by_time() {
        let entries = vec![
            msg("a", None, "user", "1", "2026-01-01T00:00:00.000Z"),
            msg("b", Some("a"), "assistant", "2", "2026-01-01T00:00:02.000Z"),
            msg("c", Some("a"), "user", "3", "2026-01-01T00:00:01.000Z"),
        ];
        let tree = build_tree(&entries, &[]);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].children.len(), 2);
        // 子节点按时间升序
        assert_eq!(
            tree[0].children[0].entry.get("id").and_then(Value::as_str),
            Some("c")
        );
    }

    #[test]
    fn tree_projection_keeps_shape() {
        let entries = vec![
            msg("a", None, "user", "1", "2026-01-01T00:00:00.000Z"),
            msg("b", Some("a"), "assistant", "2", "2026-01-01T00:00:01.000Z"),
        ];
        let projected = tree_projection(&build_tree(&entries, &[]));
        let arr = projected.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let root = &arr[0];
        assert_eq!(
            root.get("entry")
                .and_then(|e| e.get("id"))
                .and_then(Value::as_str),
            Some("a")
        );
        assert_eq!(
            root.get("branchPreview")
                .and_then(|p| p.get("text"))
                .and_then(Value::as_str),
            Some("1")
        );
        assert_eq!(
            root.get("children")
                .and_then(Value::as_array)
                .unwrap()
                .len(),
            1
        );
    }

    fn boundary(role: &str, i: usize) -> Value {
        json!({ "role": role, "customType": Value::Null, "i": i })
    }

    /// 真实会话每几轮就有一条 user 消息，页边界对齐到 user。
    fn conversation(turns: usize) -> Vec<Value> {
        let mut messages = Vec::new();
        for t in 0..turns {
            messages.push(boundary("user", t * 3));
            messages.push(boundary("assistant", t * 3 + 1));
            messages.push(boundary("toolResult", t * 3 + 2));
        }
        messages
    }

    #[test]
    fn paging_takes_last_page_and_marks_earlier() {
        let messages = conversation(100); // 300 条
        let page = page_tail(&messages, None);
        assert_eq!(page.total, 300);
        assert!(page.has_earlier());
        assert_eq!(page.end, 300);
        assert!(!page.is_empty(), "最新一页不能为空");
        // 起点必须落在 user 边界上（页大小 120 → 对齐到最近的 user）
        assert_eq!(
            messages[page.start].get("role").and_then(Value::as_str),
            Some("user")
        );
        assert!(page.len() <= DEFAULT_PAGE_SIZE + 2);
    }

    #[test]
    fn paging_page_until_stops_at_entry() {
        let messages = conversation(100);
        let ids: Vec<String> = messages
            .iter()
            .map(|m| format!("e{}", m["i"].as_u64().unwrap()))
            .collect();
        let page = page_until(&messages, &ids, "e150", None);
        assert_eq!(page.end, 151);
        assert!(!page.is_empty());
        assert!(page.has_earlier());
        let missing = page_until(&messages, &ids, "nope", None);
        assert_eq!(missing.end, 0);
    }

    #[test]
    fn paging_without_boundary_window_returns_whole_window() {
        // 极端构造：全篇只有一条 user，pi 的对齐会得到空页
        let mut messages = vec![boundary("user", 0)];
        for i in 1..300 {
            messages.push(boundary("assistant", i));
        }
        let page = page_tail(&messages, None);
        // 找不到 user 边界时，起点保持不动（退化为定长窗口）
        assert_eq!(page.start, 300 - DEFAULT_PAGE_SIZE);
        assert_eq!(page.end, 300);
    }

    #[test]
    fn paging_respects_limit_bounds() {
        assert_eq!(parse_limit(None, 120), 120);
        assert_eq!(parse_limit(Some("0"), 120), 120);
        assert_eq!(parse_limit(Some("abc"), 120), 120);
        assert_eq!(parse_limit(Some("9999"), 120), MAX_PAGE_SIZE);
        assert_eq!(parse_limit(Some("5"), 120), 5);
    }

    /// pi 的语义：`user`/`bashExecution` 之后每个条目都累加与上一个条目的间隔。
    #[test]
    fn total_active_ms_sums_gaps_after_user() {
        let entries = vec![
            msg("a", None, "user", "1", "2026-01-01T00:00:00.000Z"),
            msg("b", Some("a"), "assistant", "2", "2026-01-01T00:00:02.500Z"),
            msg("c", Some("b"), "toolResult", "", "2026-01-01T00:00:03.000Z"),
        ];
        assert_eq!(total_active_ms(&entries), 3000);
    }
}

// ---------------------------------------------------------------------------
// context 投影（复刻 pi 的 `40402.Uv` / `buildContextEntries`）
// ---------------------------------------------------------------------------

/// `/api/sessions/{id}` 与 `/api/sessions/{id}/context` 的 `context` 字段。
#[derive(Debug, Clone, Default)]
pub struct ContextView {
    pub messages: Vec<Value>,
    pub entry_ids: Vec<String>,
    /// `thinking_level_change` 条目的最后一个值
    pub thinking_level: Value,
    /// `model_change` 条目的最后一个值（`{provider, modelId}`）
    pub model: Value,
}

/// 取从根到 `leaf_id` 的那条分支（默认最后一条）。
///
/// 没有 `parentId` 的条目是根；找不到叶子时退化成「全部条目按原顺序」。
pub fn branch_entries<'a>(entries: &'a [Value], leaf_id: Option<&str>) -> Vec<&'a Value> {
    let by_id: std::collections::HashMap<&str, &Value> = entries
        .iter()
        .filter_map(|e| e.get("id").and_then(Value::as_str).map(|id| (id, e)))
        .collect();

    let leaf = match leaf_id.filter(|id| by_id.contains_key(id)) {
        Some(id) => Some(id),
        None => entries
            .iter()
            .filter_map(|e| e.get("id").and_then(Value::as_str))
            .next_back(),
    };
    let Some(leaf) = leaf else {
        return Vec::new();
    };

    let mut chain: Vec<&Value> = Vec::new();
    let mut cursor = Some(leaf);
    let mut guard = 0usize;
    while let Some(id) = cursor {
        guard += 1;
        if guard > entries.len() + 1 {
            break;
        }
        let Some(entry) = by_id.get(id).copied() else {
            break;
        };
        chain.push(entry);
        cursor = entry.get("parentId").and_then(Value::as_str);
    }
    chain.reverse();
    chain
}

fn entry_timestamp(entry: &Value) -> Value {
    entry
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(toiler_session::iso_to_ms)
        .map(Value::from)
        .unwrap_or(Value::Null)
}

/// 一条条目 → context 里的一条消息（pi 的 `Uv` 内层映射）。
///
/// - `message`：系统消息丢掉，其余归一化后返回（见 [`normalize_tool_calls`]）
/// - `compaction` → `role:"custom"` + `customType:"compaction"`
/// - `branch_summary` → 一段 user 提示
/// - `custom`（`custom_message`）→ `role:"custom"` + 原 `customType`
pub fn context_entry_message(entry: &Value) -> Option<Value> {
    match entry.get("type").and_then(Value::as_str) {
        Some("message") => {
            let message = entry.get("message")?;
            if message.get("role").and_then(Value::as_str) == Some("system") {
                return None;
            }
            Some(normalize_tool_calls(message))
        }
        Some("compaction") => {
            let summary = entry.get("summary").cloned().unwrap_or(Value::Null);
            let mut details = Map::new();
            if let Some(tokens) = entry.get("tokensBefore") {
                details.insert("tokensBefore".into(), tokens.clone());
            }
            if let Some(first) = entry.get("firstKeptEntryId") {
                details.insert("firstKeptEntryId".into(), first.clone());
            }
            Some(json!({
                "role": "custom",
                "customType": "compaction",
                "content": summary,
                "display": true,
                "details": Value::Object(details),
                "timestamp": entry_timestamp(entry),
            }))
        }
        Some("branch_summary") => {
            let summary = entry.get("summary").and_then(Value::as_str)?;
            if summary.is_empty() {
                return None;
            }
            Some(json!({
                "role": "user",
                "content": format!(
                    "*The conversation briefly explored another branch and returned with this summary:*\n\n{summary}"
                ),
                "timestamp": entry_timestamp(entry),
            }))
        }
        Some("custom_message") | Some("custom") => {
            let custom_type = entry
                .get("customType")
                .and_then(Value::as_str)
                .unwrap_or("custom");
            Some(json!({
                "role": "custom",
                "customType": custom_type,
                "content": entry.get("content").cloned().unwrap_or(Value::Null),
                "display": entry.get("display").cloned().unwrap_or(Value::Bool(false)),
                "details": entry.get("details").cloned().unwrap_or(Value::Null),
                "timestamp": entry_timestamp(entry),
            }))
        }
        _ => None,
    }
}

/// assistant 消息里的 `toolCall` 块归一化（pi 的 `lib/normalize.ts`）。
///
/// v3 文件里用的是 `{id, name, arguments}`，而前端（ChatWindow、TurnWrittenFiles、
/// ToolCall 渲染）读的是 `{toolCallId, toolName, input}`。不归一化时前端拿到
/// `undefined`，`extractTurnWrittenFiles` 里的 `isFileWritingToolName(undefined)`
/// 会直接 `TypeError`，整页白屏 —— 这是阶段 5 走查抓到的真实回归。
///
/// 兼容两种形状：已归一化的原样返回（幂等）；非 assistant 消息原样返回。
pub fn normalize_tool_calls(message: &Value) -> Value {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return message.clone();
    }
    let Some(blocks) = message.get("content").and_then(Value::as_array) else {
        return message.clone();
    };
    let normalized: Vec<Value> = blocks
        .iter()
        .map(|block| {
            if block.get("type").and_then(Value::as_str) != Some("toolCall") {
                return block.clone();
            }
            let tool_call_id = block
                .get("toolCallId")
                .or_else(|| block.get("id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let tool_name = block
                .get("toolName")
                .or_else(|| block.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let input = block
                .get("input")
                .filter(|v| v.is_object())
                .or_else(|| block.get("arguments").filter(|v| v.is_object()))
                .cloned()
                .unwrap_or_else(|| json!({}));
            json!({
                "type": "toolCall",
                "toolCallId": tool_call_id,
                "toolName": tool_name,
                "input": input,
            })
        })
        .collect();

    let mut out = message.clone();
    if let Some(map) = out.as_object_mut() {
        map.insert("content".into(), Value::Array(normalized));
    }
    out
}

/// `deferThinking`：把 assistant 里非空的 thinking 正文清空并标记 `deferred`。
///
/// 前端靠 `deferred` 决定是否显示「展开思考」入口，点开后单独拉全文；
/// 不标记的话它会以为这是一段本来就为空的思考。
pub fn defer_thinking_blocks(message: &Value) -> Value {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return message.clone();
    }
    let Some(blocks) = message.get("content").and_then(Value::as_array) else {
        return message.clone();
    };
    let mut changed = false;
    let mapped: Vec<Value> = blocks
        .iter()
        .map(|block| {
            if block.get("type").and_then(Value::as_str) != Some("thinking") {
                return block.clone();
            }
            let text = block
                .get("thinking")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if text.trim().is_empty() {
                return block.clone();
            }
            changed = true;
            let mut next = block.clone();
            if let Some(map) = next.as_object_mut() {
                map.insert("thinking".into(), Value::String(String::new()));
                map.insert("deferred".into(), Value::Bool(true));
            }
            next
        })
        .collect();
    if !changed {
        return message.clone();
    }
    let mut out = message.clone();
    if let Some(map) = out.as_object_mut() {
        map.insert("content".into(), Value::Array(mapped));
    }
    out
}

/// `deferMedia`：把图片块换成一个占位文本块，避免把 base64 正文塞进首帧。
///
/// 前端首屏一直带 `deferMedia=1`（`useAgentSession.ts` 的 `loadSession`），
/// 而这条参数曾经是空实现：带不带返回的字节数完全一样，一条会话里几张图片就能
/// 把首帧撑到 1.3MB（实测）。pi 的语义是把 `{type:"image"}` 换掉，这里是同一件事：
/// 图片块 → 一行 `[image: <mime>, <大小>]` 文本，其它块原样保留。
///
/// 两种形状都认（v3 文件里两种都存在，前端的 `imageSource()` 也是这么兼容的）：
///
/// - 嵌套：`{type:"image", source:{type:"base64", media_type, data}}`
/// - 扁平：`{type:"image", data, mimeType}`
pub fn defer_media_blocks(message: &Value) -> Value {
    let Some(blocks) = message.get("content").and_then(Value::as_array) else {
        return message.clone();
    };
    let mut changed = false;
    let mapped: Vec<Value> = blocks
        .iter()
        .map(|block| {
            if block.get("type").and_then(Value::as_str) != Some("image") {
                return block.clone();
            }
            changed = true;
            let source = block.get("source");
            let mime = source
                .and_then(|s| s.get("media_type"))
                .or_else(|| block.get("mimeType"))
                .and_then(Value::as_str)
                .unwrap_or("image");
            let data = source
                .and_then(|s| s.get("data"))
                .or_else(|| block.get("data"))
                .and_then(Value::as_str);
            let detail = match (source.and_then(|s| s.get("url")), data) {
                // base64 长度 ×3/4 ≈ 原始字节数，只用于提示，不参与判定。
                (_, Some(encoded)) => {
                    format!("[image: {mime}, ~{} KB]", encoded.len() * 3 / 4 / 1024)
                }
                _ => format!("[image: {mime}, url]"),
            };
            json!({ "type": "text", "text": detail })
        })
        .collect();
    if !changed {
        return message.clone();
    }
    let mut out = message.clone();
    if let Some(map) = out.as_object_mut() {
        map.insert("content".into(), Value::Array(mapped));
    }
    out
}

/// 构建 context（消息 + entryIds + 当前模型 / 思考等级）。
pub fn context_view(entries: &[Value], leaf_id: Option<&str>) -> ContextView {
    let branch = branch_entries(entries, leaf_id);
    let mut view = ContextView::default();
    for entry in &branch {
        match entry.get("type").and_then(Value::as_str) {
            Some("model_change") => {
                let provider = entry.get("provider").cloned().unwrap_or(Value::Null);
                let model_id = entry.get("modelId").cloned().unwrap_or(Value::Null);
                view.model = json!({ "provider": provider, "modelId": model_id });
            }
            Some("thinking_level_change") => {
                if let Some(level) = entry.get("thinkingLevel") {
                    view.thinking_level = level.clone();
                }
            }
            _ => {}
        }
        if let Some(message) = context_entry_message(entry) {
            if let Some(id) = entry.get("id").and_then(Value::as_str) {
                view.entry_ids.push(id.to_string());
            }
            view.messages.push(message);
        }
    }
    view
}

/// 会话当前的叶子 id（最后一条带 id 的条目）。
pub fn leaf_id(entries: &[Value]) -> Option<String> {
    entries
        .iter()
        .filter_map(|e| e.get("id").and_then(Value::as_str))
        .next_back()
        .map(str::to_string)
}

#[cfg(test)]
mod normalize_tests {
    use super::*;

    #[test]
    fn tool_call_blocks_are_normalized_to_the_ui_shape() {
        // v3 原生形状（name/arguments/id）→ 前端读的（toolName/input/toolCallId）
        let message = json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "先跑一下"},
                {"type": "toolCall", "id": "call_1", "name": "bash", "arguments": {"command": "ls"}},
            ],
        });
        let out = normalize_tool_calls(&message);
        let blocks = out["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "text", "非 toolCall 块原样保留");
        assert_eq!(blocks[1]["toolCallId"], "call_1");
        assert_eq!(blocks[1]["toolName"], "bash");
        assert_eq!(blocks[1]["input"]["command"], "ls");
    }

    #[test]
    fn tool_call_normalization_is_idempotent() {
        let already = json!({
            "role": "assistant",
            "content": [{"type": "toolCall", "toolCallId": "c1", "toolName": "read", "input": {"path": "/a"}}],
        });
        assert_eq!(normalize_tool_calls(&already), already);
    }

    #[test]
    fn non_assistant_messages_pass_through() {
        let user = json!({"role": "user", "content": [{"type": "text", "text": "hi"}]});
        assert_eq!(normalize_tool_calls(&user), user);
        let result = json!({
            "role": "toolResult",
            "toolCallId": "c1",
            "toolName": "bash",
            "content": [{"type": "text", "text": "ok"}],
        });
        assert_eq!(
            normalize_tool_calls(&result),
            result,
            "toolResult 不需要改名（本来就是 toolName）"
        );
    }

    #[test]
    fn missing_tool_fields_become_empty_defaults() {
        let broken = json!({
            "role": "assistant",
            "content": [{"type": "toolCall"}],
        });
        let out = normalize_tool_calls(&broken);
        assert_eq!(out["content"][0]["toolCallId"], "");
        assert_eq!(out["content"][0]["toolName"], "");
        assert_eq!(out["content"][0]["input"], json!({}));
    }

    #[test]
    fn defer_thinking_clears_non_empty_thinking() {
        let message = json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "一大段推理"},
                {"type": "thinking", "thinking": "   "},
                {"type": "text", "text": "答案"},
            ],
        });
        let out = defer_thinking_blocks(&message);
        assert_eq!(out["content"][0]["thinking"], "");
        assert_eq!(out["content"][0]["deferred"], true);
        // 空白 thinking 不动
        assert!(out["content"][1].get("deferred").is_none());
        assert_eq!(out["content"][2]["text"], "答案");
    }

    #[test]
    fn defer_thinking_leaves_messages_without_thinking_untouched() {
        let message = json!({"role": "assistant", "content": [{"type": "text", "text": "x"}]});
        assert_eq!(defer_thinking_blocks(&message), message);
    }

    #[test]
    fn system_messages_are_not_part_of_the_context() {
        let entry = json!({
            "type": "message",
            "id": "m1",
            "message": {"role": "system", "content": [{"type": "text", "text": "prompt"}]},
        });
        assert!(context_entry_message(&entry).is_none());
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;

    fn entry(id: &str, parent: Option<&str>, ty: &str) -> Value {
        let mut value = json!({
            "type": ty,
            "id": id,
            "timestamp": "2026-01-01T00:00:00.000Z",
        });
        if let Some(parent) = parent {
            value["parentId"] = json!(parent);
        }
        value
    }

    #[test]
    fn branch_walks_parent_chain() {
        let entries = vec![
            entry("a", None, "message"),
            entry("b", Some("a"), "message"),
            entry("c", Some("a"), "message"),
            entry("d", Some("c"), "message"),
        ];
        let ids: Vec<&str> = branch_entries(&entries, None)
            .iter()
            .map(|e| e["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["a", "c", "d"]);
        let ids: Vec<&str> = branch_entries(&entries, Some("b"))
            .iter()
            .map(|e| e["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn context_maps_compaction_and_summary() {
        let mut compaction = entry("c", None, "compaction");
        compaction["summary"] = json!("摘要");
        compaction["tokensBefore"] = json!(1234);
        let message = context_entry_message(&compaction).unwrap();
        assert_eq!(message["role"], "custom");
        assert_eq!(message["customType"], "compaction");
        assert_eq!(message["details"]["tokensBefore"], 1234);

        let mut summary = entry("s", None, "branch_summary");
        summary["summary"] = json!("另一条分支");
        let message = context_entry_message(&summary).unwrap();
        assert_eq!(message["role"], "user");
        assert!(message["content"].as_str().unwrap().contains("另一条分支"));
    }

    #[test]
    fn context_view_collects_model_and_thinking() {
        let mut model_change = entry("m", None, "model_change");
        model_change["provider"] = json!("qoder");
        model_change["modelId"] = json!("GLM-5.3");
        let mut thinking = entry("t", Some("m"), "thinking_level_change");
        thinking["thinkingLevel"] = json!("high");
        let mut message = entry("u", Some("t"), "message");
        message["message"] = json!({"role": "user", "content": [{"type": "text", "text": "hi"}]});

        let view = context_view(&[model_change, thinking, message], None);
        assert_eq!(view.messages.len(), 1);
        assert_eq!(view.entry_ids, vec!["u"]);
        assert_eq!(view.model["provider"], "qoder");
        assert_eq!(view.thinking_level, "high");
    }

    /// `deferMedia` 必须真的把图片块换掉：前端首屏一直带这条参数，不生效的话
    /// 一条有图的会话首帧能到 MB 级（实测 1.38MB → 214KB）。
    /// 两种形状都要认：v3 文件里既有嵌套 `source`、也有扁平的 `data`/`mimeType`。
    #[test]
    fn defer_media_replaces_image_blocks_in_both_shapes() {
        let nested = json!({
            "role": "user",
            "content": [
                { "type": "text", "text": "看这张图" },
                {
                    "type": "image",
                    "source": { "type": "base64", "media_type": "image/png", "data": "A".repeat(4096) }
                }
            ]
        });
        let out = defer_media_blocks(&nested);
        let blocks = out["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1]["type"], "text");
        let text = blocks[1]["text"].as_str().unwrap();
        assert!(text.starts_with("[image: image/png"), "{text}");

        let flat = json!({
            "role": "toolResult",
            "content": [
                { "type": "image", "data": "B".repeat(2048), "mimeType": "image/jpeg" }
            ]
        });
        let out = defer_media_blocks(&flat);
        let text = out["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("[image: image/jpeg"), "{text}");
        assert!(
            !out["content"][0].to_string().contains("BBBB"),
            "正文不能被带出来"
        );

        // 纯文本消息原样返回（不能平白改写 content）。
        let plain = json!({"role": "user", "content": [{"type": "text", "text": "hi"}]});
        assert_eq!(defer_media_blocks(&plain), plain);
    }

    #[test]
    fn system_messages_are_dropped() {
        let mut system = entry("s", None, "message");
        system["message"] = json!({"role": "system", "content": "x"});
        assert!(context_entry_message(&system).is_none());
    }
}
