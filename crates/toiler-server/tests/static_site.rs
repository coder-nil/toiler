//! 阶段 4 的端到端检查：静态前端与 `/api` 在**同一个 Router**里共存。
//!
//! 这里用的是编译期嵌入的那份产物（默认是 `crates/workman-server/frontend/` 占位页，
//! 也可以是 `WORKMAN_FRONTEND_DIR` 指向的真实 Next 导出）。断言只依赖「产物里一定
//! 有 index.html」这一点，这样换产物不用改测试。

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use tower::util::ServiceExt;
use workman_server::{catalog::ModelCatalog, routes, state::AppState};

fn fixture_root(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "workman-static-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn app(root: &std::path::Path) -> axum::Router {
    app_with_cwd(root, std::path::Path::new("/tmp/demo"))
}

fn app_with_cwd(root: &std::path::Path, cwd: &std::path::Path) -> axum::Router {
    let catalog = ModelCatalog::from_config(serde_json::json!({ "providers": {} }), None).unwrap();
    let state = AppState::with_cwd(root, catalog, cwd);
    routes::router(state)
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

async fn fetch(router: axum::Router, uri: &str, headers: &[(&str, &str)]) -> Reply {
    let mut builder = Request::builder().uri(uri).header("host", "127.0.0.1:8787");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    Reply {
        status,
        headers,
        body,
    }
}

#[tokio::test]
async fn root_serves_the_embedded_index() {
    let root = fixture_root("root");
    let reply = fetch(app(&root), "/", &[("accept", "text/html")]).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.headers.get("content-type").unwrap(),
        "text/html; charset=utf-8"
    );
    assert_eq!(reply.headers.get("cache-control").unwrap(), "no-cache");
    assert!(reply.text().contains("<html") || reply.text().contains("<!doctype"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn api_routes_are_not_swallowed_by_the_static_fallback() {
    let root = fixture_root("api");
    let reply = fetch(app(&root), "/api/sessions", &[]).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(
        reply
            .headers
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("application/json"),
        "`/api/*` 必须是 JSON，不能被静态兜底接管"
    );
    assert!(reply.text().contains("\"sessions\""));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn unknown_api_path_is_404_json_not_index_html() {
    let root = fixture_root("api404");
    // 浏览器/curl 都会带 `Accept: */*` 或 `text/html`；这两种都不能把
    // `/api/*` 当成客户端路由去兜底成 index.html。
    for accept in ["*/*", "text/html", "application/json"] {
        let reply = fetch(app(&root), "/api/does-not-exist", &[("accept", accept)]).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "accept={accept}");
        assert!(
            reply.text().contains("\"error\""),
            "accept={accept}: {}",
            reply.text()
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn client_route_falls_back_to_index_for_html_navigations() {
    let root = fixture_root("spa");
    let reply = fetch(app(&root), "/sessions/abc", &[("accept", "text/html")]).await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "客户端路由应交由 index.html 接管"
    );
    assert_eq!(
        reply.headers.get("content-type").unwrap(),
        "text/html; charset=utf-8"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn missing_static_asset_is_404() {
    let root = fixture_root("asset404");
    let reply = fetch(
        app(&root),
        "/_next/static/chunks/nope.js",
        &[("accept", "*/*")],
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn traversal_attempts_are_rejected() {
    let root = fixture_root("traversal");
    for uri in [
        "/%2e%2e/%2e%2e/etc/passwd",
        "/..%2f..%2fetc%2fpasswd",
        "/a/../../../../etc/passwd",
    ] {
        let reply = fetch(app(&root), uri, &[("accept", "text/html")]).await;
        assert_eq!(
            reply.status,
            StatusCode::NOT_FOUND,
            "{uri} 不应命中任何文件"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn rsc_requests_never_get_an_api_payload() {
    let root = fixture_root("rsc");
    // 占位产物里没有 .txt 载荷：RSC 请求要么拿到 index.html（客户端接管），
    // 要么 404 —— 无论如何不能返回 /api 的 JSON。
    let reply = fetch(
        app(&root),
        "/some/route?_rsc=abc",
        &[("rsc", "1"), ("accept", "text/x-component")],
    )
    .await;
    assert_ne!(
        reply
            .headers
            .get("content-type")
            .map(|v| v.to_str().unwrap()),
        Some("application/json")
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn empty_files_path_lists_the_default_cwd_instead_of_404() {
    // 前端文件面板首屏请求的是根路径；它必须落在 `/api/files` 上，
    // 而不是被静态兜底吃掉（否则面板直接显示 "Not found"）。
    let root = fixture_root("filesroot");
    std::fs::write(root.join("hello.txt"), "hi").unwrap();
    let reply = fetch(
        app_with_cwd(&root, &root),
        "/api/files",
        &[("accept", "*/*")],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(body["isDirectory"], true, "{}", reply.text());
    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect();
    assert!(names.contains(&"hello.txt"), "{names:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn files_endpoint_accepts_slashless_absolute_paths() {
    // pi 前端把绝对路径去掉前导 `/` 后拼进 URL：`/api/files/Users/me/proj?type=list`。
    // 这条形状不认的话，界面的文件面板会直接显示 "Not found"。
    let root = fixture_root("fileshape");
    let project = root.join("proj");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("README.md"), "# hi").unwrap();

    let mut uri = String::from("/api/files/");
    uri.push_str(project.to_string_lossy().trim_start_matches('/'));
    uri.push_str("?type=list");

    let reply = fetch(
        app_with_cwd(&root, &root),
        &uri,
        &[("accept", "application/json")],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    let entries = body["entries"].as_array().unwrap();
    let src = entries
        .iter()
        .find(|entry| entry["name"] == "src")
        .expect("src 目录应出现在列举结果里");
    assert_eq!(src["isDir"], true, "pi 前端读 `isDir`：{}", src);
    let _ = std::fs::remove_dir_all(&root);
}
