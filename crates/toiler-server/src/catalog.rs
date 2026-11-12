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
    Api, InputModality, Model, Provider, ThinkingLevel, ThinkingLevelMap,
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

    /// `/api/models` 里 `models` 字典的键：`provider:id`（**冒号**）。
    ///
    /// 注意与 `thinkingLevelPins` 的键 `provider/id`（**斜杠**）不同 —— pi 前端
    /// 两个地方各用各的分隔符，抄错一个下拉框就整片空白。
    pub fn catalog_key(&self) -> String {
        format!("{}:{}", self.provider, self.id)
    }

    /// `/api/models` 里 `modelList` 的一项（pi 只取 `id`/`name`/`provider`）。
    pub fn to_list_json(&self) -> Value {
        serde_json::json!({
            "provider": self.provider,
            "id": self.id,
            "name": self.name,
        })
    }
}

/// 支持 - 或不支持 - 一种 `enabledModels` 模式钉住的思考等级。
///
/// 等级值取自 `~/.pi/agent/models.json` 的 `thinkingLevelMap`，与 pi 一致：
///
/// | 场景 | `thinkingLevels` | `thinkingLevelPins` |
/// |---|---|---|
/// | 模型不支持推理 | `["off"]` | `None` |
/// | 缺 `thinkingLevelMap` | `["off","minimal","low","medium","high"]` | `None` |
/// | 有 `thinkingLevelMap` | `["off","minimal","low","medium","high"]` | `None` |
/// | 同上且 `xhigh`/`max` 是**非 null** 字符串 | 追加 `"xhigh"`/`"max"` | `None` |
/// | 该等级在 `thinkingLevelMap` 里为 `null` | 从结果里剔除 | `None` |
/// | 模型被 `enabledModels` 的 `provider/id:level` 钉住 | 不变 | `Some("level")` |
///
/// Thinking levels, mirroring pi's `ThinkingLevel | "off"` literally: pi 的
/// `EXTENDED_THINKING_LEVELS` 顺序就是下面这个，前端按它排下拉框。
const EXTENDED_THINKING_LEVELS: [ThinkingLevel; 7] = [
    ThinkingLevel::Off,
    ThinkingLevel::Minimal,
    ThinkingLevel::Low,
    ThinkingLevel::Medium,
    ThinkingLevel::High,
    ThinkingLevel::Xhigh,
    ThinkingLevel::Max,
];

/// 复刻 pi 的 `getSupportedThinkingLevels`（`pi-ai/dist/models.js:693`）。
///
/// 语义必须逐条对齐，否则前端下拉框会和真正能发出去的参数不一致：
///
/// 1. `reasoning == false` → 只有 `["off"]`（其它等级对这个模型没意义）。
/// 2. 遍历**全部** 7 个等级，遇到 `thinkingLevelMap[level] === null` 就剔除。
/// 3. `xhigh` / `max` 额外要求该条目**存在且非 null**（缺省不算支持）。
///
/// rpi 的 `Model::supported_thinking_levels()` 语义不同，**不能直接复用**：
/// 它对非推理模型仍返回 `["off",…,"high"]`，而 pi 只给 `["off"]`；前端拿到
/// 多余的等级时会在「不支持推理的模型」上显示出推理强度菜单。
pub fn supported_thinking_levels(model: &Model) -> Vec<&'static str> {
    if !model.reasoning {
        return vec!["off"];
    }
    let map = model.thinking_level_map.as_ref();
    EXTENDED_THINKING_LEVELS
        .iter()
        .filter(|level| {
            let mapped = map.and_then(|m| m.get(level));
            if matches!(mapped, Some(None)) {
                return false;
            }
            if matches!(level, ThinkingLevel::Xhigh | ThinkingLevel::Max) {
                return matches!(mapped, Some(Some(_)));
            }
            true
        })
        .map(|level| match level {
            ThinkingLevel::Off => "off",
            ThinkingLevel::Minimal => "minimal",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::Xhigh => "xhigh",
            ThinkingLevel::Max => "max",
        })
        .collect()
}

#[derive(Clone)]
pub struct ModelCatalog {
    pub entries: Vec<ModelEntry>,
    /// rpi 的模型对象（harness 要它）
    pub models: Vec<Model>,
    /// 每个 provider 一个实例，harness 按 `model.provider` 解析
    pub providers: Vec<Arc<dyn Provider>>,
    /// `~/.pi/agent/models.json` 的原始内容（前端 `/api/models` 会透传一部分）
    pub raw: Value,
    pub source: Option<std::path::PathBuf>,
    /// 默认模型下标（`entries` 里第一个可用的；可被 `TOILER_MODEL` 或历史会话覆盖）
    default_index: usize,
}

