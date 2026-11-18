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
async fn subscription_refresh_requires_login_and_stays_separate_from_api_keys() {
    // 订阅详情与 API Key 详情共用同一颗刷新图标，但订阅令牌唯不能用去请求 `/models`。
    let root = fixture();
    let state = state(&root).with_binding("127.0.0.1", 30142);
    let (status, payload) = call(
        &state,
        "POST",
        "/api/models-config/refresh",
        Some(json!({"providerName":"qoder-cn"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}");
    assert!(payload["error"].as_str().unwrap().contains("未登录"));

    // 其它已登录的 OAuth 供应商：明确说尚未实现，不能拿订阅令牌当 API Key 用。
    state
        .auth()
        .set(
            "anthropic",
            json!({"type":"oauth","access":"oauth-access","refresh":"oauth-refresh"}),
        )
        .unwrap();
    let (status, payload) = call(
        &state,
        "POST",
        "/api/models-config/refresh",
        Some(json!({"providerName":"anthropic"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{payload}");
    assert!(payload["error"].as_str().unwrap().contains("订阅"));

    // 同名自定义 provider：交给导入模型入口，不覆盖用户配置。
    state
        .auth()
        .set(
            "qoder-cn",
            json!({"type":"oauth","uid":"uid-a","access":"a","refresh":"r"}),
        )
        .unwrap();
    state
        .models_file()
        .write(&json!({"providers":{"qoder-cn":{
            "api":"openai-completions", "baseUrl":"https://example.invalid/v1",
            "models":[{"id":"custom"}]
        }}}))
        .unwrap();
    let before = std::fs::read(state.models_file().path()).unwrap();
    let (status, payload) = call(
        &state,
        "POST",
        "/api/models-config/refresh",
        Some(json!({"providerName":"qoder-cn"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}");
    assert!(payload["error"].as_str().unwrap().contains("自定义"));
    assert_eq!(before, std::fs::read(state.models_file().path()).unwrap());
    assert!(!toiler_server::remote_models::store_path(state.models_file().path()).exists());
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

/// 设置页保存 `models.json` 后，新增的 provider 与模型必须当场出现在
/// `/api/models` 的 `modelList` 里。
///
/// 对应前端 `ChatInput.tsx:modelOptions`（它读 `modelList`）。历史故障：
/// `PUT /api/models-config` 只写文件不重建运行时目录，而目录是启动时的快照，
/// 于是用户新加的 provider 在对话框的模型切换器里**一个都不显示**，只有重启
/// 进程才出现 —— 用户看到的是「加了但没生效」。
#[tokio::test]
async fn saving_models_config_exposes_new_providers_to_the_picker() {
    let root = fixture();
    let state = state(&root);
    // 起始状态：只有一家 provider，模型选择器里只有它。
    let (status, _) = call(
        &state,
        "PUT",
        "/api/models-config",
        Some(json!({"providers":{"first":{
            "api":"openai-completions",
            "baseUrl":"https://first.example.test/v1",
            "apiKey":"test-key",
            "models":[{"id":"model-a","name":"Model A"}]
        }}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, visible) = call(&state, "GET", "/api/models", None).await;
    let listed: Vec<&str> = visible["modelList"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert_eq!(listed, vec!["model-a"], "{visible}");

    // 保存第二家 provider + 新模型：两条都要出现在选择器里。
    let (status, body) = call(
        &state,
        "PUT",
        "/api/models-config",
        Some(json!({"providers":{
            "first":{
                "api":"openai-completions",
                "baseUrl":"https://first.example.test/v1",
                "apiKey":"test-key",
                "models":[{"id":"model-a","name":"Model A"}]
            },
            "second":{
                "api":"openai-completions",
                "baseUrl":"https://second.example.test/v1",
                "apiKey":"test-key",
                "models":[{"id":"model-b","name":"Model B"}]
            }
        }})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (_, visible) = call(&state, "GET", "/api/models", None).await;
    let pairs: Vec<(String, String)> = visible["modelList"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["provider"].as_str().unwrap().to_string(),
                item["id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(
        pairs.contains(&("second".to_string(), "model-b".to_string())),
        "新增 provider 的模型必须出现在 modelList 里：{visible}"
    );
    assert!(
        pairs.contains(&("first".to_string(), "model-a".to_string())),
        "老模型不能被弄丢：{visible}"
    );
    // `models` 字典与 `modelList` 必须同步（两者分隔符不同但内容一致）。
    assert!(visible["models"]["second:model-b"].is_string(), "{visible}");
    assert_eq!(
        visible["models"].as_object().unwrap().len(),
        visible["modelList"].as_array().unwrap().len()
    );

    // 目录里也要真的能解析出 rpi 的 `Model`：只更新 modelList 而没建 provider
    // 的话，界面能选中但发请求会报 `No provider registered`。
    assert!(state.catalog().model_for("second", "model-b").is_some());
    assert!(state
        .catalog()
        .providers
        .iter()
        .any(|provider| provider.id() == "second"));
    std::fs::remove_dir_all(root).unwrap();
}

/// 删除 provider / 模型后，`/api/models` 也必须当场摘掉它。
///
/// 否则 `modelList` 里留着已删项，选中就报 `Model not found`
/// （`LiveAgent::set_model` 从目录里查不到模型）。
#[tokio::test]
async fn deleting_models_config_removes_them_from_the_picker() {
    let root = fixture();
    let state = state(&root);
    let (status, _) = call(
        &state,
        "PUT",
        "/api/models-config",
        Some(json!({"providers":{
            "first":{
                "api":"openai-completions",
                "baseUrl":"https://first.example.test/v1",
                "apiKey":"test-key",
                "models":[{"id":"model-a"},{"id":"model-b"}]
            },
            "second":{
                "api":"openai-completions",
                "baseUrl":"https://second.example.test/v1",
                "apiKey":"test-key",
                "models":[{"id":"model-c"}]
            }
        }})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 删单个模型：另一条必须留下。
    let (status, body) = call(
        &state,
        "DELETE",
        "/api/models-config",
        Some(json!({"kind":"model","provider":"first","modelId":"model-a"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, visible) = call(&state, "GET", "/api/models", None).await;
    let ids: Vec<&str> = visible["modelList"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert!(!ids.contains(&"model-a"), "已删模型不能还在：{visible}");
    assert!(ids.contains(&"model-b"), "{visible}");
    assert!(state.catalog().model_for("first", "model-a").is_none());

    // 删整个 provider：它的模型全部消失，provider 实例也要摘掉。
    let (status, body) = call(
        &state,
        "DELETE",
        "/api/models-config",
        Some(json!({"kind":"provider","provider":"second"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, visible) = call(&state, "GET", "/api/models", None).await;
    assert!(
        !visible["modelList"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["provider"] == "second"),
        "已删 provider 不能还在：{visible}"
    );
    assert!(state.catalog().model_for("second", "model-c").is_none());
    assert!(!state
        .catalog()
        .providers
        .iter()
        .any(|provider| provider.id() == "second"));
    std::fs::remove_dir_all(root).unwrap();
}
