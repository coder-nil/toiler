//! `POST /api/version-upgrade` 的契约与安全边界。
//!
//! 这条路由**会创建提交和标签**，属于高权限写操作，所以除了 `Trusted`
//!（Host + Origin 同源）之外还有一道「进程必须是桌面壳」的闸门：环境里要有
//! `PI_WEB_DESKTOP_API_ORIGIN`，它只有 Tauri 壳会注入，命令行直启的
//! `toiler-server` 没有。
//!
//! ## 这份测试曾经掩盖过一个真实缺陷
//!
//! 早先的实现要求请求 `Origin` **字符串等于** `tauri://localhost`，而这份测试
//! 就用手工构造的 `Origin: tauri://localhost` 去断言「桌面壳放行」。但桌面窗口
//! 根本不发这个值 —— `create_main_window` 用
//! `WebviewUrl::External("http://127.0.0.1:<port>")` 加载页面，窗口里页面的
//! origin 是那个回环地址。于是**功能在它唯一被允许的地方恒为 403**，而测试全绿。
//!
//! 现在 [`desktop_window_request_shape_is_allowed`] 专门盯这一点：它发出的
//! Origin 就是桌面窗口真实会发的回环地址，不带任何伪造的 `tauri://`。
//!
//! ## 闸门不管「谁在打」这件事
//!
//! Origin 判定也区分不出手机与隧道：两者都经 `lan_proxy`，而
//! `rewrite_request_head()` 会把它们的 `Origin` 改写成
//! `http://127.0.0.1:<server_port>`，与桌面窗口发出的值逐字节相同。按当前决策，
//! 这个能力靠**前端只在桌面壳里渲染按钮**（`window.__PI_WEB_DESKTOP__`）收口，
//! 服务端不再加严格判别。别把这些断言当成「手机与隧道被硬隔离」的证据 ——
//! 它们只证明「命令行直启的进程没有这个能力」。

#![allow(clippy::await_holding_lock)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::PathBuf;
use toiler_server::{catalog::ModelCatalog, routes, state::AppState};
use tower::ServiceExt;

/// 测试里服务绑定的回环地址；`Host` 与桌面窗口的 `Origin` 都用它。
const LOOPBACK: &str = "127.0.0.1:30141";

/// 与 `toiler-desktop/src/lib.rs` 的 `DESKTOP_API_ORIGIN` 一致。
///
/// 注意它**只**用来模拟「壳进程里的那个环境变量」，不再是期望的请求 `Origin`。
const DESKTOP_ENV: &str = "tauri://localhost";

