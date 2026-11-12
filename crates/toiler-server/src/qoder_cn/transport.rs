//! Qoder CN 的传输层：把标准 OpenAI 请求改写成 COSY 签名请求，再把上游的 SSE
//! 信封拆回标准 OpenAI chunk 流。
//!
//! 对照 `pi-desktop/lib/qoder-cn/transport.ts`。上游信封里的 `body` 字段本身就是
//! 标准 OpenAI chunk（`{"headers":{…},"body":"<chunk>","statusCodeValue":200}`），
//! 所以只要把信封拆掉，rpi 自带的 OpenAI 适配器就能原样消费 —— 工具调用、usage、
//! reasoning 的解析全部复用，避免两份协议实现漂移。
//!
//! pi 是给 SDK 换掉 `fetch` 实现的；rpi 的 provider 不允许注入传输层，所以
//! toiler 把模型的 `baseUrl` 指向**自己的 HTTP 端点**
//! （`http://127.0.0.1:<port>/qoder-cn/<token>`），由 [`super::proxy`] 调用本模块。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use bytes::Bytes;
use futures::StreamExt;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::auth::{identity_from_credential, refresh_token};
use super::constants::{catalog_entry, CatalogEntry, CHAT_PATH, GATEWAY, PROVIDER_ID};
use super::cosy::{HeaderOptions, Identity, Session};
use super::request_body::{build_request_body, Conversation};

const GATEWAY_TIMEOUT_SECS: u64 = 10 * 60;

/// 上游给 delta 补的空字段，透传会让客户端渲染出空文本气泡。
const NOISE_DELTA_KEYS: [&str; 3] = ["extra_fields", "refusal", "reasoning_content"];

/// 对话 id 弱缓存：同一段对话复用同一组 id / begin_at（用量归组用）。
fn conversations() -> &'static Mutex<HashMap<String, Conversation>> {
    static SLOT: OnceLock<Mutex<HashMap<String, Conversation>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(HashMap::new()))
}

fn conversation_key(body: &Value) -> Option<String> {
    let messages = body.get("messages")?.as_array()?;
    let system_text = messages
        .iter()
        .find(|message| {
            matches!(
                message.get("role").and_then(Value::as_str),
                Some("system") | Some("developer")
            )
        })
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let first_user = messages
        .iter()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))?;
    let user_text = match first_user.get("content") {
        Some(Value::String(text)) => text.clone(),
        other => other.cloned().unwrap_or(Value::Null).to_string(),
    };
    let mut hasher = Sha256::new();
    hasher.update(format!("{system_text}\u{0}{user_text}").as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    Some(digest.chars().take(32).collect())
}

fn conversation_for(body: &Value) -> Option<Conversation> {
    let key = conversation_key(body)?;
    let mut cache = conversations().lock().ok()?;
    if let Some(existing) = cache.get(&key) {
        return Some(existing.clone());
    }
    let created = Conversation {
        id: uuid::Uuid::new_v4().to_string(),
        begin_at: super::now_ms(),
    };
    cache.insert(key, created.clone());
    // 只保留最近的条目：这是用量归组用的弱缓存，不需要无限长。
    if cache.len() > 200 {
        if let Some(oldest) = cache.keys().next().cloned() {
            cache.remove(&oldest);
        }
    }
    Some(created)
}

/// 把上游多套缓存字段归一到 OpenAI 标准位，避免客户端恒读 0。
fn normalize_usage(usage: &mut Map<String, Value>) {
    let completion_details = usage
        .get("completion_tokens_details")
        .and_then(Value::as_object)
        .cloned();
    let prompt_details = usage
        .get("prompt_tokens_details")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let candidates = [
        usage.get("cached_tokens").cloned(),
        usage.get("prompt_cache_hit_tokens").cloned(),
        completion_details.and_then(|details| details.get("cached_tokens").cloned()),
        prompt_details.get("cached_tokens").cloned(),
    ];
    // 保留原始数值形态（整数别变成 42.0）：上游给的是整数 token 数。
    let cached = candidates
        .iter()
        .flatten()
        .find(|value| value.as_f64().is_some_and(|number| number > 0.0))
        .cloned();
    let Some(cached) = cached else {
        return;
    };
    let mut target = prompt_details;
    if target.get("cached_tokens") == Some(&cached) {
        return;
    }
    target.insert("cached_tokens".into(), cached);
    usage.insert("prompt_tokens_details".into(), Value::Object(target));
}

