//! Responses 推理摘要适配：非 off 档位请求 `reasoning.summary = "auto"`。
//!
//! rpi 0.25.0 只发送 effort，既没有 summary 选项，也没有请求体注入钩子。
//! 与 qoder-cn 一样，用本进程的回环端点补参数，仍由原 provider 负责消息转换、
//! SSE 解析、计费和重试。不能修改 crates.io 的 rpi，也不能复制其协议实现。
//!
//! 只装在活会话：off、非推理模型及其他协议直接走原 provider。目录与持久化
//! 模型地址始终保留上游地址，仅单次请求的模型副本指向本机。
//! 每次请求用独立随机能力令牌，目标只能由进程内注册，完成或取消后立即撤销；
//! 不接受外部指定 URL，不记录令牌、请求正文或凭据。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use rpi_ai::event_stream::create_assistant_message_event_stream_with_cancellation;
use rpi_ai::provider::DeferredProvider;
use rpi_ai::{
    Api, AssistantMessageEventStream, Context, Model, Provider, SimpleStreamOptions, ThinkingLevel,
};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::state::AppState;

#[derive(Clone)]
struct Target {
    url: String,
    signal: CancellationToken,
}

pub struct SummaryRelay {
    targets: Mutex<HashMap<String, Target>>,
    http: reqwest::Client,
}

impl Default for SummaryRelay {
    fn default() -> Self {
        Self {
            targets: Mutex::new(HashMap::new()),
            // 转发请求携带上游凭据，不允许重定向把它们带到未注册的目标。
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("构造摘要转发 HTTP 客户端"),
        }
    }
}

struct Registration {
    relay: Arc<SummaryRelay>,
    token: String,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(target) = self.relay.targets.lock().unwrap().remove(&self.token) {
            target.signal.cancel();
        }
    }
}

impl SummaryRelay {
    fn register(self: &Arc<Self>, base: &str, signal: CancellationToken) -> Registration {
        let base = base.trim_end_matches('/');
        // 与 rpi 的 Responses 端点拼接一致，不能给已有 /v1 的地址重复补 /v1。
        let url = if base.ends_with("/v1") {
            format!("{base}/responses")
        } else {
            format!("{base}/v1/responses")
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.targets
            .lock()
            .unwrap()
            .insert(token.clone(), Target { url, signal });
        Registration {
            relay: Arc::clone(self),
            token,
        }
    }

    /// provider 列表保留 id 与模型目录，切模型和内置 Coding 的按协议分发均不变。
    pub fn wrap_all(
        self: &Arc<Self>,
        providers: &[Arc<dyn Provider>],
        port: u16,
    ) -> Vec<Arc<dyn Provider>> {
        providers
            .iter()
            .map(|inner| {
                Arc::new(SummaryProvider {
                    inner: Arc::clone(inner),
                    relay: Arc::clone(self),
                    port,
                }) as Arc<dyn Provider>
            })
            .collect()
    }
}

struct SummaryProvider {
    inner: Arc<dyn Provider>,
    relay: Arc<SummaryRelay>,
    port: u16,
}

fn wants_summary(model: &Model, opts: &SimpleStreamOptions) -> bool {
    model.api == Api::OpenaiResponses
        && model.reasoning
        && opts.reasoning_level() != ThinkingLevel::Off
}

#[async_trait::async_trait]
impl Provider for SummaryProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn models(&self) -> &[Model] {
        self.inner.models()
    }
    fn deferred(&self) -> Option<&dyn DeferredProvider> {
        self.inner.deferred()
    }

    async fn stream_simple(
        &self,
        model: &Model,
        ctx: &Context,
        opts: &SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        if !wants_summary(model, opts) {
            return self.inner.stream_simple(model, ctx, opts).await;
        }
        let (mut producer, stream) =
            create_assistant_message_event_stream_with_cancellation(&opts.signal);
        if self.port == 0 {
            // 不静默退回无摘要请求；真实服务必须先 bind，测试也能明确发现漏接。
            let error = rpi_ai::types::AssistantMessage::terminal(
                model.api.clone(),
                model.provider.clone(),
                model.id.clone(),
                rpi_ai::types::StopReason::Error,
                "推理摘要转发端点尚未绑定",
                0,
            );
            producer
                .push(rpi_ai::types::AssistantMessageEvent::Error {
                    reason: rpi_ai::types::ErrorReason::Error,
                    error,
                })
                .await;
            return stream;
        }
        let mut options = opts.clone();
        options.signal = producer.cancellation_token();
        let signal = options.signal.clone();
        let registration = self.relay.register(&model.base_url, signal.clone());
        let mut local = model.clone();
        local.base_url = format!(
            "http://127.0.0.1:{}/reasoning-summary/{}/v1",
            self.port, registration.token
        );
        let mut inner = self.inner.stream_simple(&local, ctx, &options).await;
        tokio::spawn(async move {
            let _registration = registration;
            loop {
                let event = tokio::select! {
                    biased;
                    _ = signal.cancelled() => break,
                    event = inner.next() => event,
                };
                let Some(event) = event else { break };
                let terminal = event.is_terminal();
                if !producer.push(event).await || terminal {
                    break;
                }
            }
        });
        stream
    }
}

