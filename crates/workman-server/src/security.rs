//! 请求可信性校验 —— 1:1 复刻 pi-desktop 的 `lib/request-security.ts`。
//!
//! 注意：pi-desktop **没有** Basic Auth。它靠「Host 白名单 + Origin 同源」挡
//! DNS rebinding 与 CSRF。这里的语义见 `docs/contracts/README.md`。

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};

/// 可信性配置（来自环境变量，与 pi 一致）。
#[derive(Debug, Clone, Default)]
pub struct TrustPolicy {
    /// `PI_WEB_HOSTNAME` + `PI_WEB_ALLOWED_HOSTS`（逗号分隔）。
    pub allowed_hosts: Vec<String>,
    /// `PI_WEB_DESKTOP_API_ORIGIN`
    pub desktop_api_origin: Option<String>,
}

impl TrustPolicy {
    pub fn from_env() -> Self {
        let mut allowed: Vec<String> = Vec::new();
        if let Ok(v) = std::env::var("PI_WEB_HOSTNAME") {
            allowed.push(v);
        }
        if let Ok(v) = std::env::var("PI_WEB_ALLOWED_HOSTS") {
            allowed.extend(v.split(',').map(str::to_string));
        }
        Self {
            allowed_hosts: allowed
                .into_iter()
                .filter(|s| !s.trim().is_empty())
                .collect(),
            desktop_api_origin: std::env::var("PI_WEB_DESKTOP_API_ORIGIN")
                .ok()
                .filter(|s| !s.trim().is_empty()),
        }
    }
}

/// `[::1]` → `::1`，去掉结尾的 `.`，小写。
fn normalize_host(host: &str) -> String {
    let h = if host.starts_with('[') && host.ends_with(']') {
        &host[1..host.len() - 1]
    } else {
        host
    };
    h.to_lowercase().trim_end_matches('.').to_string()
}

/// 从 `Host` 头解析 hostname；带 userinfo / path / query / fragment 的一律不认。
fn host_from_header(value: &str) -> Option<String> {
    if value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || "/@\\".contains(c))
    {
        return None;
    }
    let url = url::Url::parse(&format!("http://{value}")).ok()?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    url.host_str().map(normalize_host)
}

fn is_loopback(host: &str) -> bool {
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    let Ok(ip) = host.parse::<std::net::IpAddr>() else {
        return false;
    };
    match ip {
        std::net::IpAddr::V4(v4) => v4.octets()[0] == 127,
        std::net::IpAddr::V6(v6) => {
            if v6.is_loopback() {
                return true;
            }
            // ::ffff:127.x.x.x
            let segs = v6.segments();
            segs[0] == 0
                && segs[1] == 0
                && segs[2] == 0
                && segs[3] == 0
                && segs[4] == 0
                && segs[5] == 0xffff
                && (segs[6] >> 8) == 127
        }
    }
}

fn is_ip_literal(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok()
}

fn is_trusted_hostname(host: &str, policy: &TrustPolicy) -> bool {
    if is_loopback(host) || is_ip_literal(host) {
        return true;
    }
    policy.allowed_hosts.iter().any(|raw| {
        let v = raw.trim();
        if v.is_empty() {
            return false;
        }
        let candidate = if is_ip_literal(&normalize_host(v)) {
            normalize_host(v)
        } else {
            host_from_header(v).unwrap_or_default()
        };
        candidate == host
    })
}

