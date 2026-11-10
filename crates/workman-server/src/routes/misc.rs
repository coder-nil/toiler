//! 其余核心 route：models / mobile / home / default-cwd / cwd。
//!
//! - `GET  /api/models`：模型列表（来自 `models.json`）
//! - `GET  /api/mobile/state`：手机端会话状态
//! - `GET  /api/home`：`{home}`
//! - `POST /api/default-cwd`：建目录并返回 `{cwd}`

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::security::Trusted;
use crate::session_view;
use crate::state::AppState;

/// `GET /api/models`
pub async fn models(State(state): State<AppState>, _trusted: Trusted) -> Response {
    let catalog = state.catalog();
    let entries: Vec<Value> = catalog.entries.iter().map(|e| e.to_json()).collect();
    Json(json!({
        "models": entries,
        "default": catalog.default_model().map(|m| json!({
            "provider": m.provider.as_str(),
            "id": m.id,
            "name": m.name,
        })),
        "source": catalog.source.as_ref().map(|p| p.to_string_lossy().to_string()),
    }))
    .into_response()
}

#[derive(Debug, Deserialize, Default)]
pub struct MobileParams {
    session: Option<String>,
    cwd: Option<String>,
    limit: Option<String>,
}

/// `GET /api/mobile/state`
pub async fn mobile_state(
    State(state): State<AppState>,
    _trusted: Trusted,
    Query(params): Query<MobileParams>,
) -> Response {
    let running = state.live().running();
    let sessions = state.sessions().list(false);

    let chosen: Option<Value> = if let Some(id) = params.session.as_deref() {
        sessions
            .iter()
            .find(|s| s.get("id").and_then(Value::as_str) == Some(id))
            .cloned()
    } else {
        let filtered: Vec<&Value> = match params.cwd.as_deref() {
            Some(cwd) => sessions
                .iter()
                .filter(|s| s.get("cwd").and_then(Value::as_str) == Some(cwd))
                .collect(),
            None => sessions.iter().collect(),
        };
        let pool = if filtered.is_empty() {
            sessions.iter().collect::<Vec<_>>()
        } else {
            filtered
        };
        // 优先正在运行的
        pool.iter()
            .find(|s| {
                s.get("id")
                    .and_then(Value::as_str)
                    .map(|id| running.iter().any(|r| r == id))
                    .unwrap_or(false)
            })
            .map(|s| (*s).clone())
            .or_else(|| pool.first().map(|s| (*s).clone()))
    };

    let Some(chosen) = chosen else {
        return Json(json!({
            "sessionId": null,
            "name": null,
            "cwd": params.cwd,
            "status": "idle",
            "updatedAt": null,
            "queue": { "steering": 0, "followUp": 0 },
            "pendingUiRequest": null,
            "messages": [],
            "earlierCount": 0,
        }))
        .into_response();
    };

    let id = chosen
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    // 活 agent 的排队信息 + 流式状态
    let (status, queue) = match state.live().get(&id) {
        Some(agent) => {
            let state_json = agent.state_json().await.unwrap_or(Value::Null);
            let status = if agent.is_streaming() { "running" } else { "idle" };
            (status, state_json.get("queuedMessages").cloned())
        }
        None => ("idle", None),
    };

    // 消息：复用 detail 的裁剪逻辑（取最新一页）
    let (messages, earlier_count) = match state.sessions().session(&id) {
        Some(session) => {
            let messages: Vec<Value> = session
                .entries
                .iter()
                .filter(|e| e.get("type").and_then(Value::as_str) == Some("message"))
                .filter_map(|e| e.get("message").cloned())
                .collect();
            let page = session_view::page_tail(&messages, params.limit.as_deref());
            let earlier = page.total - page.len();
            (messages[page.start..page.end].to_vec(), earlier)
        }
        None => (Vec::new(), 0),
    };

    Json(json!({
        "sessionId": id,
        "name": chosen.get("name").cloned().unwrap_or(Value::Null),
        "cwd": chosen.get("cwd").cloned().unwrap_or(Value::Null),
        "status": status,
        "updatedAt": chosen.get("modified").cloned().unwrap_or(Value::Null),
        "queue": queue.unwrap_or_else(|| json!({ "steering": 0, "followUp": 0 })),
        "pendingUiRequest": Value::Null,
        "messages": messages,
        "earlierCount": earlier_count,
    }))
    .into_response()
}

/// `GET /api/home`
pub async fn home(_trusted: Trusted) -> Response {
    let home = std::env::var("HOME").unwrap_or_default();
    Json(json!({ "home": home })).into_response()
}

/// `POST /api/default-cwd` —— 建（如需要）、登记允许根、返回 `{cwd}`。
pub async fn default_cwd(State(state): State<AppState>, _trusted: Trusted) -> Response {
    let cwd = state.default_cwd().to_path_buf();
    if let Err(e) = crate::routes::projects::ensure_default_dir(&state, &cwd) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response();
    }
    Json(json!({ "cwd": cwd.to_string_lossy() })).into_response()
}
