//! `/api/sessions/{id}` 的补充 route（阶段 3 补齐）。
//!
//! | route | 行为 |
//! |---|---|
//! | `GET  /api/sessions/{id}/context` | 只取 context + paging（形状与 detail 的 `context` 一致） |
//! | `GET  /api/sessions/{id}/export` | 导出单文件 HTML（转写稿） |
//! | `GET  /api/sessions/{id}/entries/{entryId}/thinking?blockIndex=N` | 取某个 thinking 块的原文 |
//! | `POST /api/sessions/{id}/auto-name` | 让模型给会话起标题并写回 `session_info` |
//!
//! 与 pi 的差异（`docs/contracts/README.md` 有记录）：
//!
//! - 导出：pi 用 `pi` CLI 生成一份带树状导航的 HTML；workman 自己渲染一份
//!   线性转写稿（同一套 header / 下载文件名规则，内容更朴素）。

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::security::Trusted;
use crate::session_view;
use crate::sessions_store::{read_session, RawSession};
use crate::state::AppState;

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

#[derive(Debug, Deserialize, Default)]
pub struct ContextParams {
    #[serde(rename = "leafId")]
    leaf_id: Option<String>,
    before: Option<String>,
    limit: Option<String>,
}

/// `GET /api/sessions/{id}/context`
pub async fn context(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(id): Path<String>,
    Query(params): Query<ContextParams>,
) -> Response {
    let Some(path) = state.sessions().path_of(&id) else {
        return error(StatusCode::NOT_FOUND, "Session not found");
    };
    let Ok(session) = read_session(&path) else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "读会话失败");
    };

    let view = session_view::context_view(&session.entries, params.leaf_id.as_deref());
    let page = match params.before.as_deref().filter(|b| !b.is_empty()) {
        Some(before) => session_view::page_until(
            &view.messages,
            &view.entry_ids,
            before,
            params.limit.as_deref(),
        ),
        None => session_view::page_tail(&view.messages, params.limit.as_deref()),
    };
    let messages: Vec<Value> = view.messages[page.start..page.end].to_vec();
    let entry_ids: Vec<String> = view.entry_ids
        [page.start.min(view.entry_ids.len())..page.end.min(view.entry_ids.len())]
        .to_vec();

    Json(json!({
        "context": {
            "messages": messages,
            "entryIds": entry_ids,
            "thinkingLevel": view.thinking_level,
            "model": view.model,
        },
        "paging": {
            "hasEarlier": page.has_earlier(),
            "totalMessages": page.total,
            "startIndex": page.start_index(),
            "pageSize": session_view::parse_limit(
                params.limit.as_deref(),
                session_view::DEFAULT_PAGE_SIZE,
            ),
        },
    }))
    .into_response()
}

#[derive(Debug, Deserialize, Default)]
pub struct ThinkingQuery {
    #[serde(rename = "blockIndex")]
    block_index: Option<String>,
}

/// `GET /api/sessions/{id}/entries/{entryId}/thinking?blockIndex=N`
pub async fn thinking(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path((id, entry_id)): Path<(String, String)>,
    Query(query): Query<ThinkingQuery>,
) -> Response {
    let Some(index) = query
        .block_index
        .as_deref()
        .and_then(|raw| raw.parse::<usize>().ok())
    else {
        return error(StatusCode::BAD_REQUEST, "Valid blockIndex is required");
    };
    let Some(path) = state.sessions().path_of(&id) else {
        return error(StatusCode::NOT_FOUND, "Session not found");
    };
    let Ok(session) = read_session(&path) else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "读会话失败");
    };
    let Some(entry) = session
        .entries
        .iter()
        .find(|e| e.get("id").and_then(Value::as_str) == Some(entry_id.as_str()))
    else {
        return error(StatusCode::NOT_FOUND, "Assistant message not found");
    };
    let Some(message) = entry.get("message").filter(|_| {
        entry.get("type").and_then(Value::as_str) == Some("message")
            && entry
                .get("message")
                .and_then(|m| m.get("role"))
                .and_then(Value::as_str)
                == Some("assistant")
    }) else {
        return error(StatusCode::NOT_FOUND, "Assistant message not found");
    };
    let Some(block) = message
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.get(index))
    else {
        return error(StatusCode::NOT_FOUND, "Thinking block not found");
    };
    if block.get("type").and_then(Value::as_str) != Some("thinking") {
        return error(StatusCode::NOT_FOUND, "Thinking block not found");
    }
    Json(json!({ "thinking": block.get("thinking").cloned().unwrap_or(Value::Null) }))
        .into_response()
}