impl ModelCatalog {
    /// 默认路径：`$TOILER_MODELS` > `~/.toiler/agent/models.json` > `~/.pi/agent/models.json`
    /// 模型目录：`$TOILER_MODELS` > `<toilerHome>/configs/models.json`。
    ///
    /// 早先这里自己拼 `~/.toiler/agent` → `~/.pi/agent` 两条路径，与
    /// `paths::config_file` 各写一份，很容易漂移（阶段 6 换布局时就漏了这里，
    /// 表现为「会话根换了、模型目录还在读 pi」）。统一走 `paths`。
    pub fn default_path() -> Option<std::path::PathBuf> {
        if let Ok(p) = std::env::var("TOILER_MODELS") {
            if !p.trim().is_empty() {
                return Some(std::path::PathBuf::from(p));
            }
        }
        Some(crate::paths::config_file("models.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::default_path();
        let Some(path) = path else {
            return Ok(Self::empty());
        };
        if !path.exists() {
            return Ok(Self::empty());
        }
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("读不到 {}", path.display()))?;
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
                // `thinkingLevelMap` 必须解析进来：`supported_thinking_levels()`
                // 靠它决定剔除哪些等级；丢掉它就会把 `null`（不支持）当成支持。
                model.thinking_level_map = parse_thinking_level_map(def.get("thinkingLevelMap"));

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

    /// 把内置 `qoder-cn` 目录注入一份副本（`base_url` 指向本机转发端点）。
    ///
    /// `qoder-cn` 是订阅 provider：模型清单在 pi 侧是静态快照，不走 `models.json`。
    /// 用户自己在 `models.json` 里写了同名 provider 的话，调用方不该再注入
    /// （[`crate::state::AppState::attach_qoder_cn`] 会先查一遍）。
    pub fn with_qoder_cn(mut self, base_url: &str) -> Self {
        let provider_id = crate::qoder_cn::PROVIDER_ID;
        if self.providers.iter().any(|p| p.id() == provider_id) {
            return self;
        }
        let models = crate::qoder_cn::models(base_url);
        self.entries.extend(models.iter().map(entry_for_model));
        self.models.extend(models.iter().cloned());
        self.providers
            .push(Arc::new(crate::qoder_cn::provider(base_url)));
        self
    }

    /// 把内置 `apisets`（设置页里叫 **Coding**）的目录注入一份副本。
    ///
    /// 模型清单来自远端缓存（[`crate::apisets`] 的 `models-store.json`），由调用方
    /// 读好再传进来 —— `catalog` 本身不做 IO，也不联网。
    ///
    /// 与 `qoder-cn` 不同的是这层会**先摘掉旧的 apisets**：刷新一次目录后模型会变，
    /// 只 add 不 remove 会让同一个 id 在目录里出现两次。
    pub fn with_apisets(mut self, api_key: &str, defs: &[serde_json::Value]) -> Self {
        self = self.without_provider(crate::apisets::PROVIDER_ID);
        let models = crate::apisets::models(defs);
        if models.is_empty() {
            return self;
        }
        self.entries.extend(models.iter().map(entry_for_model));
        self.models.extend(models.iter().cloned());
        self.providers
            .push(Arc::new(crate::apisets::provider(api_key, models)));
        self
    }

    /// 摘掉某个 provider 的 entries / models / providers 实例。
    pub fn without_provider(mut self, provider_id: &str) -> Self {
        self.entries.retain(|entry| entry.provider != provider_id);
        self.models
            .retain(|model| model.provider.as_str() != provider_id);
        self.providers
            .retain(|provider| provider.id() != provider_id);
        self
    }

    /// 选默认模型：`TOILER_MODEL=provider/id` > 历史会话提示 > 第一个。
    ///
    /// 历史会话提示（`hint`）来自最近一个会话的 `model_change` 条目——也就是
    /// 「你上次用的模型」，这比随便挑第一个更符合直觉。
    pub fn apply_default_hint(&mut self, hint: Option<(String, String)>) {
        let env_hint = std::env::var("TOILER_MODEL").ok().and_then(|raw| {
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

/// 把 rpi 的 [`Model`] 变成 `/api/models` 用的目录条目。
///
/// 内置目录（`qoder-cn` / `apisets`）都是先造 `Model`（harness 要它）再反推出条目，
/// 两处必须用同一套映射：`api` 取模型自己的协议（`apisets` 的模型横跳
/// `anthropic-messages` / `openai-responses`，不能像 `models.json` 那样按 provider 写死）。
fn entry_for_model(model: &Model) -> ModelEntry {
    ModelEntry {
        provider: model.provider.clone(),
        id: model.id.clone(),
        name: model.name.clone(),
        api: model.api.as_str().to_string(),
        context_window: model.context_window,
        max_tokens: model.max_tokens,
        reasoning: model.reasoning,
        input: model
            .input
            .iter()
            .map(|modality| match modality {
                InputModality::Text => "text".to_string(),
                InputModality::Image => "image".to_string(),
            })
            .collect(),
    }
}

/// 把 `models.json` 的 `thinkingLevelMap` 解析成 rpi 的 `ThinkingLevelMap`。
///
/// pi 的形状是 `{ low: "low", medium: null }`（值可以是字符串或 `null`）：
///
/// | JSON 值 | 含义 | 映射到 rpi |
/// |---|---|---|
/// | `"low"` 等字符串 | 显式映射到这个值 | `Some(Some("low"))` |
/// | `null` | 该等级**不支持** | `Some(None)` |
/// | 键不存在 | 用 provider 默认（= 支持） | 不放入 map |
/// | 无法识别的键（如 `highest`） | pi 也会忽略 | 跳过 |
fn parse_thinking_level_map(raw: Option<&Value>) -> Option<ThinkingLevelMap> {
    let obj = raw?.as_object()?;
    let mut map = ThinkingLevelMap::new();
    for (key, value) in obj {
        let level = match key.as_str() {
            "off" => ThinkingLevel::Off,
            "minimal" => ThinkingLevel::Minimal,
            "low" => ThinkingLevel::Low,
            "medium" => ThinkingLevel::Medium,
            "high" => ThinkingLevel::High,
            "xhigh" => ThinkingLevel::Xhigh,
            "max" => ThinkingLevel::Max,
            _ => continue,
        };
        match value {
            Value::Null => map.insert(level, None),
            Value::String(text) => map.insert(level, Some(text.clone())),
            _ => continue,
        };
    }
    if map.is_empty() {
        None
    } else {
        Some(map)
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
