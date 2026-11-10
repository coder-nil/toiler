//! 路由表：挂 axum Router，路径与方法对齐 pi-desktop 的 `/api`。
//!
//! 路径语法用 axum 0.8 的 `{param}` / `{*wildcard}`。

pub mod agent;
pub mod files;
pub mod misc;
pub mod search;
pub mod sessions;

use axum::routing::{get, post};
use axum::Router;

use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        // sessions
        .route(
            "/api/sessions",
            get(crate::routes::sessions::list),
        )
        .route(
            "/api/sessions/{id}",
            get(crate::routes::sessions::detail)
                .patch(crate::routes::sessions::rename)
                .delete(crate::routes::sessions::delete),
        )
        // sessions/{id}/state 与 GET /api/agent/{id} 同契约
        .route(
            "/api/sessions/{id}/state",
            get(crate::routes::agent::state_of),
        )
        .route("/api/sessions/search", get(crate::routes::search::search))
        // agent
        .route(
            "/api/agent/{id}",
            get(crate::routes::agent::state_of).post(crate::routes::agent::command),
        )
        .route(
            "/api/agent/{id}/events",
            get(crate::routes::agent::events),
        )
        .route("/api/agent/new", post(crate::routes::agent::new_session))
        .route("/api/agent/running", get(crate::routes::agent::running))
        // files
        .route(
            "/api/files/{*path}",
            get(crate::routes::files::read),
        )
        // models / mobile / misc
        .route("/api/models", get(crate::routes::misc::models))
        .route(
            "/api/mobile/state",
            get(crate::routes::misc::mobile_state),
        )
        .route("/api/home", get(crate::routes::misc::home))
        .route(
            "/api/cwd/validate",
            get(crate::routes::misc::cwd_validate),
        )
        .route(
            "/api/default-cwd",
            post(crate::routes::misc::default_cwd),
        )
        .with_state(state)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .fallback(not_found)
}

async fn not_found() -> impl axum::response::IntoResponse {
    (
        axum::http::StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({ "error": "Not found" })),
    )
}
