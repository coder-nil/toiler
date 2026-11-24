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
    request(app, method, uri, origin, None).await
}

async fn request(
    app: Router,
    method: &str,
    uri: &str,
    origin: Option<&str>,
    json: Option<&Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1:30141");
    if let Some(origin) = origin {
        request = request.header("origin", origin);
    }
    let body = if let Some(json) = json {
        request = request.header("content-type", "application/json");
        Body::from(json.to_string())
    } else {
        Body::empty()
    };
    let response = app.oneshot(request.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn checkpoint_calls_return_explicit_unsupported_not_404() {
    // useAgentSession 创建/完成 checkpoint；`POST /api/files/reveal` 的契约另见 `reveal_contract.rs`。
    let fixture = Fixture::new();
    let (status, body) = call(fixture.app(), "POST", "/api/checkpoints", None).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert!(body["error"].as_str().unwrap().contains("暂不支持"));
    assert!(body.get("success").is_none());
    let (status, _) = call(
        fixture.app(),
        "POST",
        "/api/checkpoints",
        Some("https://evil.invalid"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
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
async fn skills_management_requires_authorized_cwd_and_project_trust() {
    let fixture = Fixture::new();
    let project = fixture.0.join("project");
    std::fs::create_dir_all(project.join(".agents/skills")).unwrap();
    for action in ["check", "update", "remove"] {
        let path = format!("/api/skills/{action}");
        let body = json!({"cwd": project, "scope": "project", "package": "owner/repo@demo"});
        let (status, _) = request(
            fixture.app(),
            "POST",
            &path,
            Some("https://evil.invalid"),
            Some(&body),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        let (status, reply) = request(fixture.app(), "POST", &path, None, Some(&body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(reply["error"], "Project resources require trust");

        let outside = json!({"cwd": fixture.0, "scope": "project", "package": "owner/repo@demo"});
        let (status, reply) = request(fixture.app(), "POST", &path, None, Some(&outside)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(reply["error"], "Access denied");
    }
}

#[tokio::test]
async fn market_skill_can_be_removed_but_manual_skill_is_preserved() {
    // SkillsConfig 使用 cwd、包标识和作用域卸载，并重新请求技能列表。
    let fixture = Fixture::new();
    let project = fixture.0.join("project");
    std::fs::create_dir(project.join(".git")).unwrap();
    let root = project.join(".agents/skills");
    for name in ["demo", "manual"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(
            root.join(name).join("SKILL.md"),
            format!("---\nname: {name}\ndescription: 测试技能\n---\n"),
        )
        .unwrap();
    }
    std::fs::write(
        root.join("demo/.toiler-skill.json"),
        json!({
            "package": "owner/repo@demo", "scope": "project", "source": "owner/repo",
            "sourceType": "github", "skillsShUrl": "https://skills.sh/owner/repo/demo",
            "skillPath": "demo", "versionHash": "a".repeat(64), "canCheckForUpdates": false
        })
        .to_string(),
    )
    .unwrap();
    TrustStore::at(fixture.0.join("trust.json"))
        .set(project.to_str().unwrap(), Some(true))
        .unwrap();
    let cwd = project.join("src");
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("cwd", &cwd.to_string_lossy())
        .finish();
    let (status, list) = call(fixture.app(), "GET", &format!("/api/skills?{query}"), None).await;
    assert_eq!(status, StatusCode::OK);
    let installed = list["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|skill| skill["install"]["package"] == "owner/repo@demo")
        .unwrap();
    assert_eq!(installed["install"]["canCheckForUpdates"], true);

    for (package, expected) in [
        ("owner/repo@../demo", StatusCode::BAD_REQUEST),
        ("elsewhere/repo@demo", StatusCode::NOT_FOUND),
        ("owner/repo@manual", StatusCode::NOT_FOUND),
        ("owner/repo@demo", StatusCode::OK),
        ("owner/repo@demo", StatusCode::NOT_FOUND),
    ] {
        let body = json!({"cwd": cwd, "scope": "project", "package": package});
        let (status, reply) = request(
            fixture.app(),
            "POST",
            "/api/skills/remove",
            None,
            Some(&body),
        )
        .await;
        assert_eq!(status, expected, "{reply}");
        if status == StatusCode::OK {
            assert_eq!(reply, json!({"success": true}));
        }
    }
    assert!(!root.join("demo").exists());
    assert!(root.join("manual/SKILL.md").is_file());
    let (status, checked) = request(
        fixture.app(),
        "POST",
        "/api/skills/check",
        None,
        Some(&json!({"cwd": cwd, "scope": "project"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(checked, json!({"updates": []}));
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
