//! `apisets` —— 设置页里显示为 **Coding** 的内置服务商。
//!
//! 它和 `qoder-cn` 是 pi-desktop 自己注册的两家（见 [`crate::builtin_providers`]
//! 的 `DESKTOP_PROVIDERS`），但目录来源完全不同：
//!
//! | provider | 目录来源 |
//! |---|---|
//! | `qoder-cn` | 静态快照（[`crate::qoder_cn::constants::STATIC_CATALOG`]） |
//! | `apisets` | **上游 `GET {baseUrl}/v1/models`**（无静态表） |
//!
//! 所以这里要复刻 pi 的「远端目录 + 本地缓存」机制，pi 侧对应
//! `pi-desktop/lib/desktop-providers.ts` 的 `registerProvider("apisets", …)`：
//!
//! 1. 缓存文件 `models-store.json`（与 `models.json` 同目录），形状
//!    `{ "apisets": { "models": [...], "checkedAt": <ms>, "etag": "<TAG>" } }`，
//!    与 pi 写的完全一致 —— 用户两边同时用 pi 和 toiler 时互不打架；
//! 2. `etag` 不匹配（pi 有过旧版本）就整份作废，只认 [`STORE_ETAG`]；
//! 3. 缓存 [`CACHE_TTL`] 内不再打网络；过期后由调用方决定要不要刷（设置页看到
//!    空列表、或启动预热时）；
//! 4. **只有拿到 API Key 才拉网络**：没凭据时离线返回缓存，拉不到就保持旧缓存，
//!    绝不清空（pi 的 `refreshModels` 同样在拿不到 key 时直接 `return cache`）。
//!
//! ## 两种协议
//!
//! apisets 的模型分两套协议：`gpt-*` 走上游的 Responses（与 Codex 同款线协议），
//! 其余（glm / kimi / qwen）走 Anthropic Messages。而 rpi 的 harness 只按
//! `model.provider` 找 provider（`find(|p| p.id() == model.provider)`，见
//! `rpi-harness/src/agent_harness.rs` 的 `resolve_provider`），一个 id 只能挂一个
//! provider 实例。因此这里给 `apisets` 写一个按 `model.api` 分发的包装
//! （[`ApiSetsProvider`]），内部持两个 rpi 自带 provider —— 这是把 pi 的
//! 「按 model.api 选流式实现」落到 rpi 上的等价做法。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use rpi_ai::model::AnthropicMessagesCompat;
use rpi_ai::providers::anthropic::AnthropicProvider;
use rpi_ai::providers::openai_responses::OpenAiResponsesProvider;
use rpi_ai::{
    Api, AssistantMessageEventStream, Context as AiContext, InputModality, Model, Provider,
    SimpleStreamOptions, StreamingProtocolCompat,
};
use serde_json::{json, Value};

/// provider id（`auth.json` 的键、`enabledModels` 的前缀、`models-store.json` 的键）。
pub const PROVIDER_ID: &str = "apisets";

/// 上游 base url（`/v1/models` 也挂在它下面）。
pub const BASE_URL: &str = "https://coding.apisets.com";

/// 缓存格式版本。与 pi 的 `apisets-models-api-v2` 逐字一致：etag 对不上就整份
/// 作废，避免读到旧版本（v1 只有 chat 之外的模型）留下的半截数据。
pub const STORE_ETAG: &str = "apisets-models-api-v2";

/// 缓存有效期（pi：30 分钟）。过期后允许联网刷新，离线时仍然继续用缓存。
pub const CACHE_TTL: Duration = Duration::from_secs(30 * 60);

/// 上游没给 `contextWindow` 时的兜底（pi 用 128k）。
pub const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;

/// 上游没给 `maxTokens` 时的兜底（pi 用 16k）。
pub const DEFAULT_MAX_TOKENS: u64 = 16_384;

/// 拉目录的超时（pi 的 `AbortSignal.timeout(20e3)`）。
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// `models-store.json` 里属于本 provider 的一段。
#[derive(Debug, Clone, PartialEq)]
pub struct StoredCatalog {
    /// pi 形状的模型定义（见 [`model_def`]）。
    pub models: Vec<Value>,
    /// 上次落盘的毫秒时间戳。
    pub checked_at: i64,
    /// 写这份缓存的格式版本。
    pub etag: String,
}

