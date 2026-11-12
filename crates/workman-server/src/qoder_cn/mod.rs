//! Qoder CN（`qoder-cn`）订阅 provider。
//!
//! pi-desktop 的 `lib/qoder-cn/` 有 ~1850 行 TS：设备码登录、COSY 签名、请求体改写、
//! SSE 信封解包、静态模型目录。这个模块是对它的 Rust 移植，目标与 pi 一致 ——
//! 登录一次之后，`qoder-cn` 的 15 个模型能真的对话。
//!
//! # 为什么要一个本机转发端点
//!
//! pi 的做法是给 SDK 换一个 `fetch`（`withQoderCnFetch`）：body 改写成 Qoder 的
//! agent 请求体、签名、发上游、把信封拆回标准 OpenAI chunk。rpi 的 provider
//! **不允许注入传输层**（`OpenAiCompletionsProvider` 内部自己持有 `reqwest::Client`），
//! 所以 workman 换了个等价的接缝：把模型的 `baseUrl` 指向**本进程自己的**端点
//! `http://127.0.0.1:<port>/qoder-cn/<token>`，再由 [`proxy`] 调 [`transport`]
//! 完成同样的改写与解包。
//!
//! | 环节 | pi | workman |
//! |---|---|---|
//! | 请求体改写 | `withQoderCnFetch` 拦 fetch | 本机端点里调 [`request_body`] |
//! | 签名 | `createCosySession` | [`cosy::Session`] |
//! | 信封解包 | fetch 返回改写后的 SSE | [`transport`] 返回标准 chunk 流 |
//! | chunk 解析 | SDK 的 OpenAI 适配器 | 同一个适配器（零风险复用） |
//!
//! `<token>` 是进程内随机串：这条端点会带着账号凭据去刷上游额度，不能被局域网里
//! 的其他人扫到；[`proxy`] 另外只接受 loopback 对端。
//!
//! # 与 pi 的已知差异
//!
//! - **模型目录只有静态快照**：pi 登录后会再拉一次官方 `model/list`；workman 用
//!   [`constants::STATIC_CATALOG`]，官方改显示名/上下架后要跟着升版本（与内置
//!   供应商表同一套维护方式）。
//! - **只有登录/刷新走 openapi**，`user/plan`、`quota/usage` 这些用量接口没移植
//!   （workman 的界面不显示 Qoder 套餐额度）。

pub mod auth;
pub mod base64;
pub mod constants;
pub mod cosy;
pub mod fingerprint;
pub mod proxy;
pub mod request_body;
pub mod transport;

use rpi_ai::providers::openai_completions::OpenAiCompletionsProvider;
use rpi_ai::{Api, InputModality, Model, ThinkingLevel, ThinkingLevelMap};
use serde_json::Value;

pub use constants::PROVIDER_ID;

/// 毫秒时间戳。
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// 模型注册用的本机 baseUrl（`http://127.0.0.1:<port>/qoder-cn/<token>`）。
pub fn local_base_url(port: u16, token: &str) -> String {
    format!("http://127.0.0.1:{port}/qoder-cn/{token}")
}

/// 目录条目 → rpi 的模型定义（对齐 pi 的 `toPiModel`）。
///
/// `thinkingLevelMap` 必须给全：pi 对「目录没写档位」的推理模型把七个等级全部显式
/// 映射为 `null`（即不在请求里带 effort），workman 的
/// [`crate::catalog::supported_thinking_levels`] 依赖同一语义决定下拉框。
fn thinking_level_map(entry: &constants::CatalogEntry) -> Option<ThinkingLevelMap> {
    if !entry.reasoning {
        return None;
    }
    let mut map = ThinkingLevelMap::new();
    let levels = [
        ThinkingLevel::Off,
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::Xhigh,
        ThinkingLevel::Max,
    ];
    if entry.efforts.is_empty() {
        // 目录没给档位：七个等级全部显式 null（= 不带 effort）。
        for level in levels {
            map.insert(level, None);
        }
        return Some(map);
    }
    for level in levels {
        let label = match level {
            ThinkingLevel::Off => "off",
            ThinkingLevel::Minimal => "minimal",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::Xhigh => "xhigh",
            ThinkingLevel::Max => "max",
        };
        let supported = entry.efforts.contains(&label);
        map.insert(level, supported.then(|| label.to_string()));
    }
    Some(map)
}

