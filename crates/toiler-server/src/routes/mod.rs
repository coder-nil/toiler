//! 路由表：挂 axum Router，路径与方法对齐 pi-desktop 的 `/api`。
//!
//! 路径语法用 axum 0.8 的 `{param}` / `{*wildcard}`。

pub mod agent;
pub mod agent_extra;
pub mod auth;
pub mod files;
pub mod git;
pub mod mcp;
pub mod misc;
pub mod mobile;
pub mod models_config;
pub mod plugins;
pub mod projects;
pub mod search;
pub mod sessions;
pub mod sessions_extra;
pub mod skills;

use axum::routing::{get, post};
use axum::Router;

use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        // ---- sessions ----
        .route("/api/sessions", get(crate::routes::sessions::list))
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
        .route(
            "/api/sessions/{id}/context",
            get(crate::routes::sessions_extra::context),
        )
        .route(
            "/api/sessions/{id}/export",
            get(crate::routes::sessions_extra::export),
        )
        .route(
            "/api/sessions/{id}/entries/{entryId}/thinking",
            get(crate::routes::sessions_extra::thinking),
        )
        .route(
            "/api/sessions/{id}/auto-name",
            post(crate::routes::sessions_extra::auto_name),
        )
        .route("/api/sessions/search", get(crate::routes::search::search))
        // ---- agent ----
        .route(
            "/api/agent/{id}",
            get(crate::routes::agent::state_of).post(crate::routes::agent::command),
        )
        .route("/api/agent/{id}/events", get(crate::routes::agent::events))
        .route(
            "/api/agent/{id}/bash-output",
            get(crate::routes::agent_extra::bash_output),
        )
        .route("/api/agent/new", post(crate::routes::agent::new_session))
        .route("/api/agent/running", get(crate::routes::agent::running))
        .route(
            "/api/agent/running/events",
            get(crate::routes::agent_extra::running_events),
        )
        // ---- files ----
        // 路径为空时落到默认 cwd（前端文件面板首屏会请求根路径）
        .route(
            "/api/files",
            get(crate::routes::files::read_root).post(crate::routes::files::upload),
        )
        .route(
            "/api/files/",
            get(crate::routes::files::read_root).post(crate::routes::files::upload),
        )
        .route(
            "/api/files/{*path}",
            get(crate::routes::files::read)
                .put(crate::routes::files::write)
                .patch(crate::routes::files::rename)
                .post(crate::routes::files::upload),
        )
        // ---- models ----
        .route(
            "/api/skills",
            get(crate::routes::skills::list).patch(crate::routes::skills::toggle_not_implemented),
        )
        // 技能市场：pi 有搜索/安装，toiler 返回 501 + 中文说明（见 routes/skills.rs）
        .route(
            "/api/skills/search",
            post(crate::routes::skills::market_not_implemented),
        )
        .route(
            "/api/skills/install",
            post(crate::routes::skills::market_not_implemented),
        )
        // ---- plugins ----
        // 只读列出 `settings.json` 里配置的包与安装状态（见 routes/plugins.rs）。
        // 少了这条路由，插件页会把静态兜底的 404 `{"error":"Not found"}` 当成一条插件画出来。
        .route(
            "/api/plugins",
            get(crate::routes::plugins::list).post(crate::routes::plugins::command),
        )
        // 插件市场：pi 搜 npm 扩展包；toiler 返回空页（PLAN「明确不做的」）
        .route("/api/plugins/catalog", get(crate::routes::plugins::catalog))
        .route(
            "/api/plugins/catalog/translate",
            post(crate::routes::plugins::catalog_translate),
        )
        // ---- mcp ----
        // MCP 配置文件的读写与探活（见 routes/mcp.rs）。缺了它，设置面板的 MCP
        // 页会落到静态兜底的 404 `{"error":"Not found"}`，页面上显示成红字。
        .route(
            "/api/mcp",
            get(crate::routes::mcp::get)
                .put(crate::routes::mcp::put)
                .post(crate::routes::mcp::add),
        )
        .route("/api/mcp/test", post(crate::routes::mcp::test))
        .route("/api/models", get(crate::routes::misc::models))
        .route(
            "/api/models-config",
            get(crate::routes::models_config::get)
                .put(crate::routes::models_config::put)
                .delete(crate::routes::models_config::delete),
        )
        .route(
            "/api/models-config/enabled",
            get(crate::routes::models_config::enabled)
                .put(crate::routes::models_config::put_enabled),
        )
        .route(
            "/api/models-config/discover",
            post(crate::routes::models_config::discover),
        )
        .route(
            "/api/models-config/test",
            post(crate::routes::models_config::test),
        )
        .route(
            "/api/models-config/catalog",
            get(crate::routes::models_config::catalog),
        )
        // ---- auth ----
        .route("/api/auth/providers", get(crate::routes::auth::providers))
        .route(
            "/api/auth/all-providers",
            get(crate::routes::auth::all_providers),
        )
        .route(
            "/api/auth/api-key/{provider}",
            get(crate::routes::auth::api_key_status)
                .post(crate::routes::auth::save_api_key)
                .delete(crate::routes::auth::delete_api_key),
        )
        .route(
            "/api/auth/logout/{provider}",
            post(crate::routes::auth::logout),
        )
        .route(
            "/api/auth/login/{provider}",
            get(crate::routes::auth::login_stream).post(crate::routes::auth::login_unsupported),
        )
        // ---- git / worktrees ----
        .route(
            "/api/git",
            get(crate::routes::git::status).post(crate::routes::git::command),
        )
        .route("/api/git/status", get(crate::routes::git::status_only))
        .route("/api/git/diff", get(crate::routes::git::diff))
        .route(
            "/api/git/commit-message",
            post(crate::routes::git::commit_message),
        )
        .route(
            "/api/worktrees",
            get(crate::routes::git::worktrees_list)
                .post(crate::routes::git::worktrees_create)
                .delete(crate::routes::git::worktrees_delete),
        )
        // ---- projects / trust / cwd ----
        .route(
            "/api/projects",
            get(crate::routes::projects::list)
                .put(crate::routes::projects::select)
                .patch(crate::routes::projects::star)
                .delete(crate::routes::projects::remove),
        )
        .route(
            "/api/project-trust",
            get(crate::routes::projects::trust_status).post(crate::routes::projects::trust_set),
        )
        .route("/api/cwd/browse", get(crate::routes::projects::browse))
        .route(
            "/api/cwd/validate",
            post(crate::routes::projects::validate).get(crate::routes::projects::validate),
        )
        // ---- misc ----
        .route("/api/mobile/state", get(crate::routes::misc::mobile_state))
        .route(
            "/api/mobile/access",
            get(crate::routes::mobile::access_get).put(crate::routes::mobile::access_put),
        )
        .route("/api/mobile/pair", get(crate::routes::mobile::pair))
        .route("/api/home", get(crate::routes::misc::home))
        // Shell 工具开关：非 Windows 恒 `{isWindows:false,powerShellEnabled:false}`，
        // 前端据此整组不渲染（缺路由会让常规页多一行红色的 "Not found"）。
        .route(
            "/api/tools/settings",
            get(crate::routes::misc::tools_settings).put(crate::routes::misc::tools_settings_put),
        )
        .route("/api/default-cwd", post(crate::routes::misc::default_cwd))
        // 引导 / 续跑队列的投递模式。缺路由会让常规页在「完成提示音」下面
        // 常驻一行红色的 "Not found"（静态兜底的 404 原文），见 routes/misc.rs。
        .route(
            "/api/queue-modes",
            get(crate::routes::misc::queue_modes).put(crate::routes::misc::queue_modes_put),
        )
        // `qoder-cn` 的本机转发端点（不是 `/api` 契约的一部分）：rpi 的 provider
        // 只能按 baseUrl 发标准 OpenAI 请求，这里把它改写成 COSY 签名请求再转发。
        // 路径带进程内随机令牌 + 只收 loopback 对端，见 `qoder_cn::proxy`。
        .route(
            "/qoder-cn/{token}/v1/chat/completions",
            post(crate::qoder_cn::proxy::chat_completions),
        )
        .with_state(state)
        // 其余路径一律交给内嵌的前端产物：`/api/*` 会先命中上面的具体路由，
        // 所以这里只需处理静态文件、客户端路由与 404。
        .fallback(get(crate::assets::serve_static))
}
