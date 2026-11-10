//! `/api/agent` 的补充 route（阶段 3 补齐）。
//!
//! | route | 行为 |
//! |---|---|
//! | `GET /api/agent/{id}/bash-output?path=&download=1` | 取 bash 工具落盘的完整输出 |
//! | `GET /api/agent/running/events` | SSE：运行中会话集合的实时快照 |
//!
//! `bash-output` 的两道校验（模块 `80104.fM` + `65630.jV`）：
//!
//! 1. `path` 必须是 `$TMPDIR` 下形如 `pi-bash-*.log` 的文件；
//! 2. 该路径必须出现在**这个会话**的某条 `bashExecution` 消息的
//!    `fullOutputPath` 里（防越权读别人的临时文件）。

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::Stream;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::security::Trusted;
use crate::sessions_store::read_session;
use crate::state::AppState;

/// 完整输出的大小上限：5 MiB（pi 的 `80104.gQ`）。
pub const MAX_FULL_OUTPUT_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Deserialize, Default)]
pub struct BashOutputQuery {
    path: Option<String>,
    download: Option<String>,
}

/// `GET /api/agent/{id}/bash-output`
pub async fn bash_output(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(id): Path<String>,
    Query(query): Query<BashOutputQuery>,
) -> Response {
    let Some(raw) = query.path.as_deref().filter(|p| !p.trim().is_empty()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "path required" }))).into_response();
    };
    let Some(path) = resolve_bash_log(raw) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid path" }))).into_response();
    };
    if !belongs_to_session(&state, &id, &path) {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": "forbidden" }))).into_response();
    }

    let download = query.download.as_deref() == Some("1");
    if download {
        return match std::fs::read(&path) {
            Ok(bytes) => {
                let mut response = Response::new(axum::body::Body::from(bytes));
                let headers = response.headers_mut();
                headers.insert(
                    header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("text/plain; charset=utf-8"),
                );
                headers.insert(
                    header::CONTENT_DISPOSITION,
                    axum::http::HeaderValue::from_static(
                        "attachment; filename=\"bash-output.log\"",
                    ),
                );
                headers.insert(
                    header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                response
            }
            Err(_) => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "full output unavailable" })),
            )
                .into_response(),
        };
    }

    match std::fs::metadata(&path) {
        Ok(meta) if meta.len() > MAX_FULL_OUTPUT_BYTES => (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({
                "error": format!("Full output is too large to display (limit {MAX_FULL_OUTPUT_BYTES} bytes)"),
                "data": { "size": meta.len(), "maxBytes": MAX_FULL_OUTPUT_BYTES },
            })),
        )
            .into_response(),
        Ok(_) => match std::fs::read(&path) {
            Ok(bytes) => Json(json!({
                "success": true,
                "data": { "output": String::from_utf8_lossy(&bytes) },
            }))
            .into_response(),
            Err(_) => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "full output unavailable" })),
            )
                .into_response(),
        },
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "full output unavailable" })),
        )
            .into_response(),
    }
}

/// 复刻 `80104.fM`：必须在 `$TMPDIR` 下、必须是 `pi-bash-*.log`。
fn resolve_bash_log(raw: &str) -> Option<std::path::PathBuf> {
    let candidate = crate::paths::absolute(std::path::Path::new(raw));
    let tmp = std::env::temp_dir();
    let tmp = std::fs::canonicalize(&tmp).unwrap_or(tmp);
    let parent = candidate.parent()?.to_path_buf();
    let parent = std::fs::canonicalize(&parent).unwrap_or(parent);
    if parent != tmp {
        return None;
    }
    let name = candidate.file_name()?.to_string_lossy().to_string();
    if !is_bash_log_name(&name) {
        return None;
    }
    Some(candidate)
}

/// `^pi-bash-[A-Za-z0-9_-]+\.log$`
fn is_bash_log_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("pi-bash-") else {
        return false;
    };
    let Some(stem) = rest.strip_suffix(".log") else {
        return false;
    };
    !stem.is_empty()
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 复刻 `49180.j1` 的意图：这条路径必须出现在**本会话**的条目里。
///
/// pi 只认 `role:"bashExecution"` 消息的顶层 `fullOutputPath`。workman 额外
/// 接受新工具形态（`role:"toolResult"` 的 `details.fullOutputPath`），因为
/// 现在的 pi 把 bash 结果写成 toolResult —— 两种写法都要求路径属于该会话。
fn belongs_to_session(state: &AppState, id: &str, path: &std::path::Path) -> bool {
    let Some(session_path) = state.sessions().path_of(id) else {
        return false;
    };
    let Ok(session) = read_session(&session_path) else {
        return false;
    };
    let target = path.to_string_lossy().to_string();
    session.entries.iter().any(|entry| {
        if entry.get("type").and_then(Value::as_str) != Some("message") {
            return false;
        }
        let Some(message) = entry.get("message") else {
            return false;
        };
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        match role {
            "bashExecution" => {
                message.get("fullOutputPath").and_then(Value::as_str) == Some(target.as_str())
            }
            "toolResult" => {
                message
                    .get("details")
                    .and_then(|d| d.get("fullOutputPath"))
                    .and_then(Value::as_str)
                    == Some(target.as_str())
            }
            _ => false,
        }
    })
}

/// `GET /api/agent/running/events` —— 推 `{type:"running", runningSessionIds, pendingUiSessionIds}`。
pub async fn running_events(
    State(state): State<AppState>,
    _trusted: Trusted,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut changes = state.live().subscribe_changes();
    let live = std::sync::Arc::new(state.live().clone());
    let initial = live.running_snapshot();

    let stream = async_stream::stream! {
        yield Ok(Event::default().data(running_frame(&initial)));
        loop {
            match tokio::time::timeout(Duration::from_secs(30), changes.recv()).await {
                Ok(Ok(())) => {
                    let snapshot = live.running_snapshot();
                    yield Ok(Event::default().data(running_frame(&snapshot)));
                }
                // 通道跟不上了：直接推一帧最新快照
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                    let snapshot = live.running_snapshot();
                    yield Ok(Event::default().data(running_frame(&snapshot)));
                }
                // 发送端全没了（服务关停）
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                // 30 秒没变化：发一个注释行当心跳（pi 用同样的写法）
                Err(_) => {
                    yield Ok(Event::default().comment(""));
                }
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// 拼一帧 `{type:"running", runningSessionIds, pendingUiSessionIds}`。
fn running_frame(snapshot: &Value) -> String {
    let mut frame = serde_json::Map::new();
    frame.insert("type".into(), Value::from("running"));
    if let Some(map) = snapshot.as_object() {
        for (key, value) in map {
            frame.insert(key.clone(), value.clone());
        }
    }
    Value::Object(frame).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_log_name_pattern() {
        assert!(is_bash_log_name("pi-bash-abc_123-XYZ.log"));
        assert!(!is_bash_log_name("pi-bash-.log"));
        assert!(!is_bash_log_name("pi-bash-abc.txt"));
        assert!(!is_bash_log_name("other.log"));
        assert!(!is_bash_log_name("pi-bash-ab c.log"));
    }

    #[test]
    fn rejects_paths_outside_tmpdir() {
        assert!(resolve_bash_log("/etc/passwd").is_none());
        assert!(resolve_bash_log("/tmp/pi-bash-x.log").is_none() || cfg!(target_os = "linux"));
    }
}
