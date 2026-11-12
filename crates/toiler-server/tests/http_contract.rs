//! HTTP 契约测试：形状与 pi-desktop 的 `/api` 对齐（见 docs/contracts/README.md）。
//!
//! 用 `tower::ServiceExt::oneshot` 直接打 Router，不起真实端口。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use toiler_server::catalog::ModelCatalog;
use toiler_server::routes;
use toiler_server::state::AppState;
use tower::ServiceExt;

const HEADER: &str = r#"{"type":"session","version":3,"id":"sess-a","timestamp":"2026-11-10T06:41:44.431Z","cwd":"/tmp/demo"}"#;

fn fixture_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("toiler-http-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("--tmp-demo--");
    std::fs::create_dir_all(&dir).unwrap();
    let body = format!(
        "{HEADER}\n{}\n{}\n{}\n",
        r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-11-10T06:41:45.000Z","message":{"role":"user","content":[{"type":"text","text":"我需要缓存方案"}],"timestamp":1794292905000}}"#,
        r#"{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-11-10T06:41:46.000Z","message":{"role":"assistant","content":[{"type":"text","text":"用 Redis 缓存"}],"api":"openai-completions","provider":"x","model":"y","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"reasoning":0,"totalTokens":2,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":1794292906000}}"#,
        r#"{"type":"session_info","id":"m3","parentId":"m2","timestamp":"2026-11-10T06:41:47.000Z","name":"缓存讨论"}"#,
    );
    std::fs::write(dir.join("2026-11-10T06-41-44-431Z_sess-a.jsonl"), body).unwrap();
    root
}

fn app(root: &std::path::Path) -> axum::Router {
    app_with_cwd(root, std::path::Path::new("/tmp/demo"))
}

fn app_with_cwd(root: &std::path::Path, cwd: &std::path::Path) -> axum::Router {
    let catalog = ModelCatalog::from_config(serde_json::json!({"providers": {}}), None).unwrap();
    let state = AppState::with_cwd(root, catalog, cwd);
    routes::router(state)
}

async fn get(router: axum::Router, uri: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .uri(uri)
        .header("host", "127.0.0.1:8787")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn sessions_list_matches_contract_fields() {
    let root = fixture_root("list");
    let (status, body) = get(app(&root), "/api/sessions").await;
    assert_eq!(status, StatusCode::OK);

    let sessions = body["sessions"].as_array().expect("sessions 数组");
    assert_eq!(sessions.len(), 1);
    let item = &sessions[0];
    for field in [
        "path",
        "id",
        "cwd",
        "name",
        "created",
        "modified",
        "messageCount",
        "firstMessage",
        "parentSessionId",
        "transient",
        "projectRoot",
        "projectKey",
    ] {
        assert!(item.get(field).is_some(), "缺少字段 {field}");
    }
    assert_eq!(item["id"], "sess-a");
    assert_eq!(item["name"], "缓存讨论");
    assert_eq!(item["messageCount"], 2);
    assert_eq!(item["firstMessage"], "我需要缓存方案");
    assert!(body.get("sessionsUpdatedAt").is_some());
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn session_detail_matches_contract() {
    let root = fixture_root("detail");
    let (status, body) = get(app(&root), "/api/sessions/sess-a?limit=5").await;
    assert_eq!(status, StatusCode::OK);

    for field in [
        "sessionId",
        "filePath",
        "info",
        "leafId",
        "tree",
        "context",
        "paging",
        "totalActiveMs",
    ] {
        assert!(body.get(field).is_some(), "缺少字段 {field}");
    }
    assert_eq!(body["paging"]["pageSize"], 5);
    assert_eq!(body["paging"]["totalMessages"], 2);
    assert_eq!(body["context"]["messages"].as_array().unwrap().len(), 2);
    assert_eq!(body["context"]["entryIds"].as_array().unwrap().len(), 2);
    assert_eq!(body["tree"].as_array().unwrap().len(), 1);
    assert!(body["totalActiveMs"].as_i64().unwrap() >= 1000);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn unknown_session_is_404() {
    let root = fixture_root("404");
    let (status, body) = get(app(&root), "/api/sessions/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "Session not found");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn search_matches_session_and_message_text() {
    let root = fixture_root("search");
    let (status, body) = get(app(&root), "/api/sessions/search?q=%E7%BC%93%E5%AD%98").await;
    assert_eq!(status, StatusCode::OK);
    let matches = body["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1, "应命中 1 个会话：{body}");
    assert_eq!(matches[0]["sessionId"], "sess-a");
    assert!(matches[0]["snippet"].as_str().unwrap().contains("缓存"));

    // 空查询直接返回空数组（pi 的行为）
    let (_, empty) = get(app(&root), "/api/sessions/search?q=").await;
    assert!(empty["matches"].as_array().unwrap().is_empty());
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn untrusted_host_is_403() {
    let root = fixture_root("forbidden");
    let request = Request::builder()
        .uri("/api/sessions")
        .header("host", "evil.example.com")
        .body(Body::empty())
        .unwrap();
    let response = app(&root).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "Untrusted API request");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn models_endpoint_reports_empty_catalog_cleanly() {
    // 空目录也要给出 pi 的完整形状（前端对 `undefined` 和 `{}` 的容错不同），
    // 所以这里断言的是「键都在且为空」而不是「字段都没有」。
    let root = fixture_root("models");
    let (status, body) = get(app(&root), "/api/models").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["models"].as_object().unwrap().len(), 0, "{body}");
    assert!(body["modelList"].as_array().unwrap().is_empty(), "{body}");
    assert!(body["defaultModel"].is_null(), "{body}");
    assert!(body["thinkingLevels"].as_object().unwrap().is_empty());
    assert!(body["thinkingLevelPins"].as_object().unwrap().is_empty());
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn mobile_state_reports_session_and_queue() {
    let root = fixture_root("mobile");
    let (status, body) = get(app(&root), "/api/mobile/state").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sessionId"], "sess-a");
    assert_eq!(body["status"], "idle");
    assert_eq!(body["queue"]["steering"], 0);
    assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    assert_eq!(body["earlierCount"], 0);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_endpoint_refuses_paths_outside_allowed_roots() {
    // `/tmp/demo` 不在任何会话 cwd 里，也不是默认 cwd 的子路径 ——
    // pi 的 `getAllowedFileRoots` 在这一层直接 403（不信存在性）。
    let root = fixture_root("files");
    let (status, body) = get(app(&root), "/api/files/tmp/demo/none.txt").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "Access denied");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_endpoint_reports_404_inside_allowed_roots() {
    // 允许根内但文件不存在：这才是 404（区分「越权」与「没有」）。
    let root = fixture_root("files404");
    let missing = root.join("nope.txt");
    let uri = format!(
        "/api/files/{}",
        missing.to_string_lossy().trim_start_matches('/')
    );
    let (status, body) = get(app_with_cwd(&root, &root), &uri).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "Not found");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn agent_events_for_unknown_session_is_404() {
    let root = fixture_root("agent404");
    let request = Request::builder()
        .uri("/api/agent/nope/events")
        .header("host", "127.0.0.1:8787")
        .body(Body::empty())
        .unwrap();
    let response = app(&root).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn agent_prompt_for_unknown_session_is_rejected() {
    let root = fixture_root("agent-post");
    let request = Request::builder()
        .method("POST")
        .uri("/api/agent/nope")
        .header("host", "127.0.0.1:8787")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"type":"prompt","message":"hi"}"#))
        .unwrap();
    let response = app(&root).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["code"], "prompt_rejected");
    assert_eq!(body["accepted"], false);
    std::fs::remove_dir_all(&root).ok();
}
