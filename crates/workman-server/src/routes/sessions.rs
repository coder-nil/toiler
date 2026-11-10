//! `/api/sessions` 与 `/api/sessions/[id]`。
//!
//! 契约见 `docs/contracts/README.md`；本文件逐条对应：
//! - `GET /api/sessions?force=1`
//! - `GET /api/sessions/{id}?deferThinking&deferMedia&limit=N`
//! - `PATCH /api/sessions/{id}`（改名）
//! - `DELETE /api/sessions/{id}`

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::security::Trusted;
use crate::session_view;
use crate::sessions_store::{normalize_path, read_session};
use crate::state::AppState;

#[derive(Debug, Deserialize, Default)]
pub struct ListParams {
    force: Option<String>,
}

/// `GET /api/sessions`
pub async fn list(State(state): State<AppState>, _trusted: Trusted, Query(params): Query<ListParams>) -> Response {
    let force = params.force.as_deref() == Some("1");
    let sessions = state.sessions().list(force);
    let updated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    no_store(Json(json!({
        "sessions": sessions,
        "sessionsUpdatedAt": updated_at,
    })))
}

#[derive(Debug, Deserialize, Default)]
pub struct DetailParams {
    #[serde(rename = "deferThinking")]
    defer_thinking: Option<String>,
    #[serde(rename = "deferMedia")]
    defer_media: Option<String>,
    limit: Option<String>,
}

/// `GET /api/sessions/{id}`
pub async fn detail(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(id): Path<String>,
    Query(params): Query<DetailParams>,
) -> Response {
    let Some(path) = state.sessions().path_of(&id) else {
        return error_response(StatusCode::NOT_FOUND, "Session not found");
    };
    let Ok(session) = read_session(&path) else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "读会话失败");
    };

    let labels = session.labels();
    let tree = session_view::tree_projection(&session_view::build_tree(&session.entries, &labels));

    // context：pi 的 buildSessionContext 投影。
    // `deferMedia` / `deferThinking` 在 pi 里是「省掉大字段」的开关；这里条目
    // 已经解析成 JSON，省字段需要额外遍历，阶段 2 先原样返回（形状一致）。
    let entries = &session.entries;
    let messages: Vec<Value> = entries.iter().filter_map(context_message).collect();
    let entry_ids: Vec<String> = entries
        .iter()
        .filter(|e| context_message(e).is_some())
        .filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();

    let page = session_view::page_tail(&messages, params.limit.as_deref());
    let paged_messages: Vec<Value> = messages[page.start..page.end].to_vec();
    let paged_ids: Vec<String> = entry_ids[page.start.min(entry_ids.len())..page.end.min(entry_ids.len())]
        .to_vec();

    let info = state
        .sessions()
        .list(false)
        .into_iter()
        .find(|item| item.get("id").and_then(Value::as_str) == Some(id.as_str()));

    let leaf_id = session
        .entries
        .last()
        .and_then(|e| e.get("id").and_then(Value::as_str))
        .unwrap_or_default();

    let _ = (params.defer_thinking, params.defer_media);

    Json(json!({
        "sessionId": id,
        "filePath": normalize_path(&path.to_string_lossy()),
        "info": info,
        "leafId": leaf_id,
        "tree": tree,
        "context": {
            "messages": paged_messages,
            "entryIds": paged_ids,
            "thinkingLevel": Value::Null,
            "model": Value::Null,
        },
        "paging": {
            "hasEarlier": page.has_earlier(),
            "totalMessages": page.total,
            "startIndex": page.start_index(),
            "pageSize": session_view::parse_limit(params.limit.as_deref(), session_view::DEFAULT_PAGE_SIZE),
        },
        "totalActiveMs": session_view::total_active_ms(&session.entries),
    }))
    .into_response()
}

/// 条目 → context 里的一条消息（pi 只把 message 条目放进上下文）。
fn context_message(entry: &Value) -> Option<Value> {
    if entry.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    entry.get("message").cloned()
}

#[derive(Debug, Deserialize)]
pub struct RenameBody {
    name: Value,
}

/// `PATCH /api/sessions/{id}` —— 追加一条 `session_info` 条目（pi 的做法）。
pub async fn rename(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(id): Path<String>,
    Json(body): Json<RenameBody>,
) -> Response {
    let Some(name) = body.name.as_str() else {
        return error_response(StatusCode::BAD_REQUEST, "name is required");
    };
    let Some(path) = state.sessions().path_of(&id) else {
        return error_response(StatusCode::NOT_FOUND, "Session not found");
    };

    let storage = match workman_session::V3SessionStorage::load(&path) {
        Ok(s) => s,
        Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    use rpi_harness::session::types::SessionStorage;
    if let Err(e) = storage.set_name(Some(name.trim())).await {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
    }
    state.sessions().invalidate();
    Json(json!({ "ok": true })).into_response()
}

/// `DELETE /api/sessions/{id}` —— 关掉活 agent，删文件，清索引。
pub async fn delete(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(id): Path<String>,
) -> Response {
    let Some(path) = state.sessions().path_of(&id) else {
        return error_response(StatusCode::NOT_FOUND, "Session not found");
    };
    state.live().remove(&id).await;
    if let Err(e) = std::fs::remove_file(&path) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
    }
    state.sessions().invalidate();
    Json(json!({ "ok": true })).into_response()
}

fn no_store(json: Json<Value>) -> Response {
    let mut response = json.into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

pub fn error_response(status: StatusCode, message: &str) -> Response {
    let mut body = Map::new();
    body.insert("error".into(), json!(message));
    (status, Json(Value::Object(body))).into_response()
}
