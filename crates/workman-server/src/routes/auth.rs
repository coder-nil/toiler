//! `/api/auth/*` —— API Key 的存取与 provider 列表。
//!
//! | route | 行为 |
//! |---|---|
//! | `GET /api/auth/providers` | 可登录的 provider（过滤掉「凭据写在 models.json 里」的） |
//! | `GET /api/auth/all-providers` | 所有支持 OAuth 的 provider |
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

use crate::auth::{mask_key, provider_status, AuthStore, RemoveResult};
use crate::models_json::{self, provider_config, provider_display_name, provider_model_ids};
use crate::security::Trusted;
use crate::state::AppState;

/// models.json 里 `apiKey` 的来源；这些 provider 不该出现在「用 API Key 登录」列表里
/// （pi 的 `89950.d`）。
const NON_LOGIN_SOURCES: [&str; 2] = ["models_json_key", "models_json_command"];

fn provider_rows(state: &AppState) -> Vec<Value> {
    let models = state.models_file().read();
    let auth = state.auth();
    models_json::providers(&models)
        .into_iter()
        .map(|(id, config)| {
            let status = provider_status(auth, &id, Some(&config));
            json!({
                "id": id,
                "name": provider_display_name(&models, &id),
                "baseUrl": config.get("baseUrl").cloned().unwrap_or(Value::Null),
                "status": {
                    "configured": status.configured,
                    "source": status.source,
                    "label": status.label,
                },
                "credentialType": auth.credential_type(&id),
                "modelCount": provider_model_ids(&models, &id).len(),
                "hasApiKeyLogin": config.get("apiKey").is_none(),
                "hasOAuth": false,
            })
        })
        .collect()
}

/// `GET /api/auth/providers` —— 「用 API Key 登录」列表（模块 `89950.z`）。
pub async fn providers(State(state): State<AppState>, _trusted: Trusted) -> Response {
    let rows: Vec<Value> = provider_rows(&state)
        .into_iter()
        .filter(|row| {
            let source = row["status"]["source"].as_str().unwrap_or("");
            if NON_LOGIN_SOURCES.contains(&source) {
                return false;
            }
            let configured = row["status"]["configured"].as_bool().unwrap_or(false);
            // 已用 API Key 配置的（非 oauth）也算「已配置」
            row["hasApiKeyLogin"].as_bool().unwrap_or(false)
                || !configured
                || row["credentialType"].as_str() == Some("oauth")
        })
        .map(|row| {
            let configured = row["status"]["configured"].as_bool().unwrap_or(false)
                && row["credentialType"].as_str() != Some("oauth");
            let mut out = serde_json::Map::new();
            out.insert("id".into(), row["id"].clone());
            out.insert("displayName".into(), row["name"].clone());
            if !row["baseUrl"].is_null() {
                out.insert("baseUrl".into(), row["baseUrl"].clone());
            }
            out.insert("configured".into(), json!(configured));
            if configured {
                if let Some(source) = row["status"]["source"].as_str() {
                    out.insert("source".into(), json!(source));
                }
            }
            out.insert("modelCount".into(), row["modelCount"].clone());
            out.insert("supportsOAuth".into(), json!(false));
            Value::Object(out)
        })
        .collect();

    Json(json!({ "providers": rows })).into_response()
}

/// `GET /api/auth/all-providers` —— OAuth provider 列表（模块 `89950.l`）。
///
/// workman 不支持 OAuth 登录，所以永远是空数组；保留 route 是为了前端的
/// 「登录」面板不 404。
pub async fn all_providers(_trusted: Trusted) -> Response {
    Json(json!({ "providers": [] })).into_response()
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
        &format!("OAuth 登录未实现（{provider}）：workman 只支持 API Key"),
    )
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
        let dir = std::env::temp_dir().join(format!("workman-route-auth-{}", std::process::id()));
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

        // providers 列表：models.json 里写了 key 的不出现
        let (_, body) = call(router.clone(), "GET", "/api/auth/providers", None).await;
        let ids: Vec<&str> = body["providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["qoder"]);

        let (status, _) = call(router.clone(), "POST", "/api/auth/logout/qoder", None).await;
        assert_eq!(status, StatusCode::CONFLICT);

        let (status, _) = call(router.clone(), "DELETE", "/api/auth/api-key/qoder", None).await;
        assert_eq!(status, StatusCode::OK);
        let (_, body) = call(router, "GET", "/api/auth/api-key/qoder", None).await;
        assert_eq!(body["configured"], false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn login_is_not_implemented() {
        let dir = std::env::temp_dir().join(format!("workman-route-auth2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (router, _) = app(&dir);
        let (status, body) = call(router, "POST", "/api/auth/login/github-copilot", None).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert!(body["error"].as_str().unwrap().contains("OAuth"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