impl StoredCatalog {
    /// 缓存是否还在 [`CACHE_TTL`] 内。
    pub fn is_fresh(&self) -> bool {
        let age = now_ms() - self.checked_at;
        age >= 0 && (age as u64) < CACHE_TTL.as_millis() as u64
    }

    /// 缓存里的模型定义是否可用（etag 与 types 都过一遍）。
    pub fn usable_defs(&self) -> Vec<Value> {
        if self.etag != STORE_ETAG {
            return Vec::new();
        }
        self.models
            .iter()
            .filter(|def| def.get("provider").and_then(Value::as_str) == Some(PROVIDER_ID))
            .filter(|def| is_chat_model(def))
            .cloned()
            .collect()
    }
}

/// 缓存文件路径：与 `models.json` 同目录（`<toilerHome>/configs/models-store.json`）。
///
/// 不用 `paths::config_file` 是为了让测试能通过 `ModelsFile::at(tmp/…)` 决定位置，
/// 不依赖进程级环境变量（回归测试会并行跑）。
pub fn store_path(models_json: &Path) -> PathBuf {
    models_json.with_file_name("models-store.json")
}

/// 读缓存；文件不存在 / 坏 JSON / 没有本 provider 的一段 → `None`。
pub fn read_store(models_json: &Path) -> Option<StoredCatalog> {
    let text = std::fs::read_to_string(store_path(models_json)).ok()?;
    let root: Value = serde_json::from_str(&text).ok()?;
    let entry = root.get(PROVIDER_ID)?;
    let models = entry
        .get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Some(StoredCatalog {
        models,
        checked_at: entry.get("checkedAt").and_then(Value::as_i64).unwrap_or(0),
        etag: entry
            .get("etag")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

/// 缓存里的可用模型定义（etag 不匹配时是空表）。
pub fn cached_defs(models_json: &Path) -> Vec<Value> {
    read_store(models_json)
        .map(|store| store.usable_defs())
        .unwrap_or_default()
}

/// 写缓存：只动 `apisets` 这一段，文件里 pi 写的其它 provider 原样保留。
pub fn write_store(models_json: &Path, models: &[Value]) -> std::io::Result<()> {
    let path = store_path(models_json);
    let mut root = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or_else(|| json!({}));
    if !root.is_object() {
        root = json!({});
    }
    let Some(obj) = root.as_object_mut() else {
        return Ok(());
    };
    obj.insert(
        PROVIDER_ID.to_string(),
        json!({
            "models": models,
            "checkedAt": now_ms(),
            "etag": STORE_ETAG,
        }),
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".into());
    // 先写临时文件再 rename：半截 JSON 会让下次启动读到空目录。
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, body)?;
    crate::auth::set_mode_600(&tmp)?;
    std::fs::rename(&tmp, &path)?;
    crate::auth::set_mode_600(&path)?;
    Ok(())
}

/// 打上游 `/v1/models`，把结果映射成 pi 形状的模型定义并落盘。
///
/// 返回刚写进缓存的定义；上游 200 但一个模型都没有时按失败处理（与 pi 一致：
/// `APIsets model catalog did not return any models`）。
pub async fn refresh(api_key: &str, models_json: &Path) -> Result<Vec<Value>> {
    let key = api_key.trim();
    if key.is_empty() {
        bail!("apisets 还没有 API Key");
    }
    let endpoint = format!("{BASE_URL}/v1/models");
    let response = reqwest::Client::new()
        .get(&endpoint)
        .timeout(FETCH_TIMEOUT)
        .header("accept", "application/json")
        .header("authorization", format!("Bearer {key}"))
        .send()
        .await
        .context("请求 apisets 模型目录失败")?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        // 上游的 401 会带一段英文说明，原样给出去比「HTTP 401」好排查。
        let detail = text.trim();
        let detail: String = detail.chars().take(300).collect();
        if detail.is_empty() {
            bail!("apisets 模型目录返回 HTTP {}", status.as_u16());
        }
        bail!("apisets 模型目录返回 HTTP {}：{detail}", status.as_u16());
    }
    let payload: Value = serde_json::from_str(&text).context("apisets 模型目录不是合法 JSON")?;
    let discovered = crate::routes::models_config::parse_upstream_models(&payload);
    let defs: Vec<Value> = discovered.iter().filter_map(model_def_for).collect();
    if defs.is_empty() {
        bail!("apisets 模型目录没有返回任何模型");
    }
    write_store(models_json, &defs).context("写 models-store.json 失败")?;
    Ok(defs)
}

/// 一条上游模型 → pi 形状的模型定义。
///
/// 字段逐个对齐 pi 的 `apiSetsModels()`（`lib/desktop-providers.ts`）：
/// `provider` / `api` / `baseUrl` 必须随模型一起存 —— 恢复缓存时要按 `provider`
/// 过滤，而 `api` 决定这条模型走哪套线协议。
pub fn model_def_for(upstream: &Value) -> Option<Value> {
    let id = upstream
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())?;
    let name = upstream
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(id);
    let mut def = json!({
        "id": id,
        "name": name,
        "provider": PROVIDER_ID,
        "api": api_for(id),
        "baseUrl": BASE_URL,
        "reasoning": true,
        "input": ["text", "image"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": upstream
            .get("contextWindow")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_CONTEXT_WINDOW),
        "maxTokens": upstream
            .get("maxTokens")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_MAX_TOKENS),
    });
    // glm-5.3 的思考块签名会是空串，Anthropic Messages 适配要放宽校验，
    // 否则第二轮回放上下文时被判成非法签名（pi 的同一处 workaround）。
    if id == "glm-5.3" {
        if let Some(obj) = def.as_object_mut() {
            obj.insert("compat".to_string(), json!({ "allowEmptySignature": true }));
        }
    }
    Some(def)
}

