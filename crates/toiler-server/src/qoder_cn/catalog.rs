//! Qoder CN 订阅的远端模型目录：COSY 签名 GET，而不是 API Key 的 `/models`。
//!
//! 模型路由键必须同时进入运行时目录和对话请求体；只更新设置页会导致新模型在
//! 转发层被当成未知模型。缓存绑定账号 uid，切换账号不能复用前一个订阅的授权目录。

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::constants::{self, CatalogEntry, GATEWAY, PROVIDER_ID};
use super::cosy::{HeaderOptions, Identity, Session};
use super::transport::CredentialSource;
use crate::auth::AuthStore;

pub const MODEL_LIST_PATH: &str = "/algo/api/v2/model/list?Encode=1";
const STORE_ETAG: &str = "qoder-cn-models-api-v1";

/// 静态快照与远端条目共用的可拥有元数据，避免为新模型泄漏静态字符串。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub key: String,
    pub display_name: String,
    pub vision: bool,
    pub reasoning: bool,
    pub max_input_tokens: u64,
    pub efforts: Vec<String>,
}

impl From<&CatalogEntry> for CatalogModel {
    fn from(entry: &CatalogEntry) -> Self {
        Self {
            key: entry.key.to_string(),
            display_name: entry.display_name.to_string(),
            vision: entry.vision,
            reasoning: entry.reasoning,
            max_input_tokens: entry.max_input_tokens,
            efforts: entry
                .efforts
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
        }
    }
}

impl CatalogModel {
    pub fn definition(&self) -> Value {
        json!({
            "id": self.key,
            "name": self.display_name,
            "contextWindow": self.max_input_tokens,
            "maxTokens": constants::DEFAULT_MAX_OUTPUT_TOKENS,
            "reasoning": self.reasoning,
            "input": if self.vision { vec!["text", "image"] } else { vec!["text"] },
        })
    }
}

pub fn entries(models_json: &Path, auth: &AuthStore) -> Vec<CatalogModel> {
    cached(models_json, auth).unwrap_or_else(|| {
        constants::STATIC_CATALOG
            .iter()
            .map(CatalogModel::from)
            .collect()
    })
}

fn account_uid(auth: &AuthStore) -> Option<String> {
    let credentials = auth.read();
    let credential = credentials.get(PROVIDER_ID)?;
    if credential["type"] != "oauth" {
        return None;
    }
    credential["uid"]
        .as_str()
        .filter(|uid| !uid.is_empty())
        .map(str::to_string)
}

fn cached(models_json: &Path, auth: &AuthStore) -> Option<Vec<CatalogModel>> {
    let uid = account_uid(auth)?;
    let text = std::fs::read_to_string(crate::remote_models::store_path(models_json)).ok()?;
    let root: Value = serde_json::from_str(&text).ok()?;
    let entry = root.get(PROVIDER_ID)?;
    if entry["etag"] != STORE_ETAG || entry["accountUid"] != uid {
        return None;
    }
    let models: Vec<CatalogModel> = serde_json::from_value(entry["models"].clone()).ok()?;
    if models.is_empty()
        || models
            .iter()
            .any(|model| model.key.trim().is_empty() || model.max_input_tokens == 0)
    {
        return None;
    }
    Some(models)
}

/// 转发层与 rpi 目录共用这份查询，新键不能只出现在设置页。
pub fn find(models_json: &Path, auth: &AuthStore, key: &str) -> Option<CatalogModel> {
    entries(models_json, auth)
        .into_iter()
        .find(|model| model.key == key)
}

/// 当前账号是否已经拉过远端目录（卡片计数不该再把旧静态快照算进去）。
pub fn has_remote_cache(models_json: &Path, auth: &AuthStore) -> bool {
    cached(models_json, auth).is_some()
}

fn field<'a>(value: &'a Value, names: &[&str]) -> Option<&'a Value> {
    names
        .iter()
        .find_map(|name| value.get(*name).filter(|value| !value.is_null()))
}

