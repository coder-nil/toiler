//! 模型目录：从 pi 的 `models.json` 建 rpi 的 `Model` + `Provider`。
//!
//! pi 的字段名与 rpi 的 `ProviderConfig` 完全一致，所以这是纯映射，不改配置。
//! 每个 `api` 值对应一个 rpi provider 实现：
//!
//! | models.json `api` | rpi provider |
//! |---|---|
//! | `openai-responses` | `OpenAiResponsesProvider` |
//! | `openai-completions` | `OpenAiCompletionsProvider` |
//! | `anthropic-messages` | `AnthropicProvider` |
//! | 其它 | 跳过（记 warn） |

use std::sync::Arc;

use anyhow::{Context, Result};
use rpi_ai::{
    providers::{
        anthropic::AnthropicProvider, openai_completions::OpenAiCompletionsProvider,
        openai_responses::OpenAiResponsesProvider,
    },
    Api, InputModality, Model, Provider,
};
use serde_json::Value;

/// 一个模型在 `/api/models` 里的描述（前端只关心这几个字段）。
#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub api: String,
    pub context_window: u64,
    pub max_tokens: u64,
    pub reasoning: bool,
    pub input: Vec<String>,
}

impl ModelEntry {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "provider": self.provider,
            "id": self.id,
            "name": self.name,
            "api": self.api,
            "contextWindow": self.context_window,
            "maxTokens": self.max_tokens,
            "reasoning": self.reasoning,
            "input": self.input,
        })
    }
}

pub struct ModelCatalog {
    pub entries: Vec<ModelEntry>,
    /// rpi 的模型对象（harness 要它）
    pub models: Vec<Model>,
    /// 每个 provider 一个实例，harness 按 `model.provider` 解析
    pub providers: Vec<Arc<dyn Provider>>,
    /// `~/.pi/agent/models.json` 的原始内容（前端 `/api/models` 会透传一部分）
    pub raw: Value,
    pub source: Option<std::path::PathBuf>,
    /// 默认模型下标（`entries` 里第一个可用的；可被 `WORKMAN_MODEL` 或历史会话覆盖）
    default_index: usize,
}

