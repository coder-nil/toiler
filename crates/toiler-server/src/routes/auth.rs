//! `/api/auth/*` —— API Key 的存取与 provider 列表。
//!
//! | route | 行为 |
//! |---|---|
//! | `GET /api/auth/providers` | **OAuth / 订阅**列表（面板「订阅」区，含 anthropic） |
//! | `GET /api/auth/all-providers` | **API Key** 列表（面板「API Key」区，42 家里 41 家） |
//!
//! 两个列表的供应商来自**写死的内置表**（[`crate::builtin_providers`]，pi SDK
//! 的 provider 目录），不是 `models.json`；`models.json` 只用来叠加自定义 provider
//! 与 `apiKey` 配置状态。两个 route 的字段形状照抄 pi 的 `lib/provider-listing.ts`
//! （`buildOAuthProviderList` / `buildApiKeyProviderList`）—— **别把两个列表对调**，
//! 前端 `ModelsConfig.tsx` 就是按 pi 的形状取字段的。
//! | `GET /api/auth/api-key/{provider}` | 配置状态 + 掩码后的 key |
//! | `POST /api/auth/api-key/{provider}` | `{apiKey}` → 写入 `auth.json` |
//! | `DELETE /api/auth/api-key/{provider}` | 删掉 api_key 凭据 |
//! | `POST /api/auth/logout/{provider}` | 删掉 oauth 凭据 |
//! | `POST /api/auth/login/{provider}` | **不支持**（rpi 没有 OAuth 登录流程） |
//!
//! 裸密码 / 明文 key **永不出现在响应里**：`GET` 只给
//! `••••1234`（长度 < 12 时全掩）。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use std::collections::BTreeSet;
use std::convert::Infallible;

use axum::response::sse::{Event, Sse};

use crate::auth::{mask_key, provider_status, AuthStore, RemoveResult};
use crate::builtin_providers::{self, BuiltinProvider};
use crate::models_json::{self, provider_config, provider_display_name, provider_model_ids};
use crate::security::Trusted;
use crate::state::AppState;

/// models.json 里 `apiKey` 的来源；这些 provider 不该出现在「用 API Key 登录」列表里
/// （pi 的 `89950.d`）。它们已经以「models.json 里的 provider」出现在面板左侧。
const NON_LOGIN_SOURCES: [&str; 2] = ["models_json_key", "models_json_command"];

/// 面板上「一家 provider」的原始输入（pi 的 `ProviderListingInput`）。
struct ListingInput {
    id: String,
    name: String,
    base_url: Option<String>,
    has_api_key: bool,
    has_oauth: bool,
    oauth_name: Option<String>,
    configured: bool,
    source: Option<&'static str>,
    credential_type: Option<String>,
    model_count: usize,
}

