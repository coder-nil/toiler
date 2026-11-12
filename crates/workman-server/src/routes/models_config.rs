//! `/api/models-config*` —— 读写 `models.json` 与 `settings.json` 的 `enabledModels`。
//!
//! | route | 行为 |
//! |---|---|
//! | `GET    /api/models-config` | 原样返回 `models.json`（`{"providers":{}}` 兜底） |
//! | `PUT    /api/models-config` | 归一后写回（保留 `$schema`、清洗 id、补 cost） |
//! | `DELETE /api/models-config` | 删 provider 或删模型，并顺手清 `enabledModels` |
//! | `GET    /api/models-config/enabled?provider=` | 某 provider 的模型 + 是否可见 |
//! | `PUT    /api/models-config/enabled` | `{provider, modelIds}` → 写 `enabledModels` |
//! | `POST   /api/models-config/discover` | 打上游 `/models`，把模型列出来 |
//! | `POST   /api/models-config/test` | 拿新配的模型跑一句话，验证能不能通 |
//! | `GET    /api/models-config/catalog` | **已砍掉**（models.dev 目录），返回空表 |
//!
//! 注意 `PUT` 的归一规则见 [`crate::models_json`]，`enabledModels` 的匹配规则
//! 见 [`crate::settings`]。

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{provider_status, resolve_key_config, ResolvedKey};
use crate::models_json::{self, provider_config, provider_model_ids};
use crate::security::Trusted;
use crate::settings::{self, ModelPattern, ModelRef};
use crate::state::AppState;

/// `GET /api/models-config`
pub async fn get(State(state): State<AppState>, _trusted: Trusted) -> Response {
    Json(state.models_file().read()).into_response()
}