impl ModelCatalog {
    /// 默认路径：`$WORKMAN_MODELS` > `~/.workman/agent/models.json` > `~/.pi/agent/models.json`
    pub fn default_path() -> Option<std::path::PathBuf> {
        if let Ok(p) = std::env::var("WORKMAN_MODELS") {
            if !p.trim().is_empty() {
                return Some(std::path::PathBuf::from(p));
            }
        }
        let home = std::env::var_os("HOME")?;
        let work = std::path::PathBuf::from(&home).join(".workman/agent/models.json");
        if work.exists() {
            return Some(work);
        }
        Some(std::path::PathBuf::from(home).join(".pi/agent/models.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::default_path();
        let Some(path) = path else {
            return Ok(Self::empty());
        };
        if !path.exists() {
            return Ok(Self::empty());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("读不到 {}", path.display()))?;
        let raw: Value = serde_json::from_str(&text)
            .with_context(|| format!("{} 不是合法 JSON", path.display()))?;
        Self::from_config(raw, Some(path))
    }

    fn empty() -> Self {
        Self {
            entries: Vec::new(),
            models: Vec::new(),
            providers: Vec::new(),
            raw: Value::Null,
            source: None,
            default_index: 0,
        }
    }

    pub fn from_config(raw: Value, source: Option<std::path::PathBuf>) -> Result<Self> {
        let http = reqwest::Client::new();
        let mut entries = Vec::new();
        let mut models = Vec::new();
        let mut providers: Vec<Arc<dyn Provider>> = Vec::new();

        let Some(configs) = raw.get("providers").and_then(Value::as_object) else {
            return Ok(Self {
                entries,
                models,
                providers,
                raw,
                source,
                default_index: 0,
            });
        };

        for (provider_id, cfg) in configs {
            let api = cfg.get("api").and_then(Value::as_str).unwrap_or("");
            let Some(base_url) = cfg.get("baseUrl").and_then(Value::as_str) else {
                continue;
            };
            let api_key = cfg
                .get("apiKey")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();

            let mut provider_models: Vec<Model> = Vec::new();
            for def in cfg
                .get("models")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let Some(id) = def.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let Some(rpi_api) = api_from_config(api) else {
                    continue;
                };
                let mut model = Model::new(
                    id,
                    def.get("name").and_then(Value::as_str).unwrap_or(id),
                    rpi_api,
                    provider_id.as_str(),
                    base_url,
                );
                model.reasoning = def
                    .get("reasoning")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                model.context_window = def
                    .get("contextWindow")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                model.max_tokens = def.get("maxTokens").and_then(Value::as_u64).unwrap_or(0);

                let mut input_labels = Vec::new();
                if let Some(input) = def.get("input").and_then(Value::as_array) {
                    let modalities: Vec<InputModality> = input
                        .iter()
                        .filter_map(Value::as_str)
                        .filter_map(|m| match m {
                            "text" => Some(InputModality::Text),
                            "image" => Some(InputModality::Image),
                            _ => None,
                        })
                        .collect();
                    input_labels = modalities
                        .iter()
                        .map(|m| match m {
                            InputModality::Text => "text".to_string(),
                            InputModality::Image => "image".to_string(),
                        })
                        .collect();
                    model.input = modalities;
                }

                entries.push(ModelEntry {
                    provider: provider_id.clone(),
                    id: id.to_string(),
                    name: model.name.clone(),
                    api: api.to_string(),
                    context_window: model.context_window,
                    max_tokens: model.max_tokens,
                    reasoning: model.reasoning,
                    input: input_labels,
                });
                provider_models.push(model.clone());
                models.push(model);
            }

            if provider_models.is_empty() {
                continue;
            }
            let provider: Option<Arc<dyn Provider>> = match api {
                "openai-responses" => Some(Arc::new(
                    OpenAiResponsesProvider::with_models_without_env_api_key(
                        provider_id.clone(),
                        Some(api_key),
                        http.clone(),
                        provider_models,
                    ),
                )),
                "openai-completions" => Some(Arc::new(
                    OpenAiCompletionsProvider::with_models_without_env_api_key(
                        provider_id.clone(),
                        Some(api_key),
                        http.clone(),
                        provider_models,
                    ),
                )),
                "anthropic-messages" => Some(Arc::new(
                    AnthropicProvider::with_models_without_env_api_key(
                        Some(api_key),
                        http.clone(),
                        provider_models,
                    ),
                )),
                _ => None,
            };
            if let Some(p) = provider {
                providers.push(p);
            }
        }

        let mut catalog = Self {
            entries,
            models,
            providers,
            raw,
            source,
            default_index: 0,
        };
        catalog.apply_default_hint(None);
        Ok(catalog)
    }

    /// 选默认模型：`WORKMAN_MODEL=provider/id` > 历史会话提示 > 第一个。
    ///
    /// 历史会话提示（`hint`）来自最近一个会话的 `model_change` 条目——也就是
    /// 「你上次用的模型」，这比随便挑第一个更符合直觉。
    pub fn apply_default_hint(&mut self, hint: Option<(String, String)>) {
        let env_hint = std::env::var("WORKMAN_MODEL").ok().and_then(|raw| {
            let (provider, id) = raw.split_once('/')?;
            Some((provider.to_string(), id.to_string()))
        });
        for candidate in [env_hint, hint].into_iter().flatten() {
            if let Some(index) = self
                .entries
                .iter()
                .position(|e| e.provider == candidate.0 && e.id == candidate.1)
            {
                if self
                    .providers
                    .iter()
                    .any(|p| p.id() == candidate.0.as_str())
                {
                    self.default_index = index;
                    return;
                }
            }
        }
        // 兜底：第一个有 provider 的模型
        if let Some(index) = self
            .entries
            .iter()
            .position(|e| self.providers.iter().any(|p| p.id() == e.provider.as_str()))
        {
            self.default_index = index;
        }
    }

    /// 当前默认模型。
    pub fn default_entry(&self) -> Option<&ModelEntry> {
        self.entries.get(self.default_index)
    }

    /// 默认模型对象（harness 要的）。
    pub fn default_model(&self) -> Option<&Model> {
        let entry = self.default_entry()?;
        self.model_for(&entry.provider, &entry.id)
    }

    pub fn model_for(&self, provider: &str, id: &str) -> Option<&Model> {
        self.models
            .iter()
            .find(|m| m.provider.as_str() == provider && m.id == id)
    }
}

fn api_from_config(api: &str) -> Option<Api> {
    match api {
        "openai-responses" => Some(Api::OpenaiResponses),
        "openai-completions" => Some(Api::OpenaiCompletions),
        "anthropic-messages" => Some(Api::AnthropicMessages),
        _ => None,
    }
}