/// 清洗一个内层 chunk：去掉空噪声字段，并补标准 usage 位。
pub fn clean_chunk(raw: &str) -> String {
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return raw.to_string();
    };
    let Value::Object(mut chunk) = parsed else {
        return raw.to_string();
    };
    let mut changed = false;

    if let Some(Value::Object(usage)) = chunk.get_mut("usage") {
        let before = usage.clone();
        normalize_usage(usage);
        if *usage != before {
            changed = true;
        }
    }

    if let Some(Value::Array(choices)) = chunk.get_mut("choices") {
        for choice in choices.iter_mut() {
            let Some(delta) = choice.get_mut("delta").and_then(Value::as_object_mut) else {
                continue;
            };
            if delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .is_some_and(|calls| calls.is_empty())
            {
                delta.remove("tool_calls");
                changed = true;
            }
            for key in NOISE_DELTA_KEYS {
                let empty = match delta.get(key) {
                    Some(Value::Null) => true,
                    Some(Value::String(text)) => text.is_empty(),
                    Some(Value::Array(items)) => items.is_empty(),
                    _ => false,
                };
                if empty {
                    delta.remove(key);
                    changed = true;
                }
            }
            let empty_delta = delta.is_empty();
            let no_finish = choice
                .get("finish_reason")
                .map(Value::is_null)
                .unwrap_or(true);
            if empty_delta && no_finish {
                return String::new();
            }
        }
    }

    if changed {
        Value::Object(chunk).to_string()
    } else {
        raw.to_string()
    }
}

#[derive(Debug)]
pub struct Envelope {
    pub status: u16,
    pub body: String,
}

fn strip_data_prefix(line: &str) -> String {
    let mut value = line.trim();
    if value.is_empty() || value.starts_with(':') {
        return String::new();
    }
    while let Some(rest) = value.strip_prefix("data:") {
        value = rest.trim();
    }
    if value.starts_with(':') {
        return String::new();
    }
    value.to_string()
}

