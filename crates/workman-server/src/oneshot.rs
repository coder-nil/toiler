//! 一次性补全（不带工具、不落会话）。
//!
//! 三个 route 需要「调一次模型拿一段文本」：
//!
//! | route | 用途 |
//! |---|---|
//! | `POST /api/sessions/{id}/auto-name` | 给会话起标题 |
//! | `POST /api/git/commit-message` | 从暂存区 diff 生成 commit message |
//! | `POST /api/models-config/test` | 新配的模型能不能通 |
//!
//! 都走 [`complete`]：一条 user 消息 → `Provider::stream_simple` → 收完流。
//! 没有工具、没有重试、没有会话持久化（与 pi 的 `completeSimple` 一致）。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use rpi_ai::types::{AssistantMessageEvent, Content, Context, Message, StopReason, ThinkingLevel, UserMessage};
use rpi_ai::{Api, Model, Provider, SimpleStreamOptions};

/// 一次补全的结果。
#[derive(Debug, Clone)]
pub struct Reply {
    pub text: String,
    pub usage: Option<rpi_ai::types::Usage>,
    pub stop_reason: StopReason,
}

/// 跑一次补全。
///
/// `api_key` 由调用方解析（`auth.json` / `models.json`）；`None` 时交给
/// provider 自己的环境变量兜底。
pub async fn complete(
    provider: Arc<dyn Provider>,
    model: &Model,
    api_key: Option<String>,
    prompt: String,
    max_tokens: u64,
    timeout: Duration,
) -> Result<Reply> {
    let context = Context::new(vec![Message::User(UserMessage::new(prompt, now_ms()))]);

    let mut options = SimpleStreamOptions::default();
    options.api_key = api_key;
    options.max_tokens = Some(max_tokens);
    options.timeout = Some(timeout);
    options.max_retries = Some(0);
    options.reasoning = Some(ThinkingLevel::Off);

    let mut stream = provider.stream_simple(model, &context, &options).await;
    let mut text = String::new();
    let mut usage = None;
    let mut stop_reason = StopReason::Stop;

    while let Some(event) = stream.next().await {
        match event {
            AssistantMessageEvent::Done { message, .. } => {
                usage = Some(message.usage.clone());
                stop_reason = message.stop_reason;
                let collected: String = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        Content::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if !collected.is_empty() {
                    return Ok(Reply {
                        text: collected,
                        usage,
                        stop_reason,
                    });
                }
            }
            AssistantMessageEvent::Error { error, .. } => {
                let message = error
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "模型返回错误".to_string());
                return Err(anyhow!(message));
            }
            AssistantMessageEvent::TextDelta { delta, .. } => text.push_str(&delta),
            _ => {}
        }
    }

    if text.trim().is_empty() {
        return Err(anyhow!("模型没有返回任何文本"));
    }
    Ok(Reply {
        text,
        usage,
        stop_reason,
    })
}

/// provider 的 api 是否被 rpi 支持（`catalog` 的映射表）。
pub fn supported_api(api: &str) -> Option<Api> {
    match api {
        "openai-responses" => Some(Api::OpenaiResponses),
        "openai-completions" => Some(Api::OpenaiCompletions),
        "anthropic-messages" => Some(Api::AnthropicMessages),
        _ => None,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
