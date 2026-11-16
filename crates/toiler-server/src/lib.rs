pub mod agent_runtime;
pub mod apisets;
pub mod assets;
pub mod auth;
pub mod boot;
pub mod builtin_providers;
pub mod catalog;
pub mod desktop_tools;
pub mod docx;
pub mod filemeta;
pub mod git_credentials;
pub mod gitsvc;
pub mod mobile_access;
pub mod mobile_view;
pub mod models_json;
pub mod oneshot;
pub mod paths;
pub mod projects;
pub mod qoder_cn;
pub mod retry_provider;
pub mod routes;
pub mod security;
pub mod session_view;
pub mod sessions_store;
pub mod settings;
pub mod state;
pub mod system_prompt;
pub mod tools;
pub mod trust;

/// 当前时间的 ISO-8601（毫秒精度，`Z` 结尾）——与 pi 写进文件的时间戳同格式。
pub fn toiler_now_iso() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    toiler_session::ms_to_iso(ms)
}
