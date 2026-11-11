//! `GET /api/sessions/search?q=&limit=` —— 会话全文搜索。
//!
//! 复刻 pi `8131.i()`：
//! 1. 先匹配会话的 `name` / `firstMessage` / `cwd`（命中就不读文件）
//! 2. 否则逐行读 `.jsonl`，匹配 message 的文本块与 compaction 的 summary
//! 3. snippet = 命中点前后 58 字符，两端截断加 `…`
//! 4. 结果按 `modified` 倒序，最多 `limit`（1..200，默认 80）

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::security::Trusted;
use crate::state::AppState;

const SNIPPET_RADIUS: usize = 58;

#[derive(Debug, Deserialize, Default)]
pub struct SearchParams {
    q: Option<String>,
    limit: Option<String>,
}

/// `GET /api/sessions/search`
pub async fn search(
    State(state): State<AppState>,
    _trusted: Trusted,
    Query(params): Query<SearchParams>,
) -> Response {
    let raw = params.q.unwrap_or_default();
    let needle = collapse(&raw.chars().take(120).collect::<String>()).to_lowercase();
    if needle.is_empty() {
        return no_store(json!({ "matches": [] }));
    }
    let limit = params
        .limit
        .and_then(|v| v.parse::<i64>().ok())
        .map(|v| v.clamp(1, 200) as usize)
        .unwrap_or(80);

    let mut sessions = state.sessions().list(false);
    sessions.sort_by(|a, b| {
        let am = a.get("modified").and_then(Value::as_str).unwrap_or("");
        let bm = b.get("modified").and_then(Value::as_str).unwrap_or("");
        bm.cmp(am)
    });

    let mut matches: Vec<Value> = Vec::new();
    for session in sessions.iter() {
        if matches.len() >= limit {
            break;
        }
        let Some(id) = session.get("id").and_then(Value::as_str) else {
            continue;
        };

        // 元数据先匹配
        let mut snippet = None;
        for field in ["name", "firstMessage", "cwd"] {
            if let Some(text) = session.get(field).and_then(Value::as_str) {
                if let Some(s) = make_snippet(text, &needle) {
                    snippet = Some(s);
                    break;
                }
            }
        }

        // 元数据没命中再扫文件内容
        let snippet = match snippet {
            Some(s) => s,
            None => match state
                .sessions()
                .path_of(id)
                .and_then(|p| std::fs::read_to_string(p).ok())
            {
                Some(text) => match scan_file(&text, &needle) {
                    Some(s) => s,
                    None => continue,
                },
                None => continue,
            },
        };

        matches.push(json!({ "sessionId": id, "snippet": snippet }));
    }

    no_store(json!({ "matches": matches }))
}

/// 逐行找第一条命中的可搜索文本。
fn scan_file(text: &str, needle: &str) -> Option<String> {
    for line in text.lines() {
        if !line.contains("message") && !line.contains("compaction") {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        for candidate in searchable_texts(&entry) {
            if let Some(snippet) = make_snippet(&candidate, needle) {
                return Some(snippet);
            }
        }
    }
    None
}

/// 一个条目里参与搜索的文本（pi：compaction.summary 或 message 的 text 块）。
fn searchable_texts(entry: &Value) -> Vec<String> {
    let ty = entry.get("type").and_then(Value::as_str).unwrap_or("");
    if ty == "compaction" {
        return entry
            .get("summary")
            .and_then(Value::as_str)
            .map(|s| vec![s.to_string()])
            .unwrap_or_default();
    }
    if ty != "message" {
        return Vec::new();
    }
    let Some(message) = entry.get("message") else {
        return Vec::new();
    };
    let role = message.get("role").and_then(Value::as_str).unwrap_or("");
    let is_searchable = match role {
        "user" | "assistant" => true,
        "custom" => message.get("display").and_then(Value::as_bool) != Some(false),
        _ => false,
    };
    if !is_searchable {
        return Vec::new();
    }
    match message.get("content") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter(|i| i.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|i| i.get("text").and_then(Value::as_str).map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 命中点前后各 58 个字符，两端截断时加 `…`。
fn make_snippet(text: &str, needle: &str) -> Option<String> {
    let collapsed = collapse(text);
    let haystack = collapsed.to_lowercase();
    let index = haystack.find(needle)?;

    // 按字符下标切（避免破坏 UTF-8）
    let chars: Vec<char> = collapsed.chars().collect();
    let byte_to_char =
        |byte_index: usize| collapsed[..byte_index.min(collapsed.len())].chars().count();
    let start_char = byte_to_char(index);
    let end_char = byte_to_char(index + needle.len());

    let from = start_char.saturating_sub(SNIPPET_RADIUS);
    let to = (end_char + SNIPPET_RADIUS).min(chars.len());
    let mut out = String::new();
    if from > 0 {
        out.push('…');
    }
    out.extend(&chars[from..to]);
    if to < chars.len() {
        out.push('…');
    }
    Some(out)
}

fn no_store(body: Value) -> Response {
    let mut response = Json(body).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_centers_on_match_with_ellipsis() {
        let text = format!("{}NEEDLE{}", "a".repeat(100), "b".repeat(100));
        let snippet = make_snippet(&text, "needle").unwrap();
        assert!(snippet.starts_with('…'));
        assert!(snippet.ends_with('…'));
        assert!(snippet.contains("NEEDLE"));
    }

    #[test]
    fn short_text_has_no_ellipsis() {
        let snippet = make_snippet("你好，世界", "世界").unwrap();
        assert_eq!(snippet, "你好，世界");
    }

    #[test]
    fn missing_needle_returns_none() {
        assert!(make_snippet("abcdef", "zzz").is_none());
    }

    #[test]
    fn searchable_texts_skips_hidden_custom() {
        let hidden = json!({
            "type": "message",
            "message": { "role": "custom", "display": false, "content": "secret" }
        });
        assert!(searchable_texts(&hidden).is_empty());

        let visible = json!({
            "type": "message",
            "message": { "role": "user", "content": [{ "type": "text", "text": "hello" }] }
        });
        assert_eq!(searchable_texts(&visible), vec!["hello".to_string()]);
    }

    #[test]
    fn compaction_summary_is_searchable() {
        let entry = json!({ "type": "compaction", "summary": "前面聊了缓存" });
        assert_eq!(searchable_texts(&entry), vec!["前面聊了缓存".to_string()]);
    }
}