/// 把内置目录变成 rpi 的模型（`base_url` 指向本机转发端点）。
pub fn models(base_url: &str) -> Vec<Model> {
    constants::STATIC_CATALOG
        .iter()
        .map(|entry| {
            let mut model = Model::new(
                entry.key,
                entry.display_name,
                Api::OpenaiCompletions,
                PROVIDER_ID,
                base_url,
            );
            model.reasoning = entry.reasoning;
            model.context_window = entry.max_input_tokens;
            model.max_tokens = constants::DEFAULT_MAX_OUTPUT_TOKENS;
            model.input = if entry.vision {
                vec![InputModality::Text, InputModality::Image]
            } else {
                vec![InputModality::Text]
            };
            model.thinking_level_map = thinking_level_map(entry);
            model
        })
        .collect()
}

/// 把这批模型包成 rpi 的 provider（鉴权在本机端点里做，provider 本身匿名）。
///
/// 用 `with_models_without_auth`：它不会去读 `OPENAI_API_KEY` 之类的环境变量，
/// 也不会给请求加 `Authorization` —— 上游要的是 COSY 头，多一个 Bearer 只会干扰。
pub fn provider(base_url: &str) -> OpenAiCompletionsProvider {
    OpenAiCompletionsProvider::with_models_without_auth(
        PROVIDER_ID,
        reqwest::Client::new(),
        models(base_url),
    )
}

/// 目录条目 → `/api/models` 的 `models.json` 形状（给前端下拉框用的展示字段）。
pub fn model_definitions() -> Vec<Value> {
    constants::STATIC_CATALOG
        .iter()
        .map(|entry| {
            serde_json::json!({
                "id": entry.key,
                "name": entry.display_name,
                "contextWindow": entry.max_input_tokens,
                "maxTokens": constants::DEFAULT_MAX_OUTPUT_TOKENS,
                "reasoning": entry.reasoning,
                "input": if entry.vision { vec!["text", "image"] } else { vec!["text"] },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_cover_the_static_catalog() {
        let base = local_base_url(4321, "tok");
        let models = models(&base);
        assert_eq!(models.len(), 15);
        assert_eq!(models[0].id, "auto");
        assert_eq!(models[0].provider, PROVIDER_ID);
        assert_eq!(models[0].base_url, base);
        assert_eq!(models[0].api, Api::OpenaiCompletions);
        assert_eq!(models[0].context_window, 200_000);
        assert_eq!(models[0].max_tokens, constants::DEFAULT_MAX_OUTPUT_TOKENS);

        // 无档位的推理模型：七个等级全部显式 null（= 不下发 effort）
        let auto = &models[0];
        let map = auto.thinking_level_map.as_ref().expect("推理模型要有 map");
        assert_eq!(map.len(), 7);
        assert!(map.values().all(Option::is_none));

        // 有档位的：只放开目录声明的那几个
        let qwen = models.iter().find(|m| m.id == "qmodel_38max").unwrap();
        let map = qwen.thinking_level_map.as_ref().unwrap();
        assert_eq!(
            map.get(&ThinkingLevel::Medium),
            Some(&Some("medium".into()))
        );
        assert_eq!(map.get(&ThinkingLevel::Max), Some(&None));

        // 非推理模型：没有 map，输入只有文本
        let minimax = models.iter().find(|m| m.id == "mmodel").unwrap();
        assert!(minimax.thinking_level_map.is_none());
        assert_eq!(minimax.input, vec![InputModality::Text]);
    }

    #[test]
    fn model_definitions_match_the_catalog_shape() {
        let defs = model_definitions();
        assert_eq!(defs.len(), 15);
        assert_eq!(defs[0]["id"], "auto");
        assert_eq!(defs[0]["reasoning"], true);
        assert_eq!(defs[0]["input"][1], "image");
        assert_eq!(defs[13]["id"], "mmodel");
        assert!(defs[13]["input"].as_array().unwrap().len() == 1);
    }
}
