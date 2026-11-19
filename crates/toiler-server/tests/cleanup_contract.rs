//! 对应 ChatInput 的 @文件补全和保留的 pi 写侧调用；所有 Store 均隔离到临时目录。

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::PathBuf;
use toiler_server::{
    auth::AuthStore, catalog::ModelCatalog, models_json::ModelsFile, projects::ProjectStore,
    routes, settings::SettingsFile, state::AppState, trust::TrustStore,
};
use tower::ServiceExt;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("toiler-cleanup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(path.join("project/src")).unwrap();
        Self(path)
    }
    fn app(&self) -> Router {
        let catalog = ModelCatalog::from_config(json!({"providers":{}}), None).unwrap();
        routes::router(
            AppState::with_cwd(self.0.join("sessions"), catalog, self.0.join("project"))
                .with_stores(
                    AuthStore::new(self.0.join("auth.json")),
                    ModelsFile::at(self.0.join("models.json")),
                    SettingsFile::at(self.0.join("settings.json")),
                    ProjectStore::at(self.0.join("work.sqlite")),
                    TrustStore::at(self.0.join("trust.json")),
                ),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn call(app: Router, method: &str, uri: &str, origin: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1:30141");
    if let Some(origin) = origin {
        request = request.header("origin", origin);
    }
    let response = app
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn retained_pi_calls_return_explicit_unsupported_not_404() {
    // useAgentSession 创建/完成 checkpoint；SkillsConfig 检查/更新/卸载技能。
    // （`POST /api/files/reveal` 已经实现，契约在 `reveal_contract.rs`。）
    let fixture = Fixture::new();
    for path in [
        "/api/checkpoints",
        "/api/skills/check",
        "/api/skills/update",
        "/api/skills/remove",
    ] {
        let (status, body) = call(fixture.app(), "POST", path, None).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{path}");
        assert!(body["error"].as_str().unwrap().contains("暂不支持"));
        assert!(body.get("success").is_none());
        let (status, _) = call(fixture.app(), "POST", path, Some("https://evil.invalid")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
    }
    let (status, _) = call(
        fixture.app(),
        "GET",
        "/api/checkpoints?sessionId=test",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    // 不发送真实网络请求，仅验证更新入口的同源保护。
    let (status, _) = call(
        fixture.app(),
        "GET",
        "/api/app-update",
        Some("https://evil.invalid"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn at_file_index_lists_searches_and_rejects_outside_roots() {
    // ChatInput 使用 files/truncated 首屏和 matches 服务端搜索，不允许索引任意目录。
    let fixture = Fixture::new();
    let project = fixture.0.join("project");
    std::fs::write(project.join("src/ChatInput.tsx"), "测试").unwrap();
    std::fs::create_dir(project.join("node_modules")).unwrap();
    std::fs::write(project.join("node_modules/hidden.js"), "不应扫描").unwrap();
    std::fs::write(project.join(" 带空格.txt"), "测试").unwrap();
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("cwd", &project.to_string_lossy())
        .finish();
    let (status, body) = call(
        fixture.app(),
        "GET",
        &format!("/api/file-index?{query}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["truncated"], false);
    assert!(body["files"]
        .as_array()
        .unwrap()
        .contains(&json!("src/ChatInput.tsx")));
    assert!(body["files"]
        .as_array()
        .unwrap()
        .contains(&json!(" 带空格.txt")));
    assert!(!body.to_string().contains("hidden.js"));
    let (status, body) = call(
        fixture.app(),
        "GET",
        &format!("/api/file-index?{query}&q=chinp"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["matches"][0],
        json!({"path":"src/ChatInput.tsx", "isDir":false})
    );
    let (status, _) = call(fixture.app(), "GET", "/api/file-index?cwd=relative", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        fixture.app(),
        "GET",
        &format!("/api/file-index?cwd={}", fixture.0.display()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[cfg(unix)]
#[tokio::test]
async fn index_never_traverses_external_directory_symlinks() {
    let fixture = Fixture::new();
    let external = fixture.0.join("external");
    std::fs::create_dir(&external).unwrap();
    std::fs::write(external.join("secret.txt"), "不该可见").unwrap();
    std::os::unix::fs::symlink(&external, fixture.0.join("project/link")).unwrap();
    let (status, body) = call(
        fixture.app(),
        "GET",
        &format!(
            "/api/file-index?cwd={}",
            fixture.0.join("project").display()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.to_string().contains("secret.txt"));
    let (status, _) = call(
        fixture.app(),
        "GET",
        &format!(
            "/api/file-index?cwd={}",
            fixture.0.join("project/link").display()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
