pub mod agent_runtime;
pub mod assets;
pub mod auth;
pub mod boot;
pub mod catalog;
pub mod gitsvc;
pub mod models_json;
pub mod oneshot;
pub mod paths;
pub mod projects;
pub mod routes;
pub mod settings;
pub mod trust;
pub mod security;
pub mod session_view;
pub mod sessions_store;
pub mod state;
pub mod tools;

/// 当前时间的 ISO-8601（毫秒精度，`Z` 结尾）——与 pi 写进文件的时间戳同格式。
pub fn workman_now_iso() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    workman_session::ms_to_iso(ms)
}