#[derive(Debug, Deserialize, Default)]
pub struct ExportParams {
    inline: Option<String>,
    desktop: Option<String>,
    #[serde(rename = "returnTo")]
    return_to: Option<String>,
}

/// `GET /api/sessions/{id}/export` —— 下载一份自包含的 HTML。
pub async fn export(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(id): Path<String>,
    Query(params): Query<ExportParams>,
) -> Response {
    let Some(path) = state.sessions().path_of(&id) else {
        return error(StatusCode::NOT_FOUND, "Session not found");
    };
    let Ok(session) = read_session(&path) else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "读会话失败");
    };

    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| id.clone());
    let filename = format!("pi-session-{stem}.html");
    let ascii = sanitize_ascii_filename(&filename);
    let disposition = format!(
        "{}; filename=\"{}\"; filename*=UTF-8''{}",
        if params.inline.as_deref() == Some("1") {
            "inline"
        } else {
            "attachment"
        },
        ascii,
        percent_encode(&filename)
    );

    let mut html = render_export_html(&session, &id);
    if params.desktop.as_deref() == Some("1") {
        let target = params
            .return_to
            .as_deref()
            .and_then(allowed_return_to)
            .map(|url| format!("location.replace({})", json!(url)))
            .unwrap_or_else(|| "history.back()".to_string());
        let script = format!(
            "<script>document.addEventListener(\"keydown\",function(event){{if(event.key===\"Escape\"){target}}})</script>"
        );
        if let Some(pos) = html.rfind("</body>") {
            html.insert_str(pos, &script);
        }
    }

    let mut response = Response::new(axum::body::Body::from(html));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    if let Ok(value) = axum::http::HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        axum::http::HeaderValue::from_static("frame-ancestors 'none'"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        axum::http::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::X_FRAME_OPTIONS,
        axum::http::HeaderValue::from_static("DENY"),
    );
    response
}

/// 只允许 tauri / 127.0.0.1 的 returnTo（pi 的同名白名单）。
fn allowed_return_to(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw).ok()?;
    let ok = (url.scheme() == "tauri" && url.host_str() == Some("localhost"))
        || (url.scheme() == "http" && url.host_str() == Some("127.0.0.1"));
    ok.then(|| url.to_string())
}

fn sanitize_ascii_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && !matches!(c, '"' | '\\' | ';') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.trim_matches('_').is_empty() {
        "session.html".to_string()
    } else {
        cleaned
    }
}

fn percent_encode(name: &str) -> String {
    let mut out = String::new();
    for byte in name.as_bytes() {
        let ch = *byte as char;
        let unreserved = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_' | '~');
        if unreserved {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// 把一条 content 块渲染成 HTML 片段。
fn render_blocks(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return format!("<p>{}</p>", escape_html(text));
    }
    let Some(blocks) = content.as_array() else {
        return String::new();
    };
    let mut out = String::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                out.push_str(&format!("<p>{}</p>", escape_html(text)));
            }
            Some("thinking") => {
                let text = block.get("thinking").and_then(Value::as_str).unwrap_or("");
                if !text.trim().is_empty() {
                    out.push_str(&format!(
                        "<details class=\"thinking\"><summary>thinking</summary><pre>{}</pre></details>",
                        escape_html(text)
                    ));
                }
            }
            Some("toolCall") => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                let args = block.get("arguments").cloned().unwrap_or(Value::Null);
                let pretty = serde_json::to_string_pretty(&args).unwrap_or_default();
                out.push_str(&format!(
                    "<details class=\"tool\"><summary>🔧 {}</summary><pre>{}</pre></details>",
                    escape_html(name),
                    escape_html(&pretty)
                ));
            }
            Some("image") => out.push_str("<p class=\"media\">[image]</p>"),
            _ => {}
        }
    }
    out
}