fn fixture(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "toiler-verup-{tag}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// `cwd` 要拼进查询串，必须是纯 ASCII 路径。
///
/// macOS 的 `temp_dir()` 是 `/var/folders/<id>/T/…`（纯 ASCII，可用），但为了
/// 不让断言依赖用户名或临时目录形状，这里再校验一次并给出可读的失败原因。
fn ascii_cwd(path: &std::path::Path) -> String {
    let text = path.to_string_lossy().to_string();
    assert!(text.is_ascii(), "测试路径必须纯 ASCII：{text}");
    text
}

fn app(cwd: &std::path::Path) -> axum::Router {
    // 允许根要包含 cwd，否则请求会先被「不在允许范围」挡掉，
    // 测不到我们关心的桌面壳闸门。`with_cwd` 会把 cwd 种进允许根。
    let catalog = ModelCatalog::from_config(json!({"providers": {}}), None).unwrap();
    let state = AppState::with_cwd(cwd, catalog, cwd).with_binding("127.0.0.1", 30141);
    routes::router(state)
}

/// 桌面壳环境变量是进程级的，测试之间会互相影响，用互斥体串行。
///
/// poison 掉也要继续：环境变量在 drop 里会被还原，前一个测试失败不该让其余
/// 全挂（否则真正的首个失败会被淹没在一片 PoisonError 里）。
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 取串行锁。
///
/// 锁保护的是进程级环境变量，整个测试期间（含若干次 `await`）都必须保持独占，
/// 否则别的测试会在中途改掉 `PI_WEB_DESKTOP_API_ORIGIN`。改用 async 锁没有意义 ——
/// 环境变量本身就是进程全局的同步状态。因此本文件整体允许
/// `clippy::await_holding_lock`（见文件头的 `#![allow]`）。
fn acquire_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

struct DesktopEnv(Option<std::ffi::OsString>);

impl DesktopEnv {
    fn set(value: &str) -> Self {
        let previous = std::env::var_os("PI_WEB_DESKTOP_API_ORIGIN");
        std::env::set_var("PI_WEB_DESKTOP_API_ORIGIN", value);
        Self(previous)
    }
    fn unset() -> Self {
        let previous = std::env::var_os("PI_WEB_DESKTOP_API_ORIGIN");
        std::env::remove_var("PI_WEB_DESKTOP_API_ORIGIN");
        Self(previous)
    }
}

impl Drop for DesktopEnv {
    fn drop(&mut self) {
        match self.0.take() {
            Some(value) => std::env::set_var("PI_WEB_DESKTOP_API_ORIGIN", value),
            None => std::env::remove_var("PI_WEB_DESKTOP_API_ORIGIN"),
        }
    }
}

async fn post(
    router: axum::Router,
    uri: &str,
    body: Value,
    origin: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", LOOPBACK)
        .header("content-type", "application/json");
    if let Some(origin) = origin {
        builder = builder.header("origin", origin);
    }
    let request = builder
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn preview_body() -> Value {
    json!({ "level": "patch", "dryRun": true })
}

/// **核心安全断言**：没有桌面壳环境变量时（命令行直启的 `toiler-server`），
/// 即使 Origin 与 Host 完全同源也必须 403。
#[tokio::test]
async fn rejects_when_not_running_in_desktop_shell() {
    let _guard = acquire_env();
    let _env = DesktopEnv::unset();
    let root = fixture("no-shell");
    let url = format!("/api/version-upgrade?cwd={}", ascii_cwd(&root));

    // 请求形状与桌面窗口完全一致，唯一的差别是进程里没有那个环境变量。
    let origin = format!("http://{LOOPBACK}");
    let (status, body) = post(app(&root), &url, preview_body(), Some(&origin)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("桌面"),
        "{body}"
    );
}

/// **回归用例**：桌面窗口真实发出的请求形状必须放行。
///
/// 桌面窗口加载的是 `http://127.0.0.1:<port>`，所以它发出的 `Origin` 就是这个
/// 回环地址，绝不是 `tauri://localhost`。旧实现按后者的字符串相等判定，导致三个
/// 按钮在桌面里恒为 403 —— 这个用例就是守着那个缺陷不再复发。
#[tokio::test]
async fn desktop_window_request_shape_is_allowed() {
    let _guard = acquire_env();
    let _env = DesktopEnv::set(DESKTOP_ENV);
    let root = fixture("desktop-shape");
    let url = format!("/api/version-upgrade?cwd={}", ascii_cwd(&root));

    let origin = format!("http://{LOOPBACK}");
    let (status, body) = post(app(&root), &url, preview_body(), Some(&origin)).await;

    // 已穿过闸门：这里不是 Git 仓库，所以业务层报「不是 Git 仓库」。
    assert_ne!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("Git"),
        "{body}"
    );
}

/// 跨站 `Origin` 仍被 `Trusted` 那一层挡住，与桌面壳闸门无关。
///
/// 注意这**不是**「手机与隧道被挡住」的证据：`lan_proxy` 会把它们的 `Origin`
/// 改写成回环地址，所以经代理进来的请求在这一层是通过的（见文件头说明）。
/// 这条只为守住「不经过代理、直接跨站打过来」的请求被拒。
#[tokio::test]
async fn foreign_origin_is_rejected_by_the_trusted_layer() {
    let _guard = acquire_env();
    let _env = DesktopEnv::set(DESKTOP_ENV);
    let root = fixture("foreign-origin");
    let url = format!("/api/version-upgrade?cwd={}", ascii_cwd(&root));

    for origin in ["https://abc.trycloudflare.com", "http://192.168.1.5:8787"] {
        let (status, body) = post(app(&root), &url, preview_body(), Some(origin)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "origin={origin}: {body}");
    }
}

/// 参数校验：缺 cwd / 相对路径 / 不存在目录都回 400，且都在桌面壳闸门之后。
#[tokio::test]
async fn validates_cwd_arguments() {
    let _guard = acquire_env();
    let _env = DesktopEnv::set(DESKTOP_ENV);
    let root = fixture("cwd-args");
    let origin = format!("http://{LOOPBACK}");

    let (status, body) = post(
        app(&root),
        "/api/version-upgrade",
        preview_body(),
        Some(&origin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = post(
        app(&root),
        "/api/version-upgrade?cwd=relative/path",
        preview_body(),
        Some(&origin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("绝对路径"),
        "{body}"
    );

    let missing = std::env::temp_dir().join("toiler-verup-definitely-missing-dir");
    let url = format!("/api/version-upgrade?cwd={}", ascii_cwd(&missing));
    let (status, _) = post(app(&root), &url, preview_body(), Some(&origin)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// 请求体必须严格：未知字段、非法 level 都拒绝（`deny_unknown_fields`）。
#[tokio::test]
async fn rejects_malformed_bodies() {
    let _guard = acquire_env();
    let _env = DesktopEnv::set(DESKTOP_ENV);
    let root = fixture("bodies");
    let url = format!("/api/version-upgrade?cwd={}", ascii_cwd(&root));
    let origin = format!("http://{LOOPBACK}");

    // 非法 level：serde 反序列化失败 → axum 回 422。
    let (status, _) = post(
        app(&root),
        &url,
        json!({ "level": "bogus", "dryRun": true }),
        Some(&origin),
    )
    .await;
    assert!(
        status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST,
        "{status}"
    );

    // 未知字段。
    let (status, _) = post(
        app(&root),
        &url,
        json!({ "level": "patch", "dryRun": true, "command": "rm -rf /" }),
        Some(&origin),
    )
    .await;
    assert!(
        status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST,
        "{status}"
    );
}