/// 模型 id → 线协议。`gpt-*` 走 Responses，其余走 Anthropic Messages。
pub fn api_for(id: &str) -> &'static str {
    if id.starts_with("gpt-") {
        "openai-responses"
    } else {
        "anthropic-messages"
    }
}

/// 只有 chat 模型进目录（pi 的 `isModelType(model, "chat")`）。
fn is_chat_model(def: &Value) -> bool {
    matches!(
        def.get("api").and_then(Value::as_str),
        Some("openai-completions") | Some("openai-responses") | Some("anthropic-messages")
    )
}

/// pi 形状的定义 → rpi 的 [`Model`]。
pub fn models(defs: &[Value]) -> Vec<Model> {
    defs.iter().filter_map(model_from_def).collect()
}

fn model_from_def(def: &Value) -> Option<Model> {
    let id = def.get("id").and_then(Value::as_str)?.trim();
    if id.is_empty() {
        return None;
    }
    let api = api_from_str(
        def.get("api")
            .and_then(Value::as_str)
            .unwrap_or(api_for(id)),
    )?;
    let name = def.get("name").and_then(Value::as_str).unwrap_or(id);
    let base_url = def
        .get("baseUrl")
        .and_then(Value::as_str)
        .unwrap_or(BASE_URL);
    let mut model = Model::new(id, name, api.clone(), PROVIDER_ID, base_url);
    model.reasoning = def
        .get("reasoning")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    model.context_window = def
        .get("contextWindow")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_CONTEXT_WINDOW);
    model.max_tokens = def
        .get("maxTokens")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_TOKENS);
    model.input = input_modalities(def);
    model.compat = compat_from_def(def, api);
    Some(model)
}

fn input_modalities(def: &Value) -> Vec<InputModality> {
    let Some(items) = def.get("input").and_then(Value::as_array) else {
        return vec![InputModality::Text];
    };
    let modalities: Vec<InputModality> = items
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|value| match value {
            "text" => Some(InputModality::Text),
            "image" => Some(InputModality::Image),
            _ => None,
        })
        .collect();
    if modalities.is_empty() {
        vec![InputModality::Text]
    } else {
        modalities
    }
}

