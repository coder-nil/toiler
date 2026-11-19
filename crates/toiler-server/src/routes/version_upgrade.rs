//! 版本号升级（桌面 Git 面板的 patch / minor / major 三个按钮）。
//!
//! ## 为什么在 `/api` 却又要额外闸门
//!
//! 这条路由会**创建提交和标签**，属于高权限写操作。除了常规的 `Trusted`
//!（Host + Origin 同源）之外，这里再叠一层「进程必须是桌面壳」：环境里要有
//! `PI_WEB_DESKTOP_API_ORIGIN`，这个变量只有 Tauri 壳会注入。命令行直启的
//! `toiler-server` 没有它，三个按钮直接 403。
//!
//! ## 为什么不再比对请求的 `Origin`
//!
//! 早先这里要求 `Origin` 字符串等于 `PI_WEB_DESKTOP_API_ORIGIN`
//!（`tauri://localhost`）。那条判定是错的，会让功能在**桌面窗口里也恒为 403**：
//! `create_main_window` 用 `WebviewUrl::External("http://127.0.0.1:<port>")` 加载
//! 页面，窗口里页面的 origin 就是那个回环地址，永远不会是 `tauri://localhost`。
//! 只有手工用 curl 伪造 `Origin: tauri://localhost` 才通得过 —— 契约测试当初正是
//! 这么写的，于是把「桌面窗口不可用」这个缺陷盖住了。
//!
//! 换成「回环 Origin」同样区分不出来：手机与隧道都经 `lan_proxy`，而
//! `rewrite_request_head()` 会把它们的 `Origin` 改写成
//! `http://127.0.0.1:<server_port>` —— 与桌面窗口发出的值逐字节相同。
//!
//! 所以闸门只保留环境变量这一条，本能力靠**前端只在桌面壳里渲染按钮**来收口
//!（`window.__PI_WEB_DESKTOP__`，见 `lib/version-upgrade.ts`）。这意味着手机或
//! 隧道若手工构造请求，服务端**会**放行；这是明确接受的取舍，别把这里当成
//! 硬隔离。真要硬隔离得给桌面壳加一次性令牌，见 `docs/version-upgrade.md`。
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

/// 进程是不是跑在桌面壳里。
///
/// 只看 `AppState` 里的 policy 有没有 `desktop_api_origin`（来自
/// `PI_WEB_DESKTOP_API_ORIGIN`，只有 Tauri 壳注入），**不看请求的 `Origin`** ——
/// 理由见文件头「为什么不再比对请求的 `Origin`」。
///
/// 直接用 `AppState` 里的 policy，**不靠 `Trusted` 抽取器**：后者从
/// `parts.extensions` 取 `TrustPolicy`，而本项目从未往扩展里注入过它（全仓只有
/// `state.rs` 存着真实 policy），所以抽取器实际总是用 `default()`（空）。
fn from_desktop_shell(policy: &crate::security::TrustPolicy) -> bool {
    policy
        .desktop_api_origin
        .as_deref()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

/// `POST /api/version-upgrade?cwd=…`
///
/// **故意不用 `Trusted` 抽取器**。它从 `parts.extensions` 取 `TrustPolicy`，而本
/// 仓从未往扩展里注入过（`routes::router(state)` 没挂中间件），因此它总是用
/// `TrustPolicy::default()`。抽取器本身没问题（loopback 的 Host 本来就放行），
/// 但这条路由还要读 policy 里的 `desktop_api_origin`，而那个值只在 state 的真实
/// policy 里，所以直接拿 `state.trust()` 自己校。
pub async fn upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<UpgradeParams>,
    Json(request): Json<UpgradeRequest>,
) -> Response {
    // 两道闸：
    // 1. Host + Origin 必须过基础可信校验（代替 `Trusted` 抽取器，理由见上面 doc）
    // 2. 进程必须跑在桌面壳里（环境里有 `PI_WEB_DESKTOP_API_ORIGIN`）
    let policy = state.trust();
    if !security::is_trusted_api_request(
        &axum::http::Method::POST,
        "/api/version-upgrade",
        &headers,
        policy,
    ) {
        return error(StatusCode::FORBIDDEN, "不受信任的请求来源");
    }
    if !from_desktop_shell(policy) {
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