fn render_export_html(session: &RawSession, id: &str) -> String {
    let name = session.name().unwrap_or_else(|| id.to_string());
    let cwd = session.header.cwd.clone();
    let view = session_view::context_view(&session.entries, None);
    let mut body = String::new();
    for message in &view.messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let (label, class) = match role {
            "user" => ("User", "user"),
            "assistant" => ("Assistant", "assistant"),
            "toolResult" => ("Tool", "tool"),
            "custom" => ("Custom", "custom"),
            _ => (role, "other"),
        };
        body.push_str(&format!(
            "<section class=\"msg {}\"><h2>{}</h2>{}</section>",
            class,
            escape_html(label),
            render_blocks(message.get("content"))
        ));
    }
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>{title}</title>\
<meta name=\"generator\" content=\"workman\">\
<style>{style}</style></head><body>\
<header><h1>{title}</h1><p class=\"meta\">{cwd} · {count} messages</p></header>\
<main>{body}</main></body></html>\n",
        title = escape_html(&name),
        cwd = escape_html(&cwd),
        count = view.messages.len(),
        body = body,
        style = "body{font:16px/1.6 -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;\
margin:0 auto;max-width:820px;padding:32px 20px;background:#fafaf9;color:#18181b}\
header{border-bottom:1px solid #e4e4e7;margin-bottom:24px}\
h1{font-size:22px;margin:0 0 4px}.meta{color:#71717a;font-size:13px;margin:0 0 20px}\
.msg{border-radius:12px;padding:12px 16px;margin:0 0 16px;background:#fff;\
box-shadow:0 1px 2px rgba(0,0,0,.06)}.msg h2{font-size:12px;text-transform:uppercase;\
letter-spacing:.08em;color:#71717a;margin:0 0 8px}.msg.user{background:#eff6ff}\
.msg.tool{background:#fefce8}details{margin:8px 0}summary{cursor:pointer;color:#52525b;\
font-size:13px}pre{white-space:pre-wrap;word-break:break-word;font-size:13px;\
background:#f4f4f5;padding:8px;border-radius:8px;overflow:auto}p{margin:0 0 8px}\
p:last-child{margin-bottom:0}"
    )
}

// ---------------------------------------------------------------------------
// auto-name
// ---------------------------------------------------------------------------

/// 标题生成用的提示词（逐字取自 pi 的 `79495.i`）。
const TITLE_PROMPT: &str =
    "Create a concise title for this session based on the conversation above.

Requirements:
- Match the primary language used by the user.
- Describe the user's concrete goal or the outcome, not the act of chatting.
- Use 4-12 words for space-separated languages, or 8-24 characters for CJK text when practical.
- Do not call any tools.
- Return only the title as plain text, with no quotes, label, markdown, or explanation.";

