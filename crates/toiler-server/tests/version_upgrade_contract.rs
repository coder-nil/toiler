//! `POST /api/version-upgrade` 的契约与安全边界。
//!
//! 这条路由**会创建提交和标签**，所以除了 `Trusted`（Host + Origin 同源）还必须
//! 满足「来自桌面壳」：进程里有 `PI_WEB_DESKTOP_API_ORIGIN`，且请求 `Origin`
//! 与它相等。手机配对页与 Cloudflare 隧道的 Origin 都是对外地址，不可能等于
//! `tauri://localhost`，因此会被 403 挡住 —— 这是本文件最要紧的断言。

#![allow(clippy::await_holding_lock)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::PathBuf;
use toiler_server::{catalog::ModelCatalog, routes, state::AppState};
use tower::ServiceExt;

/// 与 `toiler-desktop/src/lib.rs` 的 `DESKTOP_API_ORIGIN` 一致：
/// macOS/Linux 报 `tauri://localhost`，Windows 报 `http://tauri.localhost`。
///
/// 注意 `tauri://` 不是合法 HTTP URL —— `security.rs` 的同源判定用 `url::Url`
/// 解析 Origin，所以这里取 Windows 那个形状来跑路由级断言（两者走的是同一段
/// 字符串相等比较，差别只在这个 scheme 能否被 url 解析）。
const DESKTOP_ORIGIN: &str = "http://tauri.localhost";

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
        .header("host", "127.0.0.1:30141")
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

/// **核心安全断言**：没有桌面壳环境变量时（命令行直启 / 手机 / 隧道），
/// 即使 Origin 自洽也必须 403。
#[tokio::test]
async fn rejects_when_not_running_in_desktop_shell() {
    let _guard = acquire_env();
    let _env = DesktopEnv::unset();
    let root = fixture("no-shell");
    let url = format!("/api/version-upgrade?cwd={}", ascii_cwd(&root));

    let (status, body) = post(
        app(&root),
        &url,
        preview_body(),
        Some("http://127.0.0.1:30141"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("桌面"),
        "{body}"
    );
}

/// 手机通过隧道打进来时，服务端进程里**确实**有桌面壳环境变量（同一个进程），
/// 但 Origin 是外部地址 —— 必须靠 Origin 比对挡住。
#[tokio::test]
async fn rejects_foreign_origin_even_inside_desktop_shell() {
    let _guard = acquire_env();
    let _env = DesktopEnv::set(DESKTOP_ORIGIN);
    let root = fixture("foreign-origin");
    let url = format!("/api/version-upgrade?cwd={}", ascii_cwd(&root));

    for origin in ["https://abc.trycloudflare.com", "http://192.168.1.5:8787"] {
        let (status, body) = post(app(&root), &url, preview_body(), Some(origin)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "origin={origin}: {body}");
    }

    // 不带 Origin（curl 之类）同样拒绝：不能靠「省略 Origin」绕过。
    let (status, body) = post(app(&root), &url, preview_body(), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// 桌面壳 origin 正确时放行到业务层。这里不是 Git 仓库，所以应报「不是 Git 仓库」，
/// 而不是 403 —— 说明闸门已经通过。
#[tokio::test]
async fn allows_desktop_origin_and_reaches_business_logic() {
    let _guard = acquire_env();
    let _env = DesktopEnv::set(DESKTOP_ORIGIN);
    let root = fixture("allowed");
    let url = format!("/api/version-upgrade?cwd={}", ascii_cwd(&root));

    let (status, body) = post(app(&root), &url, preview_body(), Some(DESKTOP_ORIGIN)).await;
    assert_ne!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("Git"),
        "{body}"
    );
}

/// 参数校验：缺 cwd / 相对路径 / 不存在目录都回 400，且都在桌面壳闸门之后。
#[tokio::test]
async fn validates_cwd_arguments() {
    let _guard = acquire_env();
    let _env = DesktopEnv::set(DESKTOP_ORIGIN);
    let root = fixture("cwd-args");

    let (status, body) = post(
        app(&root),
        "/api/version-upgrade",
        preview_body(),
        Some(DESKTOP_ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = post(
        app(&root),
        "/api/version-upgrade?cwd=relative/path",
        preview_body(),
        Some(DESKTOP_ORIGIN),
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
    let (status, _) = post(app(&root), &url, preview_body(), Some(DESKTOP_ORIGIN)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// 请求体必须严格：未知字段、非法 level 都拒绝（`deny_unknown_fields`）。
#[tokio::test]
async fn rejects_malformed_bodies() {
    let _guard = acquire_env();
    let _env = DesktopEnv::set(DESKTOP_ORIGIN);
    let root = fixture("bodies");
    let url = format!("/api/version-upgrade?cwd={}", ascii_cwd(&root));

    // 非法 level：serde 反序列化失败 → axum 回 422。
    let (status, _) = post(
        app(&root),
        &url,
        json!({ "level": "bogus", "dryRun": true }),
        Some(DESKTOP_ORIGIN),
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
        Some(DESKTOP_ORIGIN),
    )
    .await;
    assert!(
        status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST,
        "{status}"
    );
}
