//! `POST /api/files/reveal`（在 Finder / 文件管理器里显示）的契约与边界。
//!
//! 前端用法：`components/RevealFileButton.tsx`——文件查看器顶栏、文件面板行内
//! 悬停按钮、Git 面板都调它。这条路由曾经是 501 桩，按钮点下去只弹「无法打开
//! 文件所在位置」；本文件守着「它真的会调系统文件管理器」以及「手机与隧道调不动」。
//!
//! ## 为什么一律把 `TOILER_FILE_MANAGER` 指向 `/bin/echo`
//!
//! 真实执行 `open -R` 会在开发机上弹出 Finder 窗口：`cargo test` 变成骚扰，CI
//! 上还会因为无 GUI 而失败。`/bin/echo` 只把参数打出来就退出 0，语义与真实的
//! 成功分支一致（spawn 成功、退出码 0）。
//!
//! ## 远端标记头
//!
//! `lan_proxy::rewrite_request_head` 会把手机与隧道的 `Host` / `Origin` 改写成
//! 回环地址，改完之后与桌面窗口逐字节相同 —— 服务端只看这两项分不出「谁在打」。
//! 判据是代理自己追加的 `x-toiler-remote-client`，客户端自带的同名头会被代理丢掉。

#![allow(clippy::await_holding_lock)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use toiler_server::{catalog::ModelCatalog, routes, state::AppState};
use tower::ServiceExt;

/// 测试里服务监听的地址；`Host` 用它，桌面窗口的 `Origin` 也用它。
const LOOPBACK: &str = "127.0.0.1:30141";

/// 进程级环境变量是全局的，测试之间要串行。
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn acquire_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 让「打开文件管理器」变成一次没有副作用的 `/bin/echo`。
struct FakeFileManager(Option<std::ffi::OsString>);

impl FakeFileManager {
    fn install() -> Self {
        let previous = std::env::var_os("TOILER_FILE_MANAGER");
        std::env::set_var("TOILER_FILE_MANAGER", "/bin/echo");
        Self(previous)
    }
}

impl Drop for FakeFileManager {
    fn drop(&mut self) {
        match self.0.take() {
            Some(value) => std::env::set_var("TOILER_FILE_MANAGER", value),
            None => std::env::remove_var("TOILER_FILE_MANAGER"),
        }
    }
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "toiler-reveal-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(root.join("project")).unwrap();
        Self { root }
    }

    fn project(&self) -> PathBuf {
        self.root.join("project")
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn app(&self) -> axum::Router {
        let catalog = ModelCatalog::from_config(json!({"providers": {}}), None).unwrap();
        // `with_cwd` 把 default_cwd 种进允许根，所以 project 内的路径是授权的。
        let state = AppState::with_cwd(&self.root, catalog, self.project());
        routes::router(state)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn post(router: axum::Router, body: Value, headers: &[(&str, &str)]) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/files/reveal")
        .header("host", LOOPBACK)
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::from(body.to_string())).unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// 桌面窗口真实发出的请求形状（同源回环 Origin）必须真的打开文件管理器。
#[tokio::test]
async fn reveals_a_file_inside_the_allowed_roots() {
    let _guard = acquire_env();
    let _opener = FakeFileManager::install();
    let fixture = Fixture::new("file");
    let file = fixture.project().join("notes.md");
    std::fs::write(&file, b"# hi").unwrap();

    let origin = format!("http://{LOOPBACK}");
    let (status, body) = post(
        fixture.app(),
        json!({ "filePath": file.to_string_lossy() }),
        &[("origin", &origin)],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "ok": true }));
}

/// 目录也要能打开（文件面板里对目录行悬停时点的是同一个按钮）。
#[tokio::test]
async fn reveals_a_directory_too() {
    let _guard = acquire_env();
    let _opener = FakeFileManager::install();
    let fixture = Fixture::new("dir");
    let origin = format!("http://{LOOPBACK}");
    let (status, body) = post(
        fixture.app(),
        json!({ "filePath": fixture.project().to_string_lossy() }),
        &[("origin", &origin)],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// 手机与公网隧道：代理打了标记头就必须拒绝，否则远端能随意弹本机窗口。
#[tokio::test]
async fn remote_clients_cannot_open_the_local_file_manager() {
    let _guard = acquire_env();
    let _opener = FakeFileManager::install();
    let fixture = Fixture::new("remote");
    let file = fixture.project().join("notes.md");
    std::fs::write(&file, b"# hi").unwrap();

    let (status, body) = post(
        fixture.app(),
        json!({ "filePath": file.to_string_lossy() }),
        &[("x-toiler-remote-client", "1")],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("手机"),
        "{body}"
    );
}

/// 允许根之外的路径一律 403（与 `/api/files` 的 GET 同一层判定）。
#[tokio::test]
async fn refuses_paths_outside_the_allowed_roots() {
    let _guard = acquire_env();
    let _opener = FakeFileManager::install();
    let fixture = Fixture::new("outside");
    let outside = std::env::temp_dir().join(format!("toiler-outside-{}", std::process::id()));
    std::fs::create_dir_all(&outside).unwrap();
    let target = outside.join("secret.txt");
    std::fs::write(&target, b"secret").unwrap();

    let (status, body) = post(
        fixture.app(),
        json!({ "filePath": target.to_string_lossy() }),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "Access denied");

    let _ = std::fs::remove_dir_all(&outside);
}

/// 允许根内的符号链接不能逃到根外（否则 403 只是摆设）。
#[cfg(unix)]
#[tokio::test]
async fn refuses_symlinks_that_escape_the_allowed_roots() {
    let _guard = acquire_env();
    let _opener = FakeFileManager::install();
    let fixture = Fixture::new("symlink");
    let outside = fixture.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
    std::os::unix::fs::symlink(&outside, fixture.project().join("link")).unwrap();

    let (status, body) = post(
        fixture.app(),
        json!({ "filePath": fixture.project().join("link/secret.txt").to_string_lossy() }),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// 目标不存在是 404，不是「打开失败 500」：前端能据此说清原因。
#[tokio::test]
async fn missing_targets_are_not_found() {
    let _guard = acquire_env();
    let _opener = FakeFileManager::install();
    let fixture = Fixture::new("missing");
    let (status, body) = post(
        fixture.app(),
        json!({ "filePath": fixture.project().join("nope.md").to_string_lossy() }),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

/// 相对路径、空 body、非绝对路径都必须是 400 —— 绝不拿 cwd 猜一个路径出来。
#[tokio::test]
async fn rejects_relative_and_missing_paths() {
    let _guard = acquire_env();
    let _opener = FakeFileManager::install();
    let fixture = Fixture::new("bad");

    for body in [
        json!({ "filePath": "notes.md" }),
        json!({ "filePath": "" }),
        json!({ "filePath": null }),
        json!({}),
    ] {
        let (status, reply) = post(fixture.app(), body.clone(), &[]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} → {reply}");
    }
}

/// 跨站 Origin 仍然被 `Trusted` 那一层挡住，与远端标记头是两道独立的闸。
#[tokio::test]
async fn foreign_origins_are_rejected_by_the_trusted_layer() {
    let _guard = acquire_env();
    let _opener = FakeFileManager::install();
    let fixture = Fixture::new("origin");
    let file = fixture.project().join("notes.md");
    std::fs::write(&file, b"# hi").unwrap();

    let (status, _) = post(
        fixture.app(),
        json!({ "filePath": file.to_string_lossy() }),
        &[("origin", "https://evil.invalid")],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