/// 内置目录 ∪ `models.json` 自建 provider，并与 `auth.json` / `models.json`
/// 的凭据状态合并 —— pi 的 `collectProviderListingInputs`。
fn listing_inputs(state: &AppState) -> Vec<ListingInput> {
    let models = state.models_file().read();
    let auth = state.auth();
    let mut rows: Vec<ListingInput> = builtin_providers::all()
        .map(|provider| {
            let config = provider_config(&models, provider.id);
            let status = provider_status(auth, provider.id, config);
            ListingInput {
                id: provider.id.to_string(),
                name: provider.name.to_string(),
                // models.json 里的 baseUrl 优先（用户可能指向自己的代理）
                base_url: config
                    .and_then(|c| c.get("baseUrl"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| provider.base_url.map(str::to_string)),
                has_api_key: provider.api_key,
                has_oauth: provider.oauth.is_some(),
                oauth_name: provider.oauth.map(str::to_string),
                configured: status.configured,
                source: status.source,
                credential_type: auth.credential_type(provider.id),
                model_count: model_count(&models, provider.id, Some(provider)),
            }
        })
        .collect();

    // `models.json` 里自建的 provider：pi 也把它们注册进目录，这里只补内置表没有的
    // （同名时上面那条已经带上了它的 baseUrl / 凭据状态）。
    for (id, config) in models_json::providers(&models) {
        if builtin_providers::find(&id).is_some() {
            continue;
        }
        let status = provider_status(auth, &id, Some(&config));
        rows.push(ListingInput {
            name: provider_display_name(&models, &id),
            base_url: config
                .get("baseUrl")
                .and_then(Value::as_str)
                .map(str::to_string),
            has_api_key: true,
            has_oauth: false,
            oauth_name: None,
            configured: status.configured,
            source: status.source,
            credential_type: auth.credential_type(&id),
            model_count: model_count(&models, &id, None),
            id,
        });
    }
    rows
}

/// 卡片上的「N models」：内置目录里的模型 ∪ `models.json` 同名 provider 的模型。
///
/// pi 的 `ModelRuntime.getModels()` 会把两者合并（同 id 去重），这里靠内置表里的
/// id 列表做同样的去重 —— 所以那张表别删。
fn model_count(models: &Value, id: &str, builtin: Option<&BuiltinProvider>) -> usize {
    let mut ids: BTreeSet<&str> = BTreeSet::new();
    if let Some(provider) = builtin {
        ids.extend(provider.models.iter().copied());
    }
    if let Some(list) = provider_config(models, id)
        .and_then(|config| config.get("models"))
        .and_then(Value::as_array)
    {
        ids.extend(
            list.iter()
                .filter_map(|model| model.get("id").and_then(Value::as_str)),
        );
    }
    ids.len()
}

/// `GET /api/auth/providers` —— 「订阅（OAuth）」列表（pi 的 `buildOAuthProviderList`）。
///
/// 形状：`{id, name, baseUrl?, usesCallbackServer, loggedIn, supportsApiKey}`。
/// toiler 没有 OAuth 登录流程，`loggedIn` 只可能来自别人写进 `auth.json` 的
/// `oauth` 凭据；前端点「Login」会拿到一条 SSE 错误事件（见 [`login_stream`]）。
pub async fn providers(State(state): State<AppState>, _trusted: Trusted) -> Response {
    let rows: Vec<Value> = listing_inputs(&state)
        .into_iter()
        .filter(|row| row.has_oauth)
        .map(|row| {
            let name = builtin_providers::find(&row.id)
                .map(builtin_providers::oauth_display_name)
                .or_else(|| row.oauth_name.clone())
                .unwrap_or_else(|| row.name.clone());
            let mut out = serde_json::Map::new();
            out.insert("id".into(), json!(row.id));
            out.insert("name".into(), json!(name));
            if let Some(base_url) = row.base_url {
                out.insert("baseUrl".into(), json!(base_url));
            }
            out.insert("usesCallbackServer".into(), json!(false));
            out.insert(
                "loggedIn".into(),
                json!(row.credential_type.as_deref() == Some("oauth")),
            );
            out.insert("supportsApiKey".into(), json!(row.has_api_key));
            Value::Object(out)
        })
        .collect();

    Json(json!({ "providers": rows })).into_response()
}

/// `GET /api/auth/all-providers` —— 「API Key」列表（pi 的 `buildApiKeyProviderList`）。
///
/// 形状：`{id, displayName, baseUrl?, configured, source?, modelCount, supportsOAuth}`。
/// 已配置的也返回（前端把它渲染到左侧）；用 `models.json` 里的 key 配置的跳过，
/// 因为面板已经从 `models.json` 渲染了那一条，重复会出现两个同名 provider。
pub async fn all_providers(State(state): State<AppState>, _trusted: Trusted) -> Response {
    let rows: Vec<Value> = listing_inputs(&state)
        .into_iter()
        .filter(|row| row.has_api_key)
        .filter(|row| !NON_LOGIN_SOURCES.contains(&row.source.unwrap_or("")))
        .map(|row| {
            let configured = row.configured && row.credential_type.as_deref() != Some("oauth");
            let mut out = serde_json::Map::new();
            out.insert("id".into(), json!(row.id));
            out.insert("displayName".into(), json!(row.name));
            if let Some(base_url) = row.base_url {
                out.insert("baseUrl".into(), json!(base_url));
            }
            out.insert("configured".into(), json!(configured));
            if configured {
                if let Some(source) = row.source {
                    out.insert("source".into(), json!(source));
                }
            }
            out.insert("modelCount".into(), json!(row.model_count));
            out.insert("supportsOAuth".into(), json!(row.has_oauth));
            Value::Object(out)
        })
        .collect();

    Json(json!({ "providers": rows })).into_response()
}

/// `GET /api/auth/api-key/{provider}`
pub async fn api_key_status(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(provider): Path<String>,
) -> Response {
    let models = state.models_file().read();
    let auth = state.auth();
    let config = provider_config(&models, &provider);
    let status = provider_status(auth, &provider, config);
    let masked = auth.api_key(&provider).and_then(|key| mask_key(&key));
    Json(json!({
        "provider": provider,
        "displayName": provider_display_name(&models, &provider),
        "configured": status.configured,
        "source": status.source,
        "models": provider_model_ids(&models, &provider).len(),
        "maskedKey": masked,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct ApiKeyBody {
    #[serde(rename = "apiKey")]
    api_key: Option<String>,
}

/// `POST /api/auth/api-key/{provider}` —— 存 key（不返回任何凭据）。
pub async fn save_api_key(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(provider): Path<String>,
    Json(body): Json<ApiKeyBody>,
) -> Response {
    let Some(key) = body
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
    else {
        return error(StatusCode::BAD_REQUEST, "apiKey is required");
    };
    let store = state.auth();
    if let Err(e) = store.set(&provider, json!({ "type": "api_key", "key": key })) {
        return error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
    }
    state.sessions().invalidate();
    Json(json!({ "success": true })).into_response()
}

/// `DELETE /api/auth/api-key/{provider}`
pub async fn delete_api_key(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(provider): Path<String>,
) -> Response {
    remove_credential(state.auth(), &provider, "api_key", "not an API key")
}

/// `POST /api/auth/logout/{provider}`
pub async fn logout(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(provider): Path<String>,
) -> Response {
    remove_credential(state.auth(), &provider, "oauth", "not OAuth")
}

/// 删凭据的公共部分。
///
/// 类型不匹配 → 409，措辞取**实际存着的类型**（`api_key` → "an API key"，
/// `oauth` → "OAuth"），与 pi 一致。
fn remove_credential(store: &AuthStore, provider: &str, expect: &str, expected: &str) -> Response {
    match store.remove(provider, expect) {
        Ok(RemoveResult::Removed) | Ok(RemoveResult::NotFound) => {
            Json(json!({ "success": true })).into_response()
        }
        Ok(RemoveResult::TypeMismatch(stored)) => {
            let human = match stored.as_str() {
                "api_key" => "an API key",
                "oauth" => "OAuth",
                _ => stored.as_str(),
            };
            error(
                StatusCode::CONFLICT,
                &format!("{provider} is authenticated with {human}, {expected}"),
            )
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

/// `POST /api/auth/login/{provider}` —— OAuth 登录（不支持）。
pub async fn login_unsupported(_trusted: Trusted, Path(provider): Path<String>) -> Response {
    error(
        StatusCode::NOT_IMPLEMENTED,
        &format!("OAuth 登录未实现（{provider}）：toiler 只支持 API Key"),
    )
}

/// `GET /api/auth/login/{provider}` —— 前端用 `EventSource` 订阅登录过程。
///
/// toiler 不做 OAuth，但**必须**回一条 `{type:"error"}` 的 SSE 事件：前端只认
/// 这个形状（`ModelsConfig.tsx` 的 `es.onmessage`），返回 501/405 会让面板显示
/// 英文的「Connection lost」，用户不知道是能力缺失。
pub async fn login_stream(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(provider): Path<String>,
) -> Response {
    if provider == crate::qoder_cn::PROVIDER_ID {
        return qoder_cn_login_stream(state).await;
    }
    let payload = json!({
        "type": "error",
        "message": format!("OAuth 登录未实现（{provider}）：toiler 只支持 API Key 与 Qoder CN 订阅"),
    })
    .to_string();
    // 一条事件后就结束流：前端收到 `error` 会自己 `close()`，不必再挂 keep-alive。
    sse_once(payload)
}

/// `qoder-cn` 的设备码登录（pi 的 `provider.oauth.login`）。
///
/// 事件形状照抄 pi 的前端约定：先 `{type:"auth",url,instructions}` 让面板显示
/// 授权链接，轮询成功发 `{type:"success"}`（前端据此刷新两个 provider 列表），
/// 失败发 `{type:"error",message}`。
async fn qoder_cn_login_stream(state: AppState) -> Response {
    let stream = async_stream::stream! {
        let login = crate::qoder_cn::auth::start_device_login();
        yield Ok::<Event, Infallible>(Event::default().data(
            json!({
                "type": "auth",
                "url": login.auth_url,
                "instructions": "在浏览器里确认登录后会自动继续（最长等待 5 分钟）",
            })
            .to_string(),
        ));

        match login.poll().await {
            Ok(tokens) => {
                let credential = json!({
                    "type": "oauth",
                    "access": tokens.access,
                    "refresh": tokens.refresh,
                    "expires": tokens.expires,
                    "uid": tokens.uid,
                    "nickname": tokens.nickname,
                    "userType": tokens.user_type,
                    "organizationId": tokens.organization_id,
                    "organizationName": tokens.organization_name,
                });
                match state.auth().set(crate::qoder_cn::PROVIDER_ID, credential) {
                    Ok(()) => {
                        // 模型目录要跟着刷新：端口在 bind 时才知道，登录可能发生在
                        // 任何时刻，这里补一次注入（幂等）。
                        state.attach_qoder_cn();
                        state.sessions().invalidate();
                        yield Ok(Event::default().data(json!({ "type": "success" }).to_string()));
                    }
                    Err(error) => {
                        yield Ok(Event::default().data(
                            json!({
                                "type": "error",
                                "message": format!("Qoder CN 凭据写入失败：{error}"),
                            })
                            .to_string(),
                        ));
                    }
                }
            }
            Err(message) => {
                yield Ok(Event::default().data(
                    json!({ "type": "error", "message": message }).to_string(),
                ));
            }
        }
    };

    sse_from(stream)
}

/// 单条 SSE 事件（事件后即结束流）。
fn sse_once(payload: String) -> Response {
    // 前端收到 `error` 会自己 `close()`，不必再挂 keep-alive。
    sse_from(futures::stream::iter(vec![Ok::<Event, Infallible>(
        Event::default().data(payload),
    )]))
}

fn sse_from<S>(stream: S) -> Response
where
    S: futures::Stream<Item = Result<Event, Infallible>> + Send + 'static,
{
    let mut response = Sse::new(stream).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache, no-transform"),
    );
    response
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn app(dir: &std::path::Path) -> (axum::Router, AppState) {
        let catalog =
            crate::catalog::ModelCatalog::from_config(json!({"providers": {}}), None).unwrap();
        let state = AppState::with_cwd(dir, catalog, dir).with_stores(
            AuthStore::new(dir.join("auth.json")),
            crate::models_json::ModelsFile::at(dir.join("models.json")),
            crate::settings::SettingsFile::at(dir.join("settings.json")),
            crate::projects::ProjectStore::at(dir.join("work.sqlite")),
            crate::trust::TrustStore::at(dir.join("trust.json")),
        );
        (crate::routes::router(state.clone()), state)
    }

    async fn call(
        router: axum::Router,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:8787");
        let request = match body {
            Some(value) => builder
                .header("content-type", "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn api_key_roundtrip_never_returns_plaintext() {
        let dir = std::env::temp_dir().join(format!("toiler-route-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("models.json"),
            serde_json::to_string(&json!({
                "providers": {
                    "qoder": {"api": "openai-completions", "baseUrl": "http://x/v1",
                              "models": [{"id": "m1"}, {"id": "m2"}]},
                    "openai": {"api": "openai-responses", "apiKey": "sk-literal",
                               "models": [{"id": "gpt"}]}
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let (router, _state) = app(&dir);

        // 「订阅（OAuth）」列表：内置目录里声明了 OAuth 的 9 家，字段形状是 OAuth
        // 那套（name/usesCallbackServer/loggedIn/supportsApiKey）
        let (_, body) = call(router.clone(), "GET", "/api/auth/providers", None).await;
        assert_eq!(
            provider_ids(&body),
            vec![
                "anthropic",
                "github-copilot",
                "kimi-coding",
                "meta",
                "openai",
                "openai-codex",
                "openrouter",
                "radius",
                "xai",
                "qoder-cn",
            ]
        );
        let codex = provider_row(&body, "openai-codex");
        assert_eq!(codex["name"], "ChatGPT Plus/Pro");
        assert_eq!(codex["loggedIn"], false);
        assert_eq!(codex["supportsApiKey"], false);
        assert_eq!(codex["usesCallbackServer"], false);

        // 「API Key」列表：内置 41 家 + 自建 provider；`models.json` 里写了 key 的
        // openai 不出现（面板左侧已经从 models.json 渲染它了）
        let (_, body) = call(router.clone(), "GET", "/api/auth/all-providers", None).await;
        let ids = provider_ids(&body);
        assert!(!ids.contains(&"openai"));
        assert!(!ids.contains(&"openai-codex"));
        assert!(ids.contains(&"anthropic"));
        assert!(ids.contains(&"deepseek"));
        assert!(ids.contains(&"apisets"));
        assert!(ids.contains(&"qoder"));
        // 纯 OAuth 的两家不在 API Key 列表里
        assert!(!ids.contains(&"qoder-cn"));
        let anthropic = provider_row(&body, "anthropic");
        assert_eq!(anthropic["displayName"], "Anthropic");
        assert_eq!(anthropic["configured"], false);
        assert_eq!(anthropic["modelCount"], 17);
        assert_eq!(anthropic["supportsOAuth"], true);
        let qoder = provider_row(&body, "qoder");
        assert_eq!(qoder["modelCount"], 2);
        assert_eq!(qoder["configured"], false);

        let (status, body) = call(router.clone(), "GET", "/api/auth/api-key/qoder", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["configured"], false);
        assert_eq!(body["models"], 2);
        assert_eq!(body["maskedKey"], Value::Null);

        let (status, _body) = call(
            router.clone(),
            "POST",
            "/api/auth/api-key/qoder",
            Some(json!({"apiKey": "sk-1234567890abcdef"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, body) = call(router.clone(), "GET", "/api/auth/api-key/qoder", None).await;
        assert_eq!(body["configured"], true);
        assert_eq!(body["maskedKey"], "•••••••••••••••cdef");
        assert!(!body.to_string().contains("sk-1234567890abcdef"));

        // 存了 API Key 之后：configured=true，source 落盘为 stored
        let (_, body) = call(router.clone(), "GET", "/api/auth/all-providers", None).await;
        assert_eq!(provider_row(&body, "qoder")["configured"], true);
        assert_eq!(provider_row(&body, "qoder")["source"], "stored");
        assert_eq!(provider_row(&body, "anthropic")["configured"], false);

        let (status, _) = call(router.clone(), "POST", "/api/auth/logout/qoder", None).await;
        assert_eq!(status, StatusCode::CONFLICT);

        let (status, _) = call(router.clone(), "DELETE", "/api/auth/api-key/qoder", None).await;
        assert_eq!(status, StatusCode::OK);
        let (_, body) = call(router, "GET", "/api/auth/api-key/qoder", None).await;
        assert_eq!(body["configured"], false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 别人的 `auth.json` 里存了 oauth 凭据时：`loggedIn=true`，并且该家从
    /// 「API Key」列表里退场（前端 `ApiKeyDetail` 只在非 oauth 时算已配置）。
    #[tokio::test]
    async fn oauth_credential_moves_a_provider_to_the_subscription_list() {
        let dir = std::env::temp_dir().join(format!("toiler-route-auth3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            serde_json::to_string(&json!({
                "anthropic": {"type": "oauth", "access": "a", "refresh": "r", "expires": 1}
            }))
            .unwrap(),
        )
        .unwrap();

        let (router, _) = app(&dir);
        let (_, body) = call(router.clone(), "GET", "/api/auth/providers", None).await;
        let anthropic = provider_row(&body, "anthropic");
        assert_eq!(anthropic["loggedIn"], true);
        assert_eq!(anthropic["name"], "Anthropic (Claude Pro/Max)");

        let (_, body) = call(router, "GET", "/api/auth/all-providers", None).await;
        let anthropic = provider_row(&body, "anthropic");
        assert_eq!(anthropic["configured"], false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn provider_ids(body: &Value) -> Vec<&str> {
        body["providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_str().unwrap())
            .collect()
    }

    fn provider_row<'a>(body: &'a Value, id: &str) -> &'a Value {
        body["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap_or_else(|| panic!("列表里没有 {id}：{body}"))
    }

    #[tokio::test]
    async fn login_is_not_implemented() {
        let dir = std::env::temp_dir().join(format!("toiler-route-auth2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (router, _) = app(&dir);
        let (status, body) = call(
            router.clone(),
            "POST",
            "/api/auth/login/github-copilot",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert!(body["error"].as_str().unwrap().contains("OAuth"));

        // 前端用 EventSource（GET）订阅登录：必须回一条 `{type:"error"}`，
        // 否则面板只会显示英文的「Connection lost」。
        let request = Request::builder()
            .method("GET")
            .uri("/api/auth/login/github-copilot")
            .header("host", "127.0.0.1:8787")
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap(),
            "text/event-stream"
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("\"type\":\"error\""), "{text}");
        assert!(text.contains("OAuth 登录未实现"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