/// 浏览器直开导出接口时，不带 Origin 但带 navigate 头 —— pi 的显式例外。
fn is_navigation_export(method: &Method, uri_path: &str, headers: &HeaderMap) -> bool {
    if method != Method::GET {
        return false;
    }
    if header_str(headers, "sec-fetch-mode") != Some("navigate")
        || header_str(headers, "sec-fetch-dest") != Some("document")
        || header_str(headers, "sec-fetch-user") != Some("?1")
    {
        return false;
    }
    let mut parts = uri_path.trim_start_matches('/').split('/');
    matches!(
        (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next()
        ),
        (
            Some("api"),
            Some("sessions"),
            Some(_id),
            Some("export"),
            None,
            None
        )
    )
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn origin_of(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    Some(url.origin().ascii_serialization())
}

/// 复刻 `isTrustedApiRequest`。
pub fn is_trusted_api_request(
    method: &Method,
    uri_path: &str,
    headers: &HeaderMap,
    policy: &TrustPolicy,
) -> bool {
    let Some(hostname) = header_str(headers, "host").and_then(host_from_header) else {
        return false;
    };
    if !is_trusted_hostname(&hostname, policy) {
        return false;
    }

    if is_navigation_export(method, uri_path, headers) {
        return true;
    }

    let origin = header_str(headers, "origin");
    let fetch_site = header_str(headers, "sec-fetch-site");

    if let (Some(desktop), Some(origin)) = (&policy.desktop_api_origin, origin) {
        if desktop == origin {
            return true;
        }
    }
    if fetch_site == Some("cross-site") {
        return false;
    }
    let Some(origin) = origin else {
        // 没有 Origin：curl / 手机原生 App
        return true;
    };
    let self_origin = match header_str(headers, "host") {
        Some(host) => origin_of(&format!("http://{host}")),
        None => None,
    };
    match self_origin {
        Some(self_origin) => origin_of(origin).as_deref() == Some(self_origin.as_str()),
        None => false,
    }
}

/// `content-type` 是否为 JSON（`application/json` 或 `*+json`）。
pub fn is_json_request(headers: &HeaderMap) -> bool {
    let Some(raw) = header_str(headers, "content-type") else {
        return false;
    };
    let base = raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    base == "application/json" || (base.starts_with("application/") && base.ends_with("+json"))
}

/// Extractor：不经校验的 403 就是 pi 的 `{"error":"Untrusted API request"}`。
pub struct Trusted;

impl<S: Send + Sync> FromRequestParts<S> for Trusted {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let policy = parts
            .extensions
            .get::<TrustPolicy>()
            .cloned()
            .unwrap_or_default();
        if is_trusted_api_request(&parts.method, parts.uri.path(), &parts.headers, &policy) {
            Ok(Trusted)
        } else {
            Err(untrusted_response())
        }
    }
}

pub fn untrusted_response() -> Response {
    (
        StatusCode::FORBIDDEN,
        axum::Json(serde_json::json!({ "error": "Untrusted API request" })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn localhost_is_trusted() {
        let policy = TrustPolicy::default();
        let h = headers(&[("host", "127.0.0.1:3000")]);
        assert!(is_trusted_api_request(
            &Method::GET,
            "/api/sessions",
            &h,
            &policy
        ));
    }

    #[test]
    fn lan_ip_is_trusted_but_foreign_dns_is_not() {
        let policy = TrustPolicy::default();
        let lan = headers(&[("host", "192.168.1.10:3000")]);
        assert!(is_trusted_api_request(
            &Method::GET,
            "/api/sessions",
            &lan,
            &policy
        ));
        let dns = headers(&[("host", "evil.example.com")]);
        assert!(!is_trusted_api_request(
            &Method::GET,
            "/api/sessions",
            &dns,
            &policy
        ));
    }

    #[test]
    fn allowed_hosts_env_whitelist() {
        let policy = TrustPolicy {
            allowed_hosts: vec!["pi.local".into()],
            desktop_api_origin: None,
        };
        let h = headers(&[("host", "pi.local:3000")]);
        assert!(is_trusted_api_request(
            &Method::GET,
            "/api/sessions",
            &h,
            &policy
        ));
    }

    #[test]
    fn same_origin_passes_cross_site_fails() {
        let policy = TrustPolicy::default();
        let same = headers(&[
            ("host", "127.0.0.1:3000"),
            ("origin", "http://127.0.0.1:3000"),
        ]);
        assert!(is_trusted_api_request(
            &Method::POST,
            "/api/agent/x",
            &same,
            &policy
        ));

        let cross = headers(&[
            ("host", "127.0.0.1:3000"),
            ("origin", "http://evil.example.com"),
            ("sec-fetch-site", "cross-site"),
        ]);
        assert!(!is_trusted_api_request(
            &Method::POST,
            "/api/agent/x",
            &cross,
            &policy
        ));
    }

    #[test]
    fn export_navigation_exception() {
        let policy = TrustPolicy::default();
        let h = headers(&[
            ("host", "127.0.0.1:3000"),
            ("sec-fetch-mode", "navigate"),
            ("sec-fetch-dest", "document"),
            ("sec-fetch-user", "?1"),
        ]);
        assert!(is_trusted_api_request(
            &Method::GET,
            "/api/sessions/abc/export",
            &h,
            &policy
        ));
        // 同一组头用到别的路径就不算例外，但没有 Origin 仍然通过
        assert!(is_trusted_api_request(
            &Method::GET,
            "/api/sessions",
            &h,
            &policy
        ));
    }

    #[test]
    fn json_content_type() {
        assert!(is_json_request(&headers(&[(
            "content-type",
            "application/json; charset=utf-8"
        )])));
        assert!(is_json_request(&headers(&[(
            "content-type",
            "application/vnd.x+json"
        )])));
        assert!(!is_json_request(&headers(&[(
            "content-type",
            "text/plain"
        )])));
        assert!(!is_json_request(&HeaderMap::new()));
    }
}
