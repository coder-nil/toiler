//! `/api/agent/*` —— 命令与 SSE 事件流。
//!
//! - `POST /api/agent/{id}`：`{type:…}` → `{success:true,data}`
//! - `GET  /api/agent/{id}`：`{running}` / `{running,state}`
//! - `GET  /api/agent/{id}/events`：SSE（`connected` → agent 事件 → 关闭）
//! - `POST /api/agent/new`：建新会话
//! - `GET  /api/agent/running`、`GET /api/agent/running/events`

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::Stream;
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;

use crate::agent_runtime::LiveAgent;
use crate::security::Trusted;
use crate::state::AppState;

/// `POST /api/agent/{id}`
pub async fn command(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let command_type = body.get("type").and_then(Value::as_str).unwrap_or("");
    let is_prompt = command_type == "prompt";

    let agent = match ensure_agent(&state, &id).await {
        Ok(agent) => agent,
        Err(AgentError::NotFound) => {
            let mut payload = serde_json::Map::new();
            payload.insert("error".into(), json!("Session not found"));
            if is_prompt {
                payload.insert("code".into(), json!("prompt_rejected"));
                payload.insert("accepted".into(), json!(false));
            }
            return (StatusCode::NOT_FOUND, Json(Value::Object(payload))).into_response();
        }
        Err(AgentError::Failed(message)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": message })),
            )
                .into_response()
        }
    };

    match agent.send(&body).await {
        Ok(data) => Json(json!({ "success": true, "data": data })).into_response(),
        Err(e) => {
            let mut payload = serde_json::Map::new();
            payload.insert("error".into(), json!(e.to_string()));
            if is_prompt {
                payload.insert("code".into(), json!("prompt_rejected"));
                payload.insert("accepted".into(), json!(false));
            }
            (StatusCode::INTERNAL_SERVER_ERROR, Json(Value::Object(payload))).into_response()
        }
    }
}

/// `GET /api/agent/{id}`
pub async fn state_of(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(id): Path<String>,
) -> Response {
    match state.live().get(&id) {
        Some(agent) if agent.is_streaming() => match agent.state_json().await {
            Ok(state_json) => Json(json!({ "running": true, "state": state_json })).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response(),
        },
        _ => Json(json!({ "running": false })).into_response(),
    }
}

/// `GET /api/agent/{id}/events` —— SSE。
pub async fn events(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(id): Path<String>,
) -> Response {
    let agent = match ensure_agent(&state, &id).await {
        Ok(agent) => agent,
        Err(AgentError::NotFound) => {
            return (StatusCode::NOT_FOUND, "Session not found").into_response()
        }
        Err(AgentError::Failed(message)) => {
            // pi：先发 startup_error，再关流
            let payload = json!({
                "type": "startup_error",
                "errorMessage": format!("Failed to start agent: {message}"),
            });
            let stream = futures::stream::iter(vec![Ok::<Event, Infallible>(
                Event::default().data(payload.to_string()),
            )]);
            return sse_response(stream);
        }
    };

    let receiver = agent.subscribe();
    let is_streaming = agent.is_streaming();
    let connected = json!({
        "type": "connected",
        "sessionId": id,
        "isStreaming": is_streaming,
    });

    let stream = async_stream::stream! {
        yield Ok::<Event, Infallible>(Event::default().data(connected.to_string()));
        let mut receiver = receiver;
        loop {
            match receiver.recv().await {
                Ok(payload) => yield Ok(Event::default().data(payload.to_string())),
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    };

    sse_response(stream)
}

fn sse_response<S>(stream: S) -> Response
where
    S: Stream<Item = Result<Event, Infallible>> + Send + 'static,
{
    let mut response = Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(30))
                .text(""),
        )
        .into_response();
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache, no-transform"),
    );
    headers.insert(
        axum::http::header::CONNECTION,
        axum::http::HeaderValue::from_static("keep-alive"),
    );
    headers.insert(
        axum::http::HeaderName::from_bytes(b"x-accel-buffering").unwrap(),
        axum::http::HeaderValue::from_static("no"),
    );
    response
}

/// `GET /api/agent/running`
pub async fn running(State(state): State<AppState>, _trusted: Trusted) -> Response {
    Json(json!({ "sessions": state.live().running() })).into_response()
}

/// `POST /api/agent/new` —— 新建一个 v3 会话文件并返回 id。
pub async fn new_session(
    State(state): State<AppState>,
    _trusted: Trusted,
    body: Option<Json<Value>>,
) -> Response {
    let cwd = body
        .as_ref()
        .and_then(|Json(v)| v.get("cwd"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| state.default_cwd().to_string_lossy().to_string());

    let repo = workman_session::V3SessionRepo::new(state.sessions().root());
    let created_at = now_ms();
    let id = uuid::Uuid::now_v7().to_string();
    let header = workman_session::V3Header::new(&id, created_at, &cwd);
    let dir = state
        .sessions()
        .root()
        .join(workman_session::session_directory_name(&cwd));
    let file = dir.join(workman_session::session_file_name(header.created_at, &header.id));
    let storage = match workman_session::V3SessionStorage::create_at(
        &file,
        &header,
        repo.clock(),
        repo.ids(),
    ) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    };
    drop(storage);
    state.sessions().invalidate();
    Json(json!({
        "id": id,
        "path": file.to_string_lossy(),
        "cwd": cwd,
    }))
    .into_response()
}

enum AgentError {
    NotFound,
    Failed(String),
}

/// 有活 agent 就用它；没有就按 id 找文件、建一个。
async fn ensure_agent(state: &AppState, id: &str) -> Result<Arc<LiveAgent>, AgentError> {
    if let Some(agent) = state.live().get(id) {
        return Ok(agent);
    }
    let Some(path) = state.sessions().path_of(id) else {
        return Err(AgentError::NotFound);
    };
    let cwd = crate::sessions_store::read_session(&path)
        .map(|s| s.header.cwd)
        .unwrap_or_else(|_| state.default_cwd().to_string_lossy().to_string());

    if !state.live().guard_starting(id) {
        // 另一个请求正在建：等它建好
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(agent) = state.live().get(id) {
                return Ok(agent);
            }
        }
    }

    let started = LiveAgent::start(id, &path, &cwd, state.catalog()).await;
    state.live().clear_starting(id);
    match started {
        Ok(agent) => {
            state.live().insert(Arc::clone(&agent));
            Ok(agent)
        }
        Err(e) => Err(AgentError::Failed(e.to_string())),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
