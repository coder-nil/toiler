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
pub async fn list(
    State(state): State<AppState>,
    _trusted: Trusted,
    Query(params): Query<ListParams>,
) -> Response {
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
    #[serde(rename = "leafId")]
    leaf_id: Option<String>,
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

    // context：pi 的 `buildContextEntries` 投影（沿分支到叶子）。
    let view = session_view::context_view(&session.entries, params.leaf_id.as_deref());
    let leaf_id = params
        .leaf_id
        .clone()
        .or_else(|| session_view::leaf_id(&session.entries))
        .unwrap_or_default();
    let page = session_view::page_tail(&view.messages, params.limit.as_deref());
    let paged_messages: Vec<Value> = view.messages[page.start..page.end]
        .iter()
        .map(|message| {
            // `deferThinking`：把 thinking 正文清空并标记 `deferred`（pi 的同一行为），
            // 前端点开时才去 `/api/sessions/{id}/entries/{entryId}/thinking` 取全文。
            let message = if params.defer_thinking.is_some() {
                session_view::defer_thinking_blocks(message)
            } else {
                message.clone()
            };
            // `deferMedia`：把图片块换成一行占位文字（pi 的同一行为）。前端首屏一直
            // 带这条参数，不实现的话几张图片就能把首帧撑到 MB 级。
            if params.defer_media.is_some() {
                session_view::defer_media_blocks(&message)
            } else {
                message
            }
        })
        .collect();
    let paged_ids: Vec<String> = view.entry_ids
        [page.start.min(view.entry_ids.len())..page.end.min(view.entry_ids.len())]
        .to_vec();

    let info = state
        .sessions()
        .list(false)
        .into_iter()
        .find(|item| item.get("id").and_then(Value::as_str) == Some(id.as_str()));

    // `deferMedia` 在上一段的 map 里已按图片块换成占位文字（见 `defer_media_blocks`）。

    Json(json!({
        "sessionId": id,
        "filePath": normalize_path(&path.to_string_lossy()),
        "info": info,
        "leafId": leaf_id,
        "tree": tree,
        "context": {
            "messages": paged_messages,
            "entryIds": paged_ids,
            "thinkingLevel": view.thinking_level,
            "model": view.model,
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

    let storage = match toiler_session::V3SessionStorage::load(&path) {
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
