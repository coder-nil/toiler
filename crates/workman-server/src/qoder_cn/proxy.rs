//! 本机转发端点的 handler。
//!
//! rpi 的 provider 只能「按 baseUrl 发标准 OpenAI 请求」，所以 `qoder-cn` 的模型
//! `baseUrl` 指向这里：`http://127.0.0.1:<port>/qoder-cn/<token>`。请求进来后交给
//! [`super::transport`] 做 COSY 签名与信封解包，再把标准 chunk 流回给 rpi。
//!
//! # 安全
//!
//! 这条端点会带着用户的 Qoder 账号凭据向上游发请求（等于可消耗订阅额度），所以：
//!   1. 路径里的 `<token>` 是进程启动时生成的随机串，只有内嵌在 catalog 里的
//!      baseUrl 才知道 —— 外部扫描无法命中；
//!   2. 只接受 **loopback 对端**（[`is_loopback`]），手机端/局域网访问走的是主
//!      服务，但 rpi 的 provider 请求始终来自本机；
//!   3. 不做 `/api/*` 的 Host/Origin 校验：调用方是 rpi（不是浏览器），没有 Origin。

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use futures::StreamExt;

use super::transport::{chat_stream, CredentialSource};
use crate::state::AppState;

/// 判定对端是不是本机（127.0.0.0/8、::1、::ffff:127.x）。
fn is_loopback(addr: &std::net::SocketAddr) -> bool {
    match addr.ip() {
        std::net::IpAddr::V4(v4) => v4.is_loopback(),
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| mapped.is_loopback())
        }
    }
}

fn unauthorized(message: &str) -> Response {
    (StatusCode::FORBIDDEN, Json(json!({ "error": message }))).into_response()
}

/// `POST /qoder-cn/{token}/v1/chat/completions`
pub async fn chat_completions(
    State(state): State<AppState>,
    Path(token): Path<String>,
    request: axum::extract::Request,
) -> Response {
    if let Some(remote) = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0)
    {
        if !is_loopback(&remote) {
            return unauthorized("qoder-cn 端点只允许本机调用");
        }
    }
    if token != state.qoder_cn_token() {
        return unauthorized("qoder-cn 端点 token 不匹配");
    }

    let body_bytes = match axum::body::to_bytes(request.into_body(), 32 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": { "message": "请求体读取失败" } })),
            )
                .into_response()
        }
    };
    let openai_body: Value = match serde_json::from_slice(&body_bytes) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": { "message": "Qoder CN：无法解析请求体" } })),
            )
                .into_response()
        }
    };

    let credentials = CredentialSource {
        store: state.auth().clone(),
    };
    match chat_stream(&credentials, openai_body).await {
        Ok(outcome) => {
            let stream = outcome
                .stream
                .map(|item: Result<axum::body::Bytes, std::io::Error>| item);
            let mut response = Response::new(Body::from_stream(stream));
            let headers = response.headers_mut();
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            headers.insert(
                axum::http::header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache, no-transform"),
            );
            response
        }
        Err(message) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": { "message": message } })),
        )
            .into_response(),
    }
}