fn compat_from_def(def: &Value, api: Api) -> Option<StreamingProtocolCompat> {
    let compat = def.get("compat")?.as_object()?;
    if api != Api::AnthropicMessages {
        // 目前只有 Anthropic Messages 侧的 `allowEmptySignature` 有意义；
        // 其它协议的 compat 先不透传，免得把 rpi 不认识的键塞进模型对象。
        return None;
    }
    let allow_empty_signature = compat
        .get("allowEmptySignature")
        .and_then(Value::as_bool)
        .filter(|value| *value)?;
    Some(StreamingProtocolCompat::AnthropicMessages(
        AnthropicMessagesCompat {
            allow_empty_signature: Some(allow_empty_signature),
            ..Default::default()
        },
    ))
}

fn api_from_str(raw: &str) -> Option<Api> {
    match raw {
        "openai-responses" => Some(Api::OpenaiResponses),
        "openai-completions" => Some(Api::OpenaiCompletions),
        "anthropic-messages" => Some(Api::AnthropicMessages),
        _ => None,
    }
}

/// `apisets` 的 provider：按 `model.api` 把请求分发给两套 rpi 实现。
///
/// 为什么需要这层：rpi 的 harness 只按 `model.provider` 解析 provider
/// （`resolve_provider` → `find(|p| p.id() == model.provider)`），而 apisets
/// 的 18 个模型横跨 `anthropic-messages` 与 `openai-responses` 两套协议。注册两个
/// 同 id 的 provider 只会让 `find` 永远命中第一个，另一半模型必然调错协议。
pub struct ApiSetsProvider {
    anthropic: AnthropicProvider,
    responses: OpenAiResponsesProvider,
    models: Vec<Model>,
}

impl ApiSetsProvider {
    /// 用同一把 key 建两套实现，各自只持自己协议的模型。
    pub fn new(api_key: &str, models: Vec<Model>) -> Self {
        let http = reqwest::Client::new();
        let anthropic_models: Vec<Model> = models
            .iter()
            .filter(|model| model.api != Api::OpenaiResponses)
            .cloned()
            .collect();
        let responses_models: Vec<Model> = models
            .iter()
            .filter(|model| model.api == Api::OpenaiResponses)
            .cloned()
            .collect();
        Self {
            anthropic: AnthropicProvider::with_models_without_env_api_key(
                Some(api_key.to_string()),
                http.clone(),
                anthropic_models,
            ),
            responses: OpenAiResponsesProvider::with_models_without_env_api_key(
                PROVIDER_ID,
                Some(api_key.to_string()),
                http,
                responses_models,
            ),
            models,
        }
    }
}

#[async_trait::async_trait]
impl Provider for ApiSetsProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn models(&self) -> &[Model] {
        &self.models
    }

    async fn stream_simple(
        &self,
        model: &Model,
        ctx: &AiContext,
        opts: &SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        if model.api == Api::OpenaiResponses {
            self.responses.stream_simple(model, ctx, opts).await
        } else {
            self.anthropic.stream_simple(model, ctx, opts).await
        }
    }
}