fn text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn parse_model(value: &Value) -> Option<CatalogModel> {
    let key = text(field(value, &["key", "model_key", "modelKey", "id"]))?;
    let fallback = constants::catalog_entry(key);
    let mut model = fallback
        .map(CatalogModel::from)
        .unwrap_or_else(|| CatalogModel {
            key: key.to_string(),
            display_name: key.to_string(),
            vision: false,
            reasoning: false,
            max_input_tokens: 128_000,
            efforts: Vec::new(),
        });
    if let Some(name) = text(field(value, &["display_name", "displayName", "name"])) {
        model.display_name = name.to_string();
    }
    if let Some(vision) = field(value, &["is_vl", "isVl", "vision"]).and_then(Value::as_bool) {
        model.vision = vision;
    }
    if let Some(reasoning) =
        field(value, &["is_reasoning", "isReasoning", "reasoning"]).and_then(Value::as_bool)
    {
        model.reasoning = reasoning;
    }
    if let Some(limit) = field(
        value,
        &["max_input_tokens", "maxInputTokens", "contextWindow"],
    )
    .and_then(Value::as_u64)
    .filter(|limit| *limit > 0)
    {
        model.max_input_tokens = limit;
    }
    if let Some(efforts) = field(value, &["efforts", "reasoning_efforts", "reasoningEfforts"])
        .and_then(Value::as_array)
    {
        model.efforts = efforts
            .iter()
            .filter_map(Value::as_str)
            .filter(|value| {
                matches!(
                    *value,
                    "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                )
            })
            .map(str::to_string)
            .collect();
    }
    Some(model)
}

/// 兼容模型列表及网关的 JSON/body/Encode 信封；有界递归，不将错误信封当模型。
pub fn parse(payload: &Value) -> Result<Vec<CatalogModel>, String> {
    fn visit(value: &Value, depth: usize, models: &mut Vec<CatalogModel>) {
        if depth > 6
            || value["success"] == false
            || value
                .get("statusCodeValue")
                .and_then(Value::as_u64)
                .is_some_and(|status| status >= 400)
        {
            return;
        }
        if let Some(encoded) = value.as_str() {
            let decoded = serde_json::from_str::<Value>(encoded).ok().or_else(|| {
                super::base64::decode(encoded).and_then(|bytes| serde_json::from_slice(&bytes).ok())
            });
            if let Some(decoded) = decoded {
                visit(&decoded, depth + 1, models);
            }
        } else if let Some(list) = value.as_array() {
            for item in list {
                visit(item, depth + 1, models);
            }
        } else {
            // 先看包裹层：信封自己也可能带 `id`（请求 id），不能当成模型条目。
            let children: Vec<&Value> =
                ["data", "models", "model_list", "modelList", "list", "body"]
                    .iter()
                    .filter_map(|key| value.get(key))
                    .collect();
            if children.is_empty() {
                if let Some(model) = parse_model(value) {
                    models.push(model);
                }
            } else {
                for child in children {
                    visit(child, depth + 1, models);
                }
            }
        }
    }
    let mut models = Vec::new();
    visit(payload, 0, &mut models);
    let mut seen = std::collections::BTreeSet::new();
    models.retain(|model| seen.insert(model.key.clone()));
    if models.is_empty() {
        return Err("Qoder CN 模型目录没有返回可用模型，保留原有目录".into());
    }
    Ok(models)
}

async fn send(
    client: &reqwest::Client,
    endpoint: &str,
    identity: &Identity,
) -> Result<reqwest::Response, String> {
    let headers = Session::new(identity)?.headers(
        "",
        endpoint,
        &HeaderOptions {
            sse: false,
            ..Default::default()
        },
    );
    let mut request = client.get(endpoint);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    request
        .send()
        .await
        .map_err(|error| format!("请求 Qoder CN 模型目录失败：{error}"))
}

pub async fn refresh(
    credentials: &CredentialSource,
    models_json: &Path,
) -> Result<Vec<CatalogModel>, String> {
    let endpoint = format!("{GATEWAY}{MODEL_LIST_PATH}");
    refresh_at(credentials, models_json, &endpoint).await
}

async fn refresh_at(
    credentials: &CredentialSource,
    models_json: &Path,
    endpoint: &str,
) -> Result<Vec<CatalogModel>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())?;
    let identity = credentials.identity().await?;
    if identity.uid.is_empty() {
        return Err("Qoder CN 凭据缺少账号信息，请重新登录".into());
    }
    let mut response = send(&client, endpoint, &identity).await?;
    if matches!(response.status().as_u16(), 401 | 403) {
        let renewed = credentials.force_renew().await?;
        response = send(&client, endpoint, &renewed).await?;
    }
    if !response.status().is_success() {
        return Err(format!(
            "Qoder CN 模型目录返回 HTTP {}，保留原有目录",
            response.status().as_u16()
        ));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|_| "Qoder CN 模型目录不是合法 JSON".to_string())?;
    let models = parse(&payload)?;
    if account_uid(&credentials.store).as_deref() != Some(identity.uid.as_str()) {
        return Err("Qoder CN 登录状态已变化，请重新刷新模型列表".into());
    }
    crate::remote_models::write_entry(
        models_json,
        PROVIDER_ID,
        json!({
            "etag": STORE_ETAG,
            "accountUid": identity.uid,
            "checkedAt": super::now_ms(),
            "models": models,
        }),
    )
    .map_err(|error| format!("写 Qoder CN 模型缓存失败：{error}"))?;
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn fixture() -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("toiler-qoder-catalog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn login(store: &AuthStore, uid: &str) {
        store
            .set(
                PROVIDER_ID,
                json!({
                    "type": "oauth", "uid": uid, "access": "fake-access", "refresh": "fake-refresh",
                    "expires": super::super::now_ms() + 3600000,
                }),
            )
            .unwrap();
    }

    #[test]
    fn parses_signed_gateway_catalog_and_preserves_known_capabilities() {
        let payload = json!({ "data": { "models": [
            { "key": "gfmodel", "display_name": "Updated GLM" },
            { "key": "qmodel_future", "display_name": "Future", "is_vl": true,
              "is_reasoning": true, "max_input_tokens": 300000,
              "reasoning_efforts": ["low", "high", "invalid"] },
            { "key": "qmodel_future" }, { "display_name": "missing key" }
        ] } });
        let models = parse(&payload).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].display_name, "Updated GLM");
        assert_eq!(models[0].max_input_tokens, 1000000);
        assert_eq!(models[1].efforts, ["low", "high"]);
        let encoded = super::super::base64::encode_str(&payload.to_string());
        assert_eq!(
            parse(&json!({"statusCodeValue":200,"body":encoded})).unwrap(),
            models
        );
        // 信封自己带 `id`（请求 id）时不能当成模型，否则真实模型会被整批丢掉。
        let envelope =
            parse(&json!({"id":"req-1","data":{"models":[{"key":"qmodel_future"}]}})).unwrap();
        assert_eq!(envelope.len(), 1);
        assert_eq!(envelope[0].key, "qmodel_future");
        for empty in [
            json!({}),
            json!({"data":[]}),
            json!({"success":false,"data":[{"key":"wrong"}]}),
            json!({"statusCodeValue":401,"body":payload}),
        ] {
            assert!(parse(&empty).is_err());
        }
    }

    #[test]
    fn new_remote_model_keeps_its_key_and_effort_in_chat_requests() {
        // 对应订阅刷新后选择新模型：不能只更新 UI，代理和请求体也必须认新路由键。
        let entry = parse(&json!([{"key":"qmodel_future","display_name":"Future",
            "is_vl":true,"is_reasoning":true,"reasoning_efforts":["high"]}]))
        .unwrap()
        .remove(0);
        let models = super::super::models_from_catalog(
            "http://127.0.0.1:3000/qoder-cn/token",
            std::slice::from_ref(&entry),
        );
        assert_eq!(models[0].id, "qmodel_future");
        assert_eq!(models[0].input.len(), 2);
        let levels = crate::catalog::supported_thinking_levels(&models[0]);
        assert!(levels.contains(&"high"));
        assert!(!levels.contains(&"low"));
        let encoded = super::super::request_body::build_request_body(
            &json!({"model":"qmodel_future","messages":[{"role":"user","content":"hi"}],"reasoning_effort":"high"}),
            &entry.key,
            &entry,
            None,
            None,
        );
        let body: Value =
            serde_json::from_slice(&super::super::base64::decode(&encoded).unwrap()).unwrap();
        assert_eq!(body["model_config"]["key"], "qmodel_future");
        assert_eq!(body["business"]["model_key"], "qmodel_future");
        assert_eq!(body["parameters"]["reasoning_effort"], "high");
    }

    #[tokio::test]
    async fn refresh_uses_cosy_get_and_preserves_cache_on_failure() {
        let root = fixture();
        let models_json = root.join("models.json");
        let store = AuthStore::new(root.join("auth.json"));
        login(&store, "account-a");
        let credentials = CredentialSource {
            store: store.clone(),
        };
        crate::remote_models::write(
            &models_json,
            "openai",
            "openai-completions",
            "https://example.invalid",
            &[json!({"id":"kept"})],
        )
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!(
            "http://{}{}",
            listener.local_addr().unwrap(),
            MODEL_LIST_PATH
        );
        let upstream = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, payload) in [
                (
                    "200 OK",
                    json!({"data":[{"key":"qmodel_future","display_name":"Future"}]}),
                ),
                ("500 Internal Server Error", json!({"error":"failed"})),
                ("200 OK", json!({"data":[]})),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0; 1024];
                while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    let size = stream.read(&mut buf).unwrap();
                    assert!(size > 0);
                    bytes.extend_from_slice(&buf[..size]);
                }
                requests.push(String::from_utf8(bytes).unwrap());
                let body = payload.to_string();
                write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
            requests
        });
        let models = refresh_at(&credentials, &models_json, &endpoint)
            .await
            .unwrap();
        assert_eq!(models[0].key, "qmodel_future");
        let before = std::fs::read(crate::remote_models::store_path(&models_json)).unwrap();
        for _ in 0..2 {
            assert!(refresh_at(&credentials, &models_json, &endpoint)
                .await
                .is_err());
        }
        assert_eq!(
            before,
            std::fs::read(crate::remote_models::store_path(&models_json)).unwrap()
        );
        assert!(crate::remote_models::read(&models_json, "openai").is_some());
        let cached_text = String::from_utf8(before).unwrap();
        assert!(!cached_text.contains("fake-access") && !cached_text.contains("fake-refresh"));
        assert!(find(&models_json, &store, "qmodel_future").is_some());
        for request in upstream.join().unwrap() {
            assert!(request.starts_with("GET /algo/api/v2/model/list?Encode=1 HTTP/1.1"));
            let lower = request.to_lowercase();
            assert!(lower.contains("authorization: bearer cosy."));
            assert!(lower.contains("cosy-user: account-a"));
            assert!(lower.contains("accept: application/json"));
            assert!(!lower.contains("bearer fake-access"));
        }
        login(&store, "account-b");
        assert!(find(&models_json, &store, "qmodel_future").is_none());
        assert_eq!(
            entries(&models_json, &store).len(),
            constants::STATIC_CATALOG.len()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn missing_subscription_credentials_never_connects_upstream() {
        let root = fixture();
        let credentials = CredentialSource {
            store: AuthStore::new(root.join("auth.json")),
        };
        let error = refresh_at(
            &credentials,
            &root.join("models.json"),
            "http://127.0.0.1:1",
        )
        .await
        .unwrap_err();
        assert!(error.contains("未登录"));
        assert!(!crate::remote_models::store_path(&root.join("models.json")).exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