/// `PUT /api/models-config`
pub async fn put(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(body): Json<Value>,
) -> Response {
    match state.models_file().write(&body) {
        Ok(_) => {
            state.sessions().invalidate();
            Json(json!({ "success": true })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct DeleteBody {
    kind: Option<String>,
    provider: Option<String>,
    #[serde(rename = "modelId")]
    model_id: Option<String>,
}

/// `DELETE /api/models-config`
pub async fn delete(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(body): Json<DeleteBody>,
) -> Response {
    let kind = body.kind.as_deref().unwrap_or("");
    let provider = body.provider.as_deref().unwrap_or("").trim().to_string();
    let model_id = body.model_id.as_deref().unwrap_or("").trim().to_string();
    if (kind != "provider" && kind != "model") || provider.is_empty() {
        return bad_request("kind and provider are required");
    }
    if kind == "model" && model_id.is_empty() {
        return bad_request("modelId is required");
    }

    let result = if kind == "provider" {
        state.models_file().delete_provider(&provider)
    } else {
        state.models_file().delete_model(&provider, &model_id)
    };
    let root = match result {
        Ok(root) => root,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    };

    // 清掉指向已删项的 enabledModels 条目
    let provider_gone = kind == "provider" || provider_model_ids(&root, &provider).is_empty();
    prune_enabled_models(
        &state,
        &provider,
        if provider_gone { None } else { Some(&model_id) },
    );
    state.sessions().invalidate();
    Json(json!({ "success": true })).into_response()
}

/// 把某个 provider（或它的某个模型）从 `enabledModels` 里摘掉；摘空就删键。
fn prune_enabled_models(state: &AppState, provider: &str, model_id: Option<&str>) {
    let Some(current) = state.settings().enabled_models() else {
        return;
    };
    let filtered: Vec<String> = current
        .iter()
        .filter(|raw| {
            let Some(pattern) = ModelPattern::parse(raw) else {
                return true;
            };
            let Some(pattern_provider) = pattern.provider.as_deref() else {
                return true;
            };
            if !pattern_provider.eq_ignore_ascii_case(provider) {
                return true;
            }
            match model_id {
                // 删 provider：该 provider 的模式全删
                None => false,
                // 删模型：只删精确指向该模型的条目（`*` 保留）
                Some(id) => pattern.model != id,
            }
        })
        .cloned()
        .collect();
    if filtered.len() != current.len() {
        let _ = state.settings().set_enabled_models(if filtered.is_empty() {
            None
        } else {
            Some(filtered)
        });
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct EnabledQuery {
    provider: Option<String>,
}

/// `GET /api/models-config/enabled?provider=`
pub async fn enabled(
    State(state): State<AppState>,
    _trusted: Trusted,
    Query(query): Query<EnabledQuery>,
) -> Response {
    let provider = query.provider.as_deref().unwrap_or("").trim().to_string();
    if provider.is_empty() {
        return bad_request("provider is required");
    }

    let models_file = state.models_file().read();
    let patterns = state.settings().enabled_models();
    let all = collect_models(&state);
    let visible = settings::visible_models(patterns.as_deref(), &all);
    let warnings = pattern_warnings(patterns.as_deref(), &all);

    let available = available_models(&state, &provider);
    let visible_keys: std::collections::BTreeSet<String> = visible
        .iter()
        .map(|m| format!("{}/{}", m.provider, m.id))
        .collect();

    let mut rows: Vec<Value> = available
        .iter()
        .map(|model| {
            json!({
                "id": model.id,
                "name": model_name(&models_file, &model.provider, &model.id),
                "selected": visible_keys.contains(&format!("{}/{}", model.provider, model.id)),
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        let an = a["name"].as_str().unwrap_or("");
        let bn = b["name"].as_str().unwrap_or("");
        an.cmp(bn).then_with(|| {
            a["id"]
                .as_str()
                .unwrap_or("")
                .cmp(b["id"].as_str().unwrap_or(""))
        })
    });

    Json(json!({
        "provider": provider,
        "models": rows,
        "enabledModels": patterns,
        "unscoped": patterns.is_none(),
        "warnings": warnings,
        "refreshing": available.is_empty(),
    }))
    .into_response()
}

#[derive(Debug, Deserialize, Default)]
pub struct EnabledBody {
    provider: Option<String>,
    #[serde(rename = "modelIds")]
    model_ids: Option<Vec<String>>,
}

/// `PUT /api/models-config/enabled`
pub async fn put_enabled(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(body): Json<EnabledBody>,
) -> Response {
    let provider = body.provider.as_deref().unwrap_or("").trim().to_string();
    let Some(model_ids) = body.model_ids else {
        return bad_request("provider is required");
    };
    if provider.is_empty() {
        return bad_request("provider is required");
    }

    let available = available_models(&state, &provider);
    if available.is_empty() {
        return bad_request(&format!("No available models for \"{provider}\""));
    }
    let known: std::collections::BTreeSet<&str> = available.iter().map(|m| m.id.as_str()).collect();
    let unknown: Vec<&String> = model_ids
        .iter()
        .filter(|id| !known.contains(id.as_str()))
        .collect();
    if !unknown.is_empty() {
        return bad_request(&format!(
            "Unknown models for \"{provider}\": {}",
            unknown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let all = collect_models(&state);
    let patterns = state.settings().enabled_models();
    let visible = settings::visible_models(patterns.as_deref(), &all);
    let target = settings::patterns_with_provider_selection(&visible, &provider, &model_ids);
    let collapsed = settings::collapse_patterns(&all, &target);
    if let Some(list) = collapsed.as_ref() {
        if list.is_empty() {
            return bad_request("At least one model must stay visible in the picker");
        }
    }
    if let Err(e) = state.settings().set_enabled_models(collapsed.clone()) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response();
    }
    Json(json!({ "enabledModels": collapsed, "saved": true })).into_response()
}

/// `POST /api/models-config/discover`
pub async fn discover(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(body): Json<Value>,
) -> Response {
    let provider_name = body
        .get("providerName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if provider_name.is_empty() {
        return bad_request("providerName is required");
    }
    let Some(provider) = body.get("provider").filter(|v| v.is_object()) else {
        return bad_request("provider is required");
    };
    let base_url = provider
        .get("baseUrl")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if base_url.is_empty() {
        return bad_request("Base URL is required");
    }
    let api = provider
        .get("api")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("openai-completions");
    let Ok(endpoint) = discovery_endpoint(&base_url, api) else {
        return bad_request("Base URL is invalid");
    };

    let api_key = resolve_api_key(&state, &provider_name, provider);
    let mut request = reqwest::Client::new()
        .get(endpoint.clone())
        .timeout(std::time::Duration::from_secs(20));
    if let Some(key) = api_key.as_deref() {
        request = match api {
            "anthropic-messages" => request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01"),
            "google-generative-ai" => request.header("x-goog-api-key", key),
            _ => request.header("authorization", format!("Bearer {key}")),
        };
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(e) => {
            let status = if e.is_timeout() { 504 } else { 500 };
            return (
                StatusCode::from_u16(status).unwrap(),
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let message = if text.trim().is_empty() {
            format!("Upstream returned HTTP {}", status.as_u16())
        } else {
            text.chars().take(500).collect()
        };
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": message, "status": status.as_u16() })),
        )
            .into_response();
    }
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": "Upstream model list was not valid JSON" })),
        )
            .into_response();
    };
    let models = parse_upstream_models(&value);
    if models.is_empty() {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": "No models found in the upstream response" })),
        )
            .into_response();
    }
    Json(json!({ "models": models, "endpoint": endpoint })).into_response()
}

/// `POST /api/models-config/test` —— 用给定的 provider/model 跑一句 `Reply with OK only.`。
pub async fn test(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(body): Json<Value>,
) -> Response {
    let provider_name = body
        .get("providerName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if provider_name.is_empty() {
        return test_error("providerName is required");
    }
    let Some(provider) = body.get("provider").filter(|v| v.is_object()) else {
        return test_error("provider is required");
    };
    let Some(model) = body.get("model").filter(|v| v.is_object()) else {
        return test_error("model is required");
    };
    let model_id = model
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if model_id.is_empty() {
        return test_error("Model ID is required");
    }

    // 用一份临时 models.json 造一个只含这一个模型的 catalog（与 pi 的做法一致）
    let mut providers = serde_json::Map::new();
    let mut provider_config = provider.clone();
    if let Some(obj) = provider_config.as_object_mut() {
        let mut single = model.clone();
        if let Some(model_obj) = single.as_object_mut() {
            model_obj.insert("id".into(), json!(model_id));
        }
        obj.insert("models".into(), json!([single]));
    }
    providers.insert(provider_name.clone(), provider_config);
    let catalog =
        match crate::catalog::ModelCatalog::from_config(json!({ "providers": providers }), None) {
            Ok(catalog) => catalog,
            Err(e) => return test_error(&e.to_string()),
        };
    let Some(model_ref) = catalog.model_for(&provider_name, &model_id) else {
        return test_error(&format!("Model not found: {provider_name}/{model_id}"));
    };
    let Some(api_key) = resolve_api_key(&state, &provider_name, provider) else {
        return test_error(&format!("No API key found for \"{provider_name}\""));
    };
    let Some(runtime) = catalog
        .providers
        .iter()
        .find(|p| p.id() == provider_name.as_str())
        .cloned()
    else {
        return test_error(&format!("Provider not supported: {provider_name}"));
    };

    let started = std::time::Instant::now();
    let result = crate::oneshot::complete(
        runtime,
        model_ref,
        Some(api_key),
        "Reply with OK only.".to_string(),
        16,
        std::time::Duration::from_secs(20),
    )
    .await;
    let latency = started.elapsed().as_millis() as u64;
    match result {
        Ok(reply) => Json(json!({
            "ok": true,
            "latencyMs": latency,
            "status": Value::Null,
            "responseText": reply.text.chars().take(300).collect::<String>(),
        }))
        .into_response(),
        Err(e) => Json(json!({
            "ok": false,
            "error": e.to_string(),
            "latencyMs": latency,
            "status": Value::Null,
        }))
        .into_response(),
    }
}

/// `GET /api/models-config/catalog` —— models.dev 目录已砍，返回空表。
pub async fn catalog(_trusted: Trusted) -> Response {
    Json(json!({
        "models": [],
        "recommendation": Value::Null,
        "source": "models-dev",
        "note": "workman 不内置 models.dev 目录（见 PLAN.md「明确不做的」）",
    }))
    .into_response()
}

fn test_error(message: &str) -> Response {
    Json(json!({ "ok": false, "error": message })).into_response()
}

fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}

/// models.json 里所有模型（顺序按配置）。
pub fn collect_models(state: &AppState) -> Vec<ModelRef> {
    // `models.json` 的模型 + catalog 里多出来的（内置目录 provider，如 `qoder-cn`）。
    //
    // 只读 `models.json` 会让内置 provider 的模型「不可见也不可勾选」，还会报
    // `No models match pattern "qoder-cn/gfmodel"` 这种假警告；只读 catalog 又会在
    // catalog 尚未跟上 `models.json` 改动时丢模型（`/api/models` 用 catalog，
    // 编辑中的 `models.json` 未必已经重建）。两份取并集最稳。
    let root = state.models_file().read();
    let mut seen: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    let mut out: Vec<ModelRef> = Vec::new();
    for (provider, _) in models_json::providers(&root) {
        for id in provider_model_ids(&root, &provider) {
            if seen.insert((provider.clone(), id.clone())) {
                out.push(ModelRef {
                    provider: provider.clone(),
                    id,
                });
            }
        }
    }
    for entry in &state.catalog().entries {
        if seen.insert((entry.provider.clone(), entry.id.clone())) {
            out.push(ModelRef {
                provider: entry.provider.clone(),
                id: entry.id.clone(),
            });
        }
    }
    out
}

/// 「可用」模型 = 该 provider 的凭据能解析出来的模型（pi 的 `getAvailable()`）。
fn available_models(state: &AppState, provider: &str) -> Vec<ModelRef> {
    let root = state.models_file().read();
    let config = provider_config(&root, provider);
    let status = provider_status(state.auth(), provider, config);
    if !status.configured {
        return Vec::new();
    }
    if status.source == Some("models_json_command") {
        // 命令型凭据要跑子进程才知道有没有，先当作可用
    }
    let from_config = provider_model_ids(&root, provider);
    if !from_config.is_empty() {
        return from_config
            .into_iter()
            .map(|id| ModelRef {
                provider: provider.to_string(),
                id,
            })
            .collect();
    }
    // `qoder-cn` 这类**内置目录** provider 不在 `models.json` 里，模型清单来自
    // catalog（pi 的 SDK 目录同源）。订阅详情的「模型勾选」区靠这条分支才有内容。
    state
        .catalog()
        .entries
        .iter()
        .filter(|entry| entry.provider == provider)
        .map(|entry| ModelRef {
            provider: provider.to_string(),
            id: entry.id.clone(),
        })
        .collect()
}

/// 模型的显示名：`models.json` 里的 `name` 优先，其次目录里的名字，最后用 id。
fn model_name(root: &Value, provider: &str, id: &str) -> String {
    if let Some(name) = models_json_name(root, provider, id) {
        return name;
    }
    crate::qoder_cn::constants::catalog_entry(id)
        .filter(|_| provider == crate::qoder_cn::PROVIDER_ID)
        .map(|entry| entry.display_name.to_string())
        .unwrap_or_else(|| id.to_string())
}

fn models_json_name(root: &Value, provider: &str, id: &str) -> Option<String> {
    provider_config(root, provider)
        .and_then(|p| p.get("models"))
        .and_then(Value::as_array)
        .and_then(|models| {
            models
                .iter()
                .find(|m| m.get("id").and_then(Value::as_str) == Some(id))
        })
        .and_then(|m| m.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// 模式一条都匹配不上时给警告（pi 的 `no-match` / `ambiguous`）。
///
/// 元素必须是**纯字符串**：pi 在 `/api/models-config/enabled` 里取的是
/// `resolveModelScopeWithDiagnostics` 的 `diagnostics.map(d => d.message)`，
/// 前端再把 `warnings` 逐条渲染成文本。返回对象会让 React 抛
/// “Objects are not valid as a React child (found: object with keys
/// {type, code, message, pattern})”，整个页面直接落到 `global-error`
/// 的 “This page couldn't load” 兜底页（workman 就曾因此整页崩掉）。
fn pattern_warnings(patterns: Option<&[String]>, models: &[ModelRef]) -> Vec<String> {
    let Some(patterns) = patterns else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for raw in patterns {
        let Some(pattern) = ModelPattern::parse(raw) else {
            continue;
        };
        let hits: Vec<&ModelRef> = models
            .iter()
            .filter(|m| settings::pattern_matches(&pattern, m))
            .collect();
        if hits.is_empty() {
            out.push(format!("No models match pattern \"{raw}\""));
            continue;
        }
        if pattern.provider.is_none() {
            let providers: std::collections::BTreeSet<&str> =
                hits.iter().map(|m| m.provider.as_str()).collect();
            if providers.len() > 1 {
                out.push(format!(
                    "Pattern \"{raw}\" matches several providers: {}. Use provider/id to choose one.",
                    providers.into_iter().collect::<Vec<_>>().join(", ")
                ));
            }
        }
    }
    out
}

/// 上游模型列表的 URL（模块 `93598.h`）。
fn discovery_endpoint(base_url: &str, api: &str) -> Result<String, ()> {
    let mut url = url::Url::parse(base_url.trim()).map_err(|_| ())?;
    let path = url.path().trim_end_matches('/').to_string();
    if !path.to_ascii_lowercase().ends_with("/models") {
        let mut next = path.clone();
        if api == "anthropic-messages" && !looks_like_api_version(&next) {
            next.push_str("/v1");
        }
        if api == "google-generative-ai" && !looks_like_api_version(&next) {
            next.push_str("/v1beta");
        }
        url.set_path(&format!("{next}/models").replace("//", "/"));
    }
    if api == "anthropic-messages" && !url.query_pairs().any(|(k, _)| k == "limit") {
        url.query_pairs_mut().append_pair("limit", "1000");
    }
    if api == "google-generative-ai" && !url.query_pairs().any(|(k, _)| k == "pageSize") {
        url.query_pairs_mut().append_pair("pageSize", "1000");
    }
    Ok(url.to_string())
}

fn looks_like_api_version(path: &str) -> bool {
    let Some(last) = path.trim_end_matches('/').rsplit('/').next() else {
        return false;
    };
    let Some(rest) = last.strip_prefix('v') else {
        return false;
    };
    let rest = rest.strip_suffix("beta").unwrap_or(rest);
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

/// 解析上游 `/models` 的响应（模块 `93598.f`）。
pub fn parse_upstream_models(value: &Value) -> Vec<Value> {
    let list = model_list_of(value);
    let mut seen = std::collections::BTreeSet::new();
    let mut out: Vec<Value> = Vec::new();
    for item in list {
        let Some(parsed) = parse_upstream_model(&item) else {
            continue;
        };
        let id = parsed["id"].as_str().unwrap_or_default().to_string();
        if !seen.insert(id) {
            continue;
        }
        out.push(parsed);
    }
    out.sort_by(|a, b| {
        let an = a["name"]
            .as_str()
            .unwrap_or_else(|| a["id"].as_str().unwrap_or(""));
        let bn = b["name"]
            .as_str()
            .unwrap_or_else(|| b["id"].as_str().unwrap_or(""));
        an.to_lowercase().cmp(&bn.to_lowercase())
    });
    out
}

fn model_list_of(value: &Value) -> Vec<Value> {
    if let Some(array) = value.as_array() {
        return array.clone();
    }
    let Some(obj) = value.as_object() else {
        return Vec::new();
    };
    for key in ["data", "models", "results", "items"] {
        match obj.get(key) {
            Some(Value::Array(array)) => return array.clone(),
            Some(Value::Object(map)) => return map.values().cloned().collect(),
            _ => {}
        }
    }
    Vec::new()
}

fn non_empty(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn positive_int(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64).filter(|v| *v > 0)
}

fn parse_upstream_model(item: &Value) -> Option<Value> {
    if let Some(raw) = item.as_str() {
        let id = raw.trim();
        if id.is_empty() {
            return None;
        }
        return Some(json!({ "id": id }));
    }
    let obj = item.as_object()?;
    let id_raw = non_empty(obj.get("id"))
        .or_else(|| non_empty(obj.get("model")))
        .or_else(|| non_empty(obj.get("name")))?;
    let id = id_raw
        .strip_prefix("models/")
        .unwrap_or(&id_raw)
        .to_string();
    if id.is_empty() {
        return None;
    }
    let name = non_empty(obj.get("display_name"))
        .or_else(|| non_empty(obj.get("displayName")))
        .or_else(|| {
            if obj.contains_key("id") || obj.contains_key("model") {
                non_empty(obj.get("name"))
            } else {
                None
            }
        });
    let metadata = obj.get("metadata").filter(|v| v.is_object());
    let context_window = positive_int(metadata.and_then(|m| m.get("context_window")))
        .or_else(|| positive_int(obj.get("context_window")))
        .or_else(|| positive_int(obj.get("contextWindow")));
    let max_tokens = positive_int(metadata.and_then(|m| m.get("max_output_tokens")))
        .or_else(|| positive_int(obj.get("max_output_tokens")))
        .or_else(|| positive_int(obj.get("maxTokens")));
    let input = obj
        .get("input")
        .and_then(Value::as_array)
        .or_else(|| {
            obj.get("capabilities")
                .and_then(|c| c.get("input"))
                .and_then(Value::as_array)
        })
        .or_else(|| obj.get("supported_modalities").and_then(Value::as_array))
        .or_else(|| {
            metadata
                .and_then(|m| m.get("input"))
                .and_then(Value::as_array)
        })
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| s == "text" || s == "image")
                .collect::<Vec<_>>()
        })
        .filter(|list| !list.is_empty());

    let mut out = serde_json::Map::new();
    out.insert("id".into(), json!(id));
    if let Some(name) = name.filter(|n| n != &id) {
        out.insert("name".into(), json!(name));
    }
    if let Some(context_window) = context_window {
        out.insert("contextWindow".into(), json!(context_window));
    }
    if let Some(max_tokens) = max_tokens {
        out.insert("maxTokens".into(), json!(max_tokens));
    }
    if let Some(input) = input {
        out.insert("input".into(), json!(input));
    }
    Some(Value::Object(out))
}

/// 解析 provider 的明文 API Key（`auth.json` 优先，其次 models.json 的字面量）。
pub fn resolve_api_key(state: &AppState, provider_name: &str, provider: &Value) -> Option<String> {
    if let Some(key) = state.auth().api_key(provider_name) {
        return Some(key);
    }
    let config = provider.get("apiKey")?;
    match resolve_key_config(config) {
        ResolvedKey::Literal(key) => Some(key),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_style_list() {
        let value = json!({
            "data": [
                {"id": "gpt-6", "name": "GPT-6", "context_window": 1000},
                {"id": "models/gemini", "display_name": "Gemini"},
                {"id": "gpt-6"}
            ]
        });
        let models = parse_upstream_models(&value);
        assert_eq!(models.len(), 2);
        assert!(models.iter().any(|m| m["id"] == "gemini"));
    }

    #[test]
    fn parses_string_items_and_sorts() {
        let value = json!(["b-model", "a-model"]);
        let models = parse_upstream_models(&value);
        assert_eq!(models[0]["id"], "a-model");
        assert_eq!(models[1]["id"], "b-model");
    }

    #[test]
    fn builds_discovery_endpoints() {
        assert_eq!(
            discovery_endpoint("https://api.example.com/v1", "openai-completions").unwrap(),
            "https://api.example.com/v1/models"
        );
        let anthropic =
            discovery_endpoint("https://api.anthropic.com", "anthropic-messages").unwrap();
        assert!(anthropic.contains("/v1/models"));
        assert!(anthropic.contains("limit=1000"));
        let gemini = discovery_endpoint("https://x/y", "google-generative-ai").unwrap();
        assert!(gemini.contains("/v1beta/models"));
    }

    #[test]
    fn api_version_detection() {
        assert!(looks_like_api_version("/v1"));
        assert!(looks_like_api_version("/v1beta"));
        assert!(!looks_like_api_version("/openai"));
        assert!(!looks_like_api_version("/v1/extra"));
    }
}