/// 只补 summary；不提高 effort、不改输入/工具/缓存/签名，也不删已有 reasoning 字段。
fn add_summary(body: &mut Value) -> Result<(), &'static str> {
    let body = body.as_object_mut().ok_or("请求体必须为 JSON 对象")?;
    let reasoning = body.entry("reasoning").or_insert_with(|| json!({}));
    let reasoning = reasoning
        .as_object_mut()
        .ok_or("reasoning 必须为 JSON 对象")?;
    reasoning.insert("summary".into(), json!("auto"));
    Ok(())
}

/// 逐跳字段不能带到下一跳，尤其不能透传改写前的 Content-Length 或原 Host。
fn forwarded_headers(mut headers: HeaderMap) -> HeaderMap {
    let connection_headers: Vec<String> = headers
        .get_all("connection")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(',').map(|s| s.trim().to_string()))
        .collect();
    for name in connection_headers {
        headers.remove(name);
    }
    for name in [
        "host",
        "content-length",
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
    headers
}

fn relay_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": {"message": message}}))).into_response()
}

async fn prepare_request(
    http: &reqwest::Client,
    target: &Target,
    request: Request,
) -> Result<reqwest::Request, Response> {
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, 32 * 1024 * 1024)
        .await
        .map_err(|_| {
            relay_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "摘要请求体读取失败或超过 32 MiB",
            )
        })?;
    let mut body: Value = serde_json::from_slice(&bytes)
        .map_err(|_| relay_error(StatusCode::BAD_REQUEST, "摘要请求体不是合法 JSON"))?;
    add_summary(&mut body).map_err(|e| relay_error(StatusCode::BAD_REQUEST, e))?;
    http.post(&target.url)
        .headers(forwarded_headers(parts.headers))
        .json(&body)
        .build()
        .map_err(|_| relay_error(StatusCode::BAD_GATEWAY, "无法构造推理摘要请求"))
}

fn streaming_response(upstream: reqwest::Response, signal: CancellationToken) -> Response {
    let status = upstream.status();
    // 不能把 Location 交回 rpi，让外层客户端带着凭据绕过注册表继续跟随。
    if status.is_redirection() {
        return relay_error(
            StatusCode::BAD_GATEWAY,
            "摘要端点不接受上游重定向，请配置最终 API 地址",
        );
    }
    let headers = forwarded_headers(upstream.headers().clone());
    let mut incoming = upstream.bytes_stream();
    let stream = async_stream::stream! {
        loop {
            let next = tokio::select! {
                biased;
                _ = signal.cancelled() => break,
                next = incoming.next() => next,
            };
            let Some(next) = next else { break };
            yield next;
        }
    };
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

/// 内部端点：只有 loopback 对端与存活请求的能力令牌同时满足才转发。
pub async fn responses(
    State(state): State<AppState>,
    Path(token): Path<String>,
    request: Request,
) -> Response {
    let local = request.extensions().get::<ConnectInfo<SocketAddr>>().is_some_and(|peer| {
        peer.ip().is_loopback() || matches!(peer.ip(), std::net::IpAddr::V6(ip) if ip.to_ipv4_mapped().is_some_and(|ip| ip.is_loopback()))
    });
    if !local {
        return relay_error(StatusCode::FORBIDDEN, "摘要端点只允许本机调用");
    }
    let relay = &state.inner.summary_relay;
    let target = relay.targets.lock().unwrap().get(&token).cloned();
    let Some(target) = target else {
        return relay_error(StatusCode::FORBIDDEN, "摘要请求令牌无效或已过期");
    };
    let forward = async {
        let request = prepare_request(&relay.http, &target, request).await?;
        relay
            .http
            .execute(request)
            .await
            .map(|upstream| streaming_response(upstream, target.signal.clone()))
            // reqwest 错误可能包含令牌或 URL 查询凭据，不把它直接发给 UI 或日志。
            .map_err(|_| relay_error(StatusCode::BAD_GATEWAY, "推理摘要上游连接失败"))
    };
    tokio::select! {
        biased;
        _ = target.signal.cancelled() => relay_error(StatusCode::REQUEST_TIMEOUT, "推理摘要请求已取消"),
        result = forward => result.unwrap_or_else(|error| error),
    }
}

#[cfg(test)]
mod tests;
