//! toiler PoC — 验证三件事，都不改 rpi 源码：
//!
//! 1. crates.io 上的 `rpi-ai` 能被独立项目引入并编译（升级只需改版本号）。
//! 2. `openai-responses` provider 对自定义 baseUrl 端点真实流式出字。
//! 3. 从 pi 的 `models.json` 直接映射出 rpi 的 `Model`（provider 配置无需改写）。
//!
//! 用法：
//!   cargo run -p toiler-poc -- stream   # 打通 #2 #3（会真实发请求）
//!   cargo run -p toiler-poc -- info     # 只做配置映射，不发请求

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use rpi_ai::{
    providers::openai_responses::OpenAiResponsesProvider, Api, AssistantMessageEvent,
    Context as AiContext, InputModality, Message, Model, Provider, SimpleStreamOptions,
    UserMessage,
};
use serde_json::Value;

/// pi 的 `models.json` 与 rpi 的 `ProviderConfig` 是同一套字段名
/// （`api` / `baseUrl` / `apiKey` / `models[].{id,name,contextWindow,maxTokens,reasoning,input}`）。
/// 这里只读不写，pi 的原文件一个字都不动。
fn pi_models_path() -> Result<std::path::PathBuf> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow!("HOME 未设置"))?;
    Ok(home.join(".pi/agent/models.json"))
}

struct Resolved {
    provider_id: String,
    api_key: String,
    model: Model,
}

/// 从 pi 的 models.json 里挑第一个 `api: openai-responses` 的模型，
/// 映射成 rpi 的 `Model`。这正是将来 toiler 的 provider 加载层要做的事。
fn resolve_from_pi_config(wanted_model: Option<&str>) -> Result<Resolved> {
    let path = pi_models_path()?;
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("读不到 {}", path.display()))?;
    let config: Value = serde_json::from_str(&text).context("pi models.json 不是合法 JSON")?;

    let providers = config
        .get("providers")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("models.json 里没有 providers"))?;

    for (provider_id, provider) in providers {
        let api = provider.get("api").and_then(Value::as_str).unwrap_or("");
        if api != "openai-responses" {
            continue;
        }
        let Some(base_url) = provider.get("baseUrl").and_then(Value::as_str) else {
            continue;
        };
        let api_key = provider
            .get("apiKey")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let models = provider
            .get("models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        for def in models {
            let Some(id) = def.get("id").and_then(Value::as_str) else {
                continue;
            };
            if let Some(wanted) = wanted_model {
                if id != wanted {
                    continue;
                }
            }
            let mut model = Model::new(
                id,
                def.get("name").and_then(Value::as_str).unwrap_or(id),
                Api::OpenaiResponses,
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
            if let Some(input) = def.get("input").and_then(Value::as_array) {
                model.input = input
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(|m| match m {
                        "text" => Some(InputModality::Text),
                        "image" => Some(InputModality::Image),
                        _ => None,
                    })
                    .collect();
            }
            return Ok(Resolved {
                provider_id: provider_id.clone(),
                api_key,
                model,
            });
        }
    }
    Err(anyhow!(
        "pi models.json 里没有可用的 openai-responses 模型（wanted={:?}）",
        wanted_model
    ))
}

#[tokio::main]
async fn main() -> Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "info".into());
    let wanted = std::env::args().nth(2);

    let resolved = resolve_from_pi_config(wanted.as_deref())?;
    println!("✔ 配置映射成功（pi 的 models.json 原样读取）");
    println!("  provider : {}", resolved.provider_id);
    println!(
        "  model    : {} ({})",
        resolved.model.id, resolved.model.name
    );
    println!("  baseUrl  : {}", resolved.model.base_url);
    println!("  api      : {:?}", resolved.model.api);
    println!(
        "  context  : {} / maxTokens {} / reasoning {}",
        resolved.model.context_window, resolved.model.max_tokens, resolved.model.reasoning
    );
    println!(
        "  apiKey   : {}…",
        &resolved.api_key[..resolved.api_key.len().min(8)]
    );

    if mode != "stream" {
        return Ok(());
    }

    println!("\n→ 发起真实流式请求…");
    // with_models_without_env_api_key：自定义端点不继承进程级 OPENAI_API_KEY
    let provider = OpenAiResponsesProvider::with_models_without_env_api_key(
        resolved.provider_id.clone(),
        Some(resolved.api_key.clone()),
        reqwest::Client::new(),
        vec![resolved.model.clone()],
    );

    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
    let ctx = AiContext::new(vec![Message::User(UserMessage::new(
        "只用一句中文回答：你现在能正常流式输出吗？",
        now,
    ))]);

    let opts = SimpleStreamOptions {
        timeout: Some(Duration::from_secs(90)),
        max_retries: Some(1),
        ..Default::default()
    };

    let mut stream = provider.stream_simple(&resolved.model, &ctx, &opts).await;
    let mut deltas = 0usize;
    let mut text = String::new();
    let started = std::time::Instant::now();
    let mut first_delta_at = None;

    while let Some(event) = stream.next().await {
        match event {
            AssistantMessageEvent::TextDelta { delta, .. } => {
                if first_delta_at.is_none() {
                    first_delta_at = Some(started.elapsed());
                }
                deltas += 1;
                text.push_str(&delta);
                print!("{delta}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
            AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                if first_delta_at.is_none() {
                    first_delta_at = Some(started.elapsed());
                }
                deltas += 1;
                print!("\x1b[2m{delta}\x1b[0m");
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
            AssistantMessageEvent::Done { reason, .. } => {
                println!("\n\n✔ 流结束 reason={reason:?}");
            }
            AssistantMessageEvent::Error { reason, error } => {
                println!("\n\n✘ 流错误 reason={reason:?}");
                println!("{error:#?}");
                return Ok(());
            }
            _ => {}
        }
    }

    println!("  deltas      : {deltas}");
    println!("  首字延迟    : {:?}", first_delta_at.unwrap_or_default());
    println!("  总耗时      : {:?}", started.elapsed());
    println!("  输出长度    : {} 字符", text.chars().count());
    Ok(())
}
