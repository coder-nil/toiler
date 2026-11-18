//! 模型设置页的「刷新模型列表」回归；所有配置均落在临时目录。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use toiler_server::{catalog::ModelCatalog, routes, state::AppState};
use tower::ServiceExt;

fn fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!("toiler-model-refresh-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn state(root: &Path) -> AppState {
    let models = toiler_server::models_json::ModelsFile::at(root.join("models.json"));
    let catalog =
        ModelCatalog::from_config(models.read(), Some(models.path().to_path_buf())).unwrap();
    AppState::with_cwd(root.join("sessions"), catalog, root).with_stores(
        toiler_server::auth::AuthStore::new(root.join("auth.json")),
        models,
        toiler_server::settings::SettingsFile::at(root.join("settings.json")),
        toiler_server::projects::ProjectStore::at(root.join("work.sqlite")),
        toiler_server::trust::TrustStore::at(root.join("trust.json")),
    )
}

async fn call(
    state: &AppState,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body(
            body.map(|value| Body::from(value.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    let response = routes::router(state.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// 仅模拟被请求的上游；被测应用 Router 始终通过 oneshot 调用，不监听端口。
fn upstream(
    responses: Vec<(&'static str, Value)>,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let size = stream.read(&mut buf).unwrap();
                assert!(size > 0);
                request.extend_from_slice(&buf[..size]);
            }
            requests.push(String::from_utf8(request).unwrap());
            let body = body.to_string();
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
        requests
    });
    (base, handle)
}

#[tokio::test]
async fn refresh_fetches_real_models_preserves_user_configuration_and_scope() {
    // ModelsConfig 的 API Key 详情刷新后，EnabledModelsPicker 和对话模型列表都要更新。
    let root = fixture();
    let (base, upstream) = upstream(vec![
        (
            "200 OK",
            json!({"data":[{"id":"new","name":"New model"},{"id":"old","name":"Upstream name"}]}),
        ),
        ("200 OK", json!({"data":[{"id":"latest"}]})),
        ("502 Bad Gateway", json!({"error":"upstream unavailable"})),
    ]);
    let state = state(&root);
    state
        .models_file()
        .write(&json!({"providers":{"openai":{
            "api":"openai-completions", "baseUrl":base,
            "headers":{"x-test-header":"kept"},
            "models":[{"id":"old","name":"User name","contextWindow":200000}]
        }}}))
        .unwrap();
    state
        .auth()
        .set(
            "openai",
            json!({"type":"api_key","key":"test-key-not-a-secret"}),
        )
        .unwrap();
    state
        .settings()
        .set_enabled_models(Some(vec!["openai/old".into()]))
        .unwrap();
    let original_models = std::fs::read(state.models_file().path()).unwrap();
    let original_settings = std::fs::read(root.join("settings.json")).unwrap();

    let (status, body) = call(
        &state,
        "POST",
        "/api/models-config/refresh",
        Some(json!({"providerName":"openai"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["models"].as_array().unwrap().len(), 2);
    assert!(!body.to_string().contains("test-key-not-a-secret"));
    let catalog = state.catalog();
    assert_eq!(
        catalog.model_for("openai", "old").unwrap().name,
        "User name"
    );
    assert!(catalog.model_for("openai", "new").is_some());
    assert!(catalog
        .providers
        .iter()
        .any(|provider| provider.id() == "openai" && provider.models().len() == 2));
    let (_, enabled) = call(
        &state,
        "GET",
        "/api/models-config/enabled?provider=openai",
        None,
    )
    .await;
    assert_eq!(enabled["models"].as_array().unwrap().len(), 2);
    assert!(enabled["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|model| model["id"] == "new" && model["selected"] == false));
    let (_, visible) = call(&state, "GET", "/api/models", None).await;
    assert_eq!(visible["modelList"].as_array().unwrap().len(), 1);
    assert_eq!(visible["modelList"][0]["name"], "User name");

    let (status, _) = call(
        &state,
        "POST",
        "/api/models-config/refresh",
        Some(json!({"providerName":"openai"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        state.catalog().model_for("openai", "new").is_none(),
        "旧远端模型应被替换，而不是无限追加"
    );
    assert!(state.catalog().model_for("openai", "latest").is_some());
    assert!(
        state.catalog().model_for("openai", "old").is_some(),
        "用户模型不应因上游下架而删除"
    );
    let before_failure = std::fs::read(toiler_server::remote_models::store_path(
        state.models_file().path(),
    ))
    .unwrap();
    let (status, body) = call(
        &state,
        "POST",
        "/api/models-config/refresh",
        Some(json!({"providerName":"openai"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(
        before_failure,
        std::fs::read(toiler_server::remote_models::store_path(
            state.models_file().path()
        ))
        .unwrap()
    );
    assert!(state.catalog().model_for("openai", "latest").is_some());
    assert_eq!(
        original_models,
        std::fs::read(state.models_file().path()).unwrap()
    );
    assert_eq!(
        original_settings,
        std::fs::read(root.join("settings.json")).unwrap()
    );
    for request in upstream.join().unwrap() {
        assert!(request.starts_with("GET /v1/models HTTP/1.1"));
        assert!(request
            .to_lowercase()
            .contains("authorization: bearer test-key-not-a-secret"));
        assert!(request.to_lowercase().contains("x-test-header: kept"));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cached_models_survive_restart_and_disappear_when_key_is_removed() {
    // API Key 详情断开后，不能继续展示无凭据的远端模型；重启则应恢复已刷新目录。
    let root = fixture();
    let state = state(&root);
    state
        .auth()
        .set("minimax", json!({"type":"api_key","key":"fake"}))
        .unwrap();
    toiler_server::remote_models::write(
        state.models_file().path(),
        "minimax",
        "anthropic-messages",
        "https://example.invalid/anthropic",
        &[json!({"id":"new-chat"})],
    )
    .unwrap();
    state.attach_builtin_providers();
    assert!(state.catalog().model_for("minimax", "new-chat").is_some());
    assert!(state
        .catalog()
        .providers
        .iter()
        .any(|provider| provider.id() == "minimax"));
    state.attach_builtin_providers();
    assert_eq!(
        state
            .catalog()
            .entries
            .iter()
            .filter(|entry| entry.provider == "minimax")
            .count(),
        1
    );
    let restarted = self::state(&root);
    restarted.attach_builtin_providers();
    assert!(restarted
        .catalog()
        .model_for("minimax", "new-chat")
        .is_some());
    let (status, _) = call(&restarted, "DELETE", "/api/auth/api-key/minimax", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(restarted
        .catalog()
        .model_for("minimax", "new-chat")
        .is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn refresh_validates_credentials_protocol_and_request_trust_without_network() {
    // 未保存 Key、非内置供应商、未支持协议和跨源请求均须显式报错，不能返回假成功。
    let root = fixture();
    let state = state(&root);
    for (body, expected) in [
        (json!({}), StatusCode::BAD_REQUEST),
        (json!({"providerName":"unknown"}), StatusCode::BAD_REQUEST),
        (json!({"providerName":"openai"}), StatusCode::BAD_REQUEST),
        (json!({"providerName":"apisets"}), StatusCode::BAD_REQUEST),
        (
            json!({"providerName":"google"}),
            StatusCode::NOT_IMPLEMENTED,
        ),
        (
            json!({"providerName":"amazon-bedrock"}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, payload) =
            call(&state, "POST", "/api/models-config/refresh", Some(body)).await;
        assert_eq!(status, expected, "{payload}");
        assert!(payload["error"].is_string());
    }
    let request = Request::builder()
        .method("POST")
        .uri("/api/models-config/refresh")
        .header("host", "127.0.0.1")
        .header("origin", "https://untrusted.invalid")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"providerName":"openai"}"#))
        .unwrap();
    assert_eq!(
        routes::router(state.clone())
            .oneshot(request)
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let _guard = state.inner.models_refresh_gate.lock().await;
    let (status, _) = call(
        &state,
        "POST",
        "/api/models-config/refresh",
        Some(json!({"providerName":"openai"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(!toiler_server::remote_models::store_path(state.models_file().path()).exists());
    std::fs::remove_dir_all(root).unwrap();
}
