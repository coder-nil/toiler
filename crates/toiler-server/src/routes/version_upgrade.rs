//! 版本号升级（桌面 Git 面板的 patch / minor / major 三个按钮）。
//!
//! ## 为什么在 `/api` 却又要额外闸门
//!
//! 这条路由会**创建提交和标签**，属于高权限写操作。但它不能像普通 `/api` 那样
//! 只靠 `Trusted`（Host + Origin 同源）：手机配对与 Cloudflare 隧道也走同一套
//! `/api`，`Trusted` 对它们是放行的。所以这里再叠一层「必须是桌面壳」：
//!
//! 1. 进程里必须有 `PI_WEB_DESKTOP_API_ORIGIN`（只有 Tauri 壳会注入）
//! 2. 请求的 `Origin` 必须与它相等。手机浏览器的 Origin 是对外地址，
//!    隧道转发的请求带的是隧道域名，两者都不可能等于 `tauri://localhost`
//!
//! 第 2 条是关键：只查环境变量的话，手机通过隧道打进来也会命中（服务端进程是
//! 同一个），判定就失效了。原来把这段逻辑放在 Tauri command 里天然满足这两条，
//! 现在移到 HTTP 上必须显式补回来。
//!
//! ## 为什么不再依赖 Python
//!
//! 客户端机器不保证有 Python 3.9+。原来的实现用 `python3 -c` 跑内嵌的
//! `bump-version.py`，没装 Python 的机器上三个按钮直接报错。现在全部逻辑在
//! [`crate::version_bump`] 里用 Rust 实现；`tools/bump-version.py` 只留给开发者
//! 当 CLI 用。

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::gitsvc;
use crate::security;
use crate::state::AppState;
use crate::version_bump::{self, UpgradeRequest};

#[derive(Debug, Deserialize)]
pub struct UpgradeParams {
    /// 项目里的任意目录；内部会解析到仓库根。
    cwd: String,
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

/// 只有桌面壳窗口能改版本号。
///
/// 直接用 `AppState` 里的 policy，**不靠 `Trusted` 抽取器**：后者从
/// `parts.extensions` 取 `TrustPolicy`，而本项目从未往扩展里注入过它（全仓只有
/// `state.rs` 存着真实 policy），所以抽取器实际总是用 `default()`（空）。
/// 对普通路由没影响（loopback 的 `Host` 本来就放行），但「桌面壳 origin 例外」
/// 恰恰要靠 policy 里那个值 —— 跟着抽取器走永远不会生效。
fn from_desktop_shell(policy: &crate::security::TrustPolicy, headers: &HeaderMap) -> bool {
    let Some(desktop) = policy.desktop_api_origin.as_deref() else {
        return false;
    };
    if desktop.trim().is_empty() {
        return false;
    }
    let Some(origin) = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    origin.trim() == desktop.trim()
}

/// `POST /api/version-upgrade?cwd=…`
///
/// **故意不用 `Trusted` 抽取器**。它从 `parts.extensions` 取 `TrustPolicy`，而本
/// 仓从未往扩展里注入过（`routes::router(state)` 没挂中间件），因此它总是用
/// `TrustPolicy::default()`：`desktop_api_origin` 为 `None`，桌面壳的
/// `Origin: tauri://localhost` 与 loopback 的 `Host` 不同源 → 一律 403。
/// 普通路由靠「loopback Host 就放行」没暴露问题，但这条路由要的恰好是那条
/// 例外，所以这里直接拿 state 里的真实 policy 自带校验。
pub async fn upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<UpgradeParams>,
    Json(request): Json<UpgradeRequest>,
) -> Response {
    // 两道闸：
    // 1. Host + Origin 必须过基础可信校验（代替 `Trusted` 抽取器，理由见上面 doc）
    // 2. 必须是桌面壳（Origin 等于 `PI_WEB_DESKTOP_API_ORIGIN`）
    let policy = state.trust();
    if !security::is_trusted_api_request(
        &axum::http::Method::POST,
        "/api/version-upgrade",
        &headers,
        policy,
    ) {
        return error(StatusCode::FORBIDDEN, "不受信任的请求来源");
    }
    if !from_desktop_shell(policy, &headers) {
        return error(
            StatusCode::FORBIDDEN,
            "版本升级只在桌面窗口提供，手机与隧道不支持",
        );
    }

    // `cwd` 必须落在允许根内，且是真实目录。版本升级会写文件，不能只做词法判定。
    let cwd = params.cwd.trim();
    if cwd.is_empty() {
        return error(StatusCode::BAD_REQUEST, "缺少项目目录");
    }
    let path = std::path::Path::new(cwd);
    if !path.is_absolute() {
        return error(StatusCode::BAD_REQUEST, "项目路径必须是绝对路径");
    }
    if !path.is_dir() {
        return error(StatusCode::BAD_REQUEST, "项目目录不存在");
    }
    let allowed = state.roots();
    if !allowed.allows_real(cwd) {
        // 与其它 Git 写路由一致：先同步一次允许根（新建项目还没进会话表时其中包含它）。
        state.sync_allowed_roots();
        if !state.roots().allows_real(cwd) {
            return error(StatusCode::FORBIDDEN, "项目目录不在允许访问的范围内");
        }
    }

    let Some(root) = gitsvc::repository_root(path) else {
        return error(StatusCode::BAD_REQUEST, "不是 Git 仓库");
    };

    // `upgrade` 是阻塞的（同步 git 子进程 + 文件读写），不能占着 async 线程。
    // 仓库锁也在阻塞块里取：`std::sync::MutexGuard` 不是 `Send`，不能跨 await。
    let result = tokio::task::spawn_blocking(move || {
        let lock = gitsvc::repo_lock(&root);
        let Ok(_guard) = lock.lock() else {
            return Err(BumpFailure::Conflict("仓库正在执行其它操作"));
        };
        version_bump::upgrade(&root, &request).map_err(BumpFailure::Failed)
    })
    .await;
    match result {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(BumpFailure::Conflict(message))) => error(StatusCode::CONFLICT, message),
        Ok(Err(BumpFailure::Failed(failure))) => error(StatusCode::BAD_REQUEST, failure.0),
        Err(join_error) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("版本升级失败：{join_error}"),
        ),
    }
}

/// 阻塞块里的失败分类：仓库忙与业务失败要回不同状态码。
enum BumpFailure {
    Conflict(&'static str),
    Failed(version_bump::BumpError),
}