/// `POST /api/sessions/{id}/auto-name`
pub async fn auto_name(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(id): Path<String>,
) -> Response {
    let Some(path) = state.sessions().path_of(&id) else {
        return error(StatusCode::NOT_FOUND, "Session not found");
    };
    let Ok(session) = read_session(&path) else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "读会话失败");
    };

    let view = session_view::context_view(&session.entries, None);
    let transcript = render_transcript(&view.messages);
    if transcript.trim().is_empty() {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The session has no user messages to name",
        );
    }

    // 模型：会话里最后一个 `model_change`，否则目录默认
    let (provider, model_id) = session_model(&state, &view.model);
    let Some((runtime, model)) = lookup_model(&state, &provider, &model_id) else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "No available model configured for title generation",
        );
    };
    let api_key = resolve_key(&state, &provider);
    let prompt = format!("{transcript}\n\n{TITLE_PROMPT}");

    let result = crate::oneshot::complete(
        runtime,
        &model,
        api_key,
        prompt,
        256,
        std::time::Duration::from_secs(90),
    )
    .await;
    let reply = match result {
        Ok(reply) => reply,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let title = match clean_title(&reply.text) {
        Ok(title) => title,
        Err(message) => return error(StatusCode::INTERNAL_SERVER_ERROR, &message),
    };

    // 写回会话名（pi 的 `setSessionName` → 追加 `session_info` 条目）
    let storage = match workman_session::V3SessionStorage::load(&path) {
        Ok(storage) => storage,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    {
        use rpi_harness::session::types::SessionStorage;
        if let Err(e) = storage.set_name(Some(&title)).await {
            return error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
        }
    }
    state.sessions().invalidate();

    let usage = reply.usage.map(|usage| {
        json!({
            "input": usage.input,
            "output": usage.output,
            "cacheRead": usage.cache_read,
            "cacheWrite": usage.cache_write,
            "total": usage.total_tokens,
        })
    });
    Json(json!({ "title": title, "usage": usage })).into_response()
}

fn session_model(state: &AppState, model: &Value) -> (String, String) {
    if let (Some(provider), Some(model_id)) = (
        model.get("provider").and_then(Value::as_str),
        model.get("modelId").and_then(Value::as_str),
    ) {
        if !provider.is_empty() && !model_id.is_empty() {
            return (provider.to_string(), model_id.to_string());
        }
    }
    state
        .default_model_ref()
        .unwrap_or_else(|| (String::new(), String::new()))
}

fn lookup_model(
    state: &AppState,
    provider: &str,
    model_id: &str,
) -> Option<(std::sync::Arc<dyn rpi_ai::Provider>, rpi_ai::Model)> {
    let runtime = state
        .catalog()
        .providers
        .iter()
        .find(|p| p.id() == provider)
        .cloned()?;
    let model = state.catalog().model_for(provider, model_id)?.clone();
    Some((runtime, model))
}

fn resolve_key(state: &AppState, provider: &str) -> Option<String> {
    if let Some(key) = state.auth().api_key(provider) {
        return Some(key);
    }
    let root = state.models_file().read();
    let config = crate::models_json::provider_config(&root, provider)?;
    match crate::auth::resolve_key_config(config.get("apiKey")?) {
        crate::auth::ResolvedKey::Literal(key) => Some(key),
        _ => None,
    }
}

/// 把 context 消息压成纯文本对话（只要 user / assistant 的文字）。
fn render_transcript(messages: &[Value]) -> String {
    let mut out = String::new();
    for message in messages {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        let (label, text) = match role {
            "user" => ("User", text_of(message.get("content"))),
            "assistant" => ("Assistant", text_of(message.get("content"))),
            "custom" => ("Note", text_of(message.get("content"))),
            _ => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("{label}: {text}\n\n"));
    }
    // 只保留最后一段，避免超长会话把上下文吃满
    const MAX_CHARS: usize = 12_000;
    if out.chars().count() > MAX_CHARS {
        let tail: String = out
            .chars()
            .rev()
            .take(MAX_CHARS)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        return tail;
    }
    out
}

fn text_of(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(blocks) = content.as_array() else {
        return String::new();
    };
    blocks
        .iter()
        .filter_map(|block| {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                block.get("text").and_then(Value::as_str)
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 清洗模型返回的标题（pi 的 `79495` 里那段 `function(a)`）。
pub fn clean_title(raw: &str) -> Result<String, String> {
    let mut text = raw.trim().to_string();
    if let Some(stripped) = strip_code_fence(&text) {
        text = stripped;
    }
    if text.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            if let Some(title) = value.get("title").and_then(Value::as_str) {
                text = title.trim().to_string();
            }
        }
    }
    // 只要第一行
    text = text.lines().next().unwrap_or("").to_string();
    // 去掉 `Title:` / `标题：` 前缀
    let lowered = text.to_lowercase();
    for prefix in ["session title", "title", "标题"] {
        if lowered.starts_with(prefix) {
            let rest = &text[prefix.len()..];
            let trimmed = rest.trim_start();
            if let Some(stripped) = trimmed.strip_prefix([':', '：', '-']) {
                text = stripped.trim().to_string();
            }
        }
    }
    // 去掉成对引号
    for (open, close) in [
        ('"', '"'),
        ('\'', '\''),
        ('`', '`'),
        ('“', '”'),
        ('「', '」'),
        ('『', '』'),
    ] {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() > 2 && chars[0] == open && chars[chars.len() - 1] == close {
            text = chars[1..chars.len() - 1]
                .iter()
                .collect::<String>()
                .trim()
                .to_string();
        }
    }
    text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    text = text
        .trim_end_matches(['。', '.', '!', '！'])
        .trim()
        .to_string();
    if !text.chars().any(|c| c.is_alphanumeric()) {
        return Err("The model did not return a usable session title".to_string());
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() > 80 {
        return Ok(chars[..80].iter().collect::<String>().trim().to_string());
    }
    Ok(text)
}

fn strip_code_fence(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let after_open = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```text"))
        .or_else(|| trimmed.strip_prefix("```"))?;
    let body = after_open.strip_suffix("```")?;
    Some(body.trim().to_string())
}

/// 供测试：把「分支叶子」当参数传进来（真实路径不暴露给 HTTP）。
#[allow(dead_code)]
pub fn branch_ids(entries: &[Value]) -> Vec<String> {
    let view = session_view::context_view(entries, None);
    let mut seen: HashMap<String, ()> = HashMap::new();
    view.entry_ids
        .into_iter()
        .filter(|id| seen.insert(id.clone(), ()).is_none())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_generated_titles() {
        assert_eq!(clean_title("修复登录 bug").unwrap(), "修复登录 bug");
        assert_eq!(clean_title("\"Fix login bug\"").unwrap(), "Fix login bug");
        assert_eq!(clean_title("标题：缓存方案设计。").unwrap(), "缓存方案设计");
        assert_eq!(
            clean_title("```json\n{\"title\":\"Refactor auth\"}\n```").unwrap(),
            "Refactor auth"
        );
        assert_eq!(
            clean_title("Title: Add SSE route").unwrap(),
            "Add SSE route"
        );
        assert!(clean_title("   ").is_err());
        assert!(clean_title("!!!").is_err());
    }

    #[test]
    fn truncates_long_titles() {
        let long = "a".repeat(120);
        assert_eq!(clean_title(&long).unwrap().chars().count(), 80);
    }

    #[test]
    fn sanitizes_download_names() {
        assert_eq!(
            sanitize_ascii_filename("pi-session-x.html"),
            "pi-session-x.html"
        );
        assert_eq!(sanitize_ascii_filename("名字.html"), "__.html");
        assert_eq!(percent_encode("a b.html"), "a%20b.html");
    }

    #[test]
    fn renders_transcript_text_only() {
        let messages = vec![
            json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
            json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "hmm"},
                {"type": "text", "text": "hello"}
            ]}),
            json!({"role": "toolResult", "content": "ignored"}),
        ];
        let text = render_transcript(&messages);
        assert!(text.contains("User: hi"));
        assert!(text.contains("Assistant: hello"));
        assert!(!text.contains("hmm"));
        assert!(!text.contains("ignored"));
    }
}