/// 把模型列表包成 provider。
pub fn provider(api_key: &str, models: Vec<Model>) -> ApiSetsProvider {
    ApiSetsProvider::new(api_key, models)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 上游 `/v1/models` 的一条 —— 这里用 `parse_upstream_models` 归一后的形状
    /// （`contextWindow` 是驼峰：上游的 `context_window` 在那一步就换掉了）。
    fn upstream(id: &str) -> Value {
        json!({ "id": id, "name": id.to_uppercase(), "contextWindow": 200_000 })
    }

    #[test]
    fn gpt_models_go_through_responses() {
        assert_eq!(api_for("gpt-6-sol"), "openai-responses");
        assert_eq!(api_for("glm-5.3"), "anthropic-messages");
        assert_eq!(api_for("qwen3.8-max"), "anthropic-messages");
    }

    #[test]
    fn def_carries_provider_api_and_base_url() {
        let def = model_def_for(&upstream("gpt-6-sol")).unwrap();
        assert_eq!(def["provider"], PROVIDER_ID);
        assert_eq!(def["api"], "openai-responses");
        assert_eq!(def["baseUrl"], BASE_URL);
        assert_eq!(def["contextWindow"], 200_000);
        // 上游没给 maxTokens → 用兜底
        assert_eq!(def["maxTokens"], DEFAULT_MAX_TOKENS);
        assert!(def.get("compat").is_none());
    }

    #[test]
    fn glm_53_carries_the_empty_signature_compat() {
        let def = model_def_for(&upstream("glm-5.3")).unwrap();
        assert_eq!(def["compat"]["allowEmptySignature"], true);
    }

    #[test]
    fn models_split_by_api_and_keep_windows() {
        let defs: Vec<Value> = ["glm-5.3", "gpt-6-sol"]
            .iter()
            .map(|id| model_def_for(&upstream(id)).unwrap())
            .collect();
        let models = models(&defs);
        assert_eq!(models.len(), 2);
        let anthropic = models.iter().find(|m| m.id == "glm-5.3").unwrap();
        assert_eq!(anthropic.api, Api::AnthropicMessages);
        assert_eq!(anthropic.provider, PROVIDER_ID);
        assert_eq!(anthropic.context_window, 200_000);
        assert_eq!(anthropic.max_tokens, DEFAULT_MAX_TOKENS);
        assert!(anthropic
            .compat
            .as_ref()
            .and_then(StreamingProtocolCompat::as_anthropic)
            .and_then(|compat| compat.allow_empty_signature)
            .unwrap_or(false));
        let responses = models.iter().find(|m| m.id == "gpt-6-sol").unwrap();
        assert_eq!(responses.api, Api::OpenaiResponses);
    }

    #[test]
    fn provider_dispatches_by_model_api() {
        let defs: Vec<Value> = ["glm-5.3", "gpt-6-sol"]
            .iter()
            .map(|id| model_def_for(&upstream(id)).unwrap())
            .collect();
        let models_all = models(&defs);
        let provider = provider("sk-test", models_all.clone());
        assert_eq!(provider.id(), PROVIDER_ID);
        assert_eq!(provider.models().len(), 2);
        // 两套实现各自只持自己协议的模型
        assert_eq!(provider.anthropic.models().len(), 1);
        assert_eq!(provider.responses.models().len(), 1);
    }

    #[test]
    fn store_round_trip_keeps_other_providers() {
        let dir = std::env::temp_dir().join(format!("toiler-apisets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let models_json = dir.join("models.json");
        let store = store_path(&models_json);
        // pi 写的另一家：不能被我们写坏
        std::fs::write(
            &store,
            json!({ "deepseek": { "models": [], "checkedAt": 1, "etag": "models.dev" } })
                .to_string(),
        )
        .unwrap();

        let defs = vec![model_def_for(&upstream("glm-5.3")).unwrap()];
        write_store(&models_json, &defs).unwrap();

        let root: Value = serde_json::from_str(&std::fs::read_to_string(&store).unwrap()).unwrap();
        assert_eq!(root["deepseek"]["etag"], "models.dev");
        assert_eq!(root[PROVIDER_ID]["etag"], STORE_ETAG);

        let read = read_store(&models_json).unwrap();
        assert_eq!(read.usable_defs().len(), 1);
        assert!(read.is_fresh());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_etag_invalidates_the_whole_segment() {
        let store = StoredCatalog {
            models: vec![model_def_for(&upstream("glm-5.3")).unwrap()],
            checked_at: now_ms(),
            etag: "apisets-models-api-v1".to_string(),
        };
        assert!(store.usable_defs().is_empty(), "旧 etag 必须整份作废");
    }

    #[test]
    fn stale_cache_is_not_fresh_but_still_usable() {
        let store = StoredCatalog {
            models: vec![model_def_for(&upstream("glm-5.3")).unwrap()],
            checked_at: now_ms() - CACHE_TTL.as_millis() as i64 - 1,
            etag: STORE_ETAG.to_string(),
        };
        assert!(!store.is_fresh());
        // 过期不等于作废：没有网络时仍要用这份缓存
        assert_eq!(store.usable_defs().len(), 1);
    }

    #[test]
    fn non_chat_and_foreign_models_are_filtered_out() {
        let mut def = model_def_for(&upstream("glm-5.3")).unwrap();
        def["api"] = json!("some-image-api");
        let store = StoredCatalog {
            models: vec![
                def,
                json!({ "id": "x", "provider": "other", "api": "anthropic-messages" }),
            ],
            checked_at: now_ms(),
            etag: STORE_ETAG.to_string(),
        };
        assert!(store.usable_defs().is_empty());
    }
}