/// 解析一帧 SSE 信封；非信封帧（`event: finish` 之类的计时元数据）返回 `None`。
pub fn parse_envelope_frame(frame: &str) -> Option<Envelope> {
    let payload = strip_data_prefix(frame);
    if payload.is_empty() {
        return None;
    }
    let parsed: Value = serde_json::from_str(&payload).ok()?;
    if !parsed.is_object() {
        return None;
    }
    let status = parsed
        .get("statusCodeValue")
        .and_then(Value::as_f64)
        .map(|value| value as u16)
        .unwrap_or(200);
    Some(Envelope {
        status,
        body: parsed
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

/// 从缓冲区里取下一帧（`\r\n\r\n` / `\n\n` / `\r\r` 分隔）。
fn take_frame(pending: &mut String) -> Option<String> {
    let candidate = ["\r\n\r\n", "\n\n", "\r\r"]
        .iter()
        .filter_map(|boundary| pending.find(boundary).map(|index| (index, boundary.len())))
        .min_by_key(|(index, _)| *index);
    let (index, length) = candidate?;
    let frame = pending[..index].to_string();
    pending.drain(..index + length);
    Some(frame)
}

/// 一次对话请求的产出：交给代理 handler 的 chunk 流。
pub struct ChatOutcome {
    pub stream: futures::stream::BoxStream<'static, Result<Bytes, std::io::Error>>,
}

/// 读 `auth.json` 里当前凭据，过期就先刷新（并把新凭据写回）。
pub struct CredentialSource {
    pub store: crate::auth::AuthStore,
}

impl CredentialSource {
    fn read(&self) -> Option<Value> {
        self.store.read().get(PROVIDER_ID).cloned()
    }

    /// 取可用身份：`expires` 在 60 秒内到期就先刷新。
    pub async fn identity(&self) -> Result<Identity, String> {
        let credential = self
            .read()
            .ok_or_else(|| "Qoder CN 未登录：请到「模型 → 订阅」完成登录".to_string())?;
        let identity = identity_from_credential(&credential)
            .ok_or_else(|| "Qoder CN 凭据不是 OAuth 类型，请重新登录".to_string())?;
        let expires = credential
            .get("expires")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if expires > 0 && expires - super::now_ms() < 60_000 {
            return self.renew(&credential, identity).await;
        }
        Ok(identity)
    }

    /// 强制刷新一次（上游 401/403 时用）。
    pub async fn force_renew(&self) -> Result<Identity, String> {
        let credential = self
            .read()
            .ok_or_else(|| "Qoder CN 未登录：请到「模型 → 订阅」完成登录".to_string())?;
        let identity = identity_from_credential(&credential)
            .ok_or_else(|| "Qoder CN 凭据不是 OAuth 类型，请重新登录".to_string())?;
        self.renew(&credential, identity).await
    }

    async fn renew(&self, credential: &Value, mut identity: Identity) -> Result<Identity, String> {
        let (access, refresh, expires) = refresh_token(&identity.refresh_token).await?;
        identity.access_token = access.clone();
        identity.refresh_token = refresh.clone();
        let mut next = credential.clone();
        if let Value::Object(map) = &mut next {
            map.insert("access".into(), json!(access));
            map.insert("refresh".into(), json!(refresh));
            map.insert("expires".into(), json!(expires));
        }
        self.store
            .set(PROVIDER_ID, next)
            .map_err(|error| format!("写回 Qoder CN 凭据失败：{error}"))?;
        Ok(identity)
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(GATEWAY_TIMEOUT_SECS))
        .build()
        .unwrap_or_default()
}

/// 发一次上游请求，返回响应（未消费 body）。
async fn send(
    identity: &Identity,
    model_key: &str,
    catalog: &CatalogEntry,
    openai_body: &Value,
) -> Result<reqwest::Response, String> {
    let session = Session::new(identity)?;
    let conversation = conversation_for(openai_body);
    let body = build_request_body(
        openai_body,
        model_key,
        catalog,
        Some(identity.user_type.as_str()),
        conversation.as_ref(),
    );
    let raw_url = format!("{GATEWAY}{CHAT_PATH}");
    let headers = session.headers(
        &body,
        &raw_url,
        &HeaderOptions {
            model_key: Some(model_key.to_string()),
            sse: true,
            accept: None,
        },
    );
    let mut request = http_client().post(&raw_url).body(body);
    for (key, value) in headers {
        request = request.header(&key, &value);
    }
    request
        .send()
        .await
        .map_err(|error| format!("Qoder CN 上游请求失败：{error}"))
}

async fn read_upstream_error(response: reqwest::Response) -> String {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if text.is_empty() {
        return format!("Qoder CN 上游返回 HTTP {status}");
    }
    let detail = parse_envelope_frame(&text)
        .map(|envelope| envelope.body)
        .filter(|body| !body.is_empty())
        .unwrap_or(text);
    format!(
        "Qoder CN 上游返回 HTTP {status}：{}",
        detail.chars().take(600).collect::<String>()
    )
}

/// 走完一次对话：签名、发请求、解信封、清洗 chunk。
///
/// 与 pi 的 `createQoderCnFetch` 等价：第一帧的 `statusCodeValue != 200` 直接当
/// 错误返回（否则错误会伪装成一条空流）。
pub async fn chat_stream(
    credentials: &CredentialSource,
    openai_body: Value,
) -> Result<ChatOutcome, String> {
    let model_id = openai_body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| "Qoder CN：请求体缺少 model".to_string())?
        .to_string();
    let catalog =
        catalog_entry(&model_id).ok_or_else(|| format!("Qoder CN：未知模型 {model_id}"))?;

    let mut identity = credentials.identity().await?;
    let mut response = send(&identity, &model_id, catalog, &openai_body).await?;

    // 凭证可能刚被上游吊销：强制刷新后再试一次（只重试一次，避免打爆登录链路）。
    if matches!(response.status().as_u16(), 401 | 403) {
        let failed = read_upstream_error(response).await;
        if ["TOKEN_INVALID", "TOKEN_EXPIRE", "401"]
            .iter()
            .any(|needle| failed.contains(needle))
        {
            identity = credentials.force_renew().await?;
            response = send(&identity, &model_id, catalog, &openai_body).await?;
        } else {
            return Err(failed);
        }
    }

    if !response.status().is_success() {
        return Err(read_upstream_error(response).await);
    }

    let mut upstream = response.bytes_stream();
    let mut pending = String::new();
    let mut first_frame: Option<Envelope> = None;

    // 先读到第一帧：信封里的 statusCodeValue 决定这次请求算成功还是失败。
    while first_frame.is_none() {
        if let Some(frame) = take_frame(&mut pending) {
            if let Some(envelope) = parse_envelope_frame(&frame) {
                first_frame = Some(envelope);
            }
            continue;
        }
        match upstream.next().await {
            Some(Ok(chunk)) => pending.push_str(&String::from_utf8_lossy(&chunk)),
            _ => break,
        }
    }

    if let Some(envelope) = &first_frame {
        if envelope.status != 200 {
            return Err(format!(
                "Qoder CN 上游返回 statusCodeValue={}：{}",
                envelope.status,
                envelope.body.chars().take(600).collect::<String>()
            ));
        }
    }

    let stream =
        futures::stream::unfold(Some((upstream, pending, first_frame)), |state| async move {
            let (mut upstream, mut pending, mut first) = state?;
            loop {
                if let Some(envelope) = first.take() {
                    match emit_envelope(envelope) {
                        Emitted::Done => return Some((Ok(done_frame()), None)),
                        Emitted::Chunk(bytes) => {
                            return Some((Ok(bytes), Some((upstream, pending, None))))
                        }
                        Emitted::Skip => {}
                    }
                }
                if let Some(frame) = take_frame(&mut pending) {
                    if let Some(envelope) = parse_envelope_frame(&frame) {
                        match emit_envelope(envelope) {
                            Emitted::Done => return Some((Ok(done_frame()), None)),
                            Emitted::Chunk(bytes) => {
                                return Some((Ok(bytes), Some((upstream, pending, None))))
                            }
                            Emitted::Skip => continue,
                        }
                    }
                    continue;
                }
                match upstream.next().await {
                    Some(Ok(chunk)) => pending.push_str(&String::from_utf8_lossy(&chunk)),
                    _ => {
                        // 上游没发 [DONE] 就直接断流：补一个，避免客户端一直等结束。
                        let tail = parse_envelope_frame(&pending);
                        pending.clear();
                        if let Some(envelope) = tail {
                            if let Emitted::Chunk(bytes) = emit_envelope(envelope) {
                                // 先把这帧吐出去，下一轮再补 [DONE]。
                                return Some((
                                    Ok(bytes),
                                    Some((
                                        upstream,
                                        pending,
                                        Some(Envelope {
                                            status: 0,
                                            body: "[DONE]".into(),
                                        }),
                                    )),
                                ));
                            }
                        }
                        return Some((Ok(done_frame()), None));
                    }
                }
            }
        });

    Ok(ChatOutcome {
        stream: Box::pin(stream),
    })
}

enum Emitted {
    Done,
    Chunk(Bytes),
    Skip,
}

fn done_frame() -> Bytes {
    Bytes::from_static(b"data: [DONE]\n\n")
}

fn emit_envelope(envelope: Envelope) -> Emitted {
    if envelope.body == "[DONE]" {
        return Emitted::Done;
    }
    let cleaned = clean_chunk(&envelope.body);
    if cleaned.is_empty() {
        return Emitted::Skip;
    }
    Emitted::Chunk(Bytes::from(format!("data: {cleaned}\n\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_split_on_any_blank_line_variant() {
        let mut pending = "data: {\"a\":1}\n\ndata: {\"a\":2}\n\n".to_string();
        assert_eq!(take_frame(&mut pending).as_deref(), Some("data: {\"a\":1}"));
        assert_eq!(take_frame(&mut pending).as_deref(), Some("data: {\"a\":2}"));
        assert_eq!(take_frame(&mut pending), None);

        let mut crlf = "data: x\r\n\r\nrest".to_string();
        assert_eq!(take_frame(&mut crlf).as_deref(), Some("data: x"));
        assert_eq!(crlf, "rest");
    }

    #[test]
    fn envelope_parsing_reads_status_and_body() {
        let frame = "data: {\"statusCodeValue\":200,\"body\":\"{\\\"choices\\\":[]}\"}";
        let envelope = parse_envelope_frame(frame).expect("信封");
        assert_eq!(envelope.status, 200);
        assert_eq!(envelope.body, "{\"choices\":[]}");

        // 计时元数据帧不是信封
        assert!(parse_envelope_frame("event: finish").is_none());
        assert!(parse_envelope_frame(": keep-alive").is_none());
    }

    #[test]
    fn clean_chunk_drops_noise_and_normalizes_usage() {
        let raw = r#"{"choices":[{"delta":{"content":"hi","extra_fields":null,"tool_calls":[]},"finish_reason":null}],"usage":{"prompt_cache_hit_tokens":42}}"#;
        let cleaned: Value = serde_json::from_str(&clean_chunk(raw)).unwrap();
        assert!(cleaned["choices"][0]["delta"].get("extra_fields").is_none());
        assert!(cleaned["choices"][0]["delta"].get("tool_calls").is_none());
        assert_eq!(
            cleaned["usage"]["prompt_tokens_details"]["cached_tokens"],
            42
        );
    }

    #[test]
    fn empty_delta_without_finish_reason_is_dropped() {
        assert_eq!(clean_chunk(r#"{"choices":[{"delta":{}}]}"#), "");
        // 有 finish_reason 就要保留
        let kept = clean_chunk(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#);
        assert!(kept.contains("stop"));
    }

    #[test]
    fn conversation_ids_are_stable_per_prompt() {
        let body = json!({"messages": [
            {"role": "system", "content": "sys"},
            {"role": "user", "content": "hi"},
        ]});
        let first = conversation_for(&body).unwrap();
        let second = conversation_for(&body).unwrap();
        assert_eq!(first.id, second.id);
        // 换一段对话就是新 id
        let other =
            conversation_for(&json!({"messages": [{"role": "user", "content": "other"}]})).unwrap();
        assert_ne!(first.id, other.id);
    }
}
