//! `/api/mobile/access` 与 `/api/mobile/pair` —— 手机访问的开关、密码、扫码地址。
//!
//! 这两个 route 是**设置页 ↔ 桌面壳**的唯一契约：
//!
//! * `PUT /api/mobile/access` 只写 `desktop-access.json`，壳里的反向代理与隧道
//!   管理器每几百毫秒检查一次 mtime，变了就热加载 —— 改密码、开关公网都不需要
//!   重启任何进程。
//! * `GET /api/mobile/pair` **不返回任何凭据**，只回地址与状态标记。
//!
//! 密码会被读回给已通过同源校验的界面（用户要在手机上输它，等同路由器管理页展示
//! Wi-Fi 密码）；`GET` 的响应里没有明文 `apiKey` 那样的敏感字段。

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::mobile_access::{self, LanAccess, PairInput, PublicAccess, MIN_PASSWORD_LENGTH};
use crate::security::Trusted;
use crate::state::AppState;

#[derive(Debug, Deserialize, Default)]
pub struct PairParams {
    session: Option<String>,
    cwd: Option<String>,
}

/// 三份文件的位置：`<workmanHome>/configs/desktop-access*.json`。
fn paths() -> mobile_access::MobilePaths {
    mobile_access::MobilePaths::in_dir(&crate::paths::configs_dir())
}

/// 桌面壳会注入 `PI_WEB_DESKTOP_API_ORIGIN`（`tauri://localhost`）；
/// 命令行直启时没有它，界面据此区分「去设置里开启」与「用 dev:lan 启动」。
fn desktop_shell() -> bool {
    std::env::var("PI_WEB_DESKTOP_API_ORIGIN")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

/// 代理那份运行态里的 `ownerPid` 必须等于当前壳进程（`PI_WEB_SHELL_PID`）。
fn shell_pid() -> Option<u32> {
    std::env::var("PI_WEB_SHELL_PID")
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|pid| *pid > 0)
}

/// `GET /api/mobile/pair`
pub async fn pair(
    State(state): State<AppState>,
    _trusted: Trusted,
    Query(params): Query<PairParams>,
    headers: HeaderMap,
) -> Response {
    let paths = paths();
    let access = mobile_access::read_access(&paths.access);
    let lan_port = mobile_access::read_lan_port(&paths.runtime, shell_pid());
    let tunnel = mobile_access::read_tunnel(&paths.tunnel);

    let host_header = headers
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok());
    let port =
        mobile_access::resolve_pair_port(host_header, std::env::var("PI_WEB_PORT").ok().as_deref());

    let bind_host = state.bind_host();
    let info = mobile_access::resolve_pair(PairInput {
        bind_host: &bind_host,
        port,
        password_required: access.password.is_some()
            || std::env::var("PI_WEB_PASSWORD")
                .map(|value| !value.is_empty())
                .unwrap_or(false),
        desktop_shell: desktop_shell(),
        lan_enabled: access.enabled,
        lan_port,
        session: params.session.as_deref(),
        cwd: params.cwd.as_deref(),
        lan_address: None,
    });

    // 公网地址：quick 模式抓隧道日志里的 trycloudflare 域名，named 模式用配置里的
    // 域名推。这里刻意用 **Unicode** 形式（`π.works`）—— 给人看和扫码的地址不该带
    // `xn--` 前缀，浏览器打开时会自己转回 punycode。
    let public_base = tunnel.url.clone().or_else(|| {
        (tunnel.state == mobile_access::TunnelState::Running)
            .then(|| tunnel.hostname.clone())
            .flatten()
            .and_then(|hostname| {
                mobile_access::display_hostname(Some(&hostname))
                    .map(|display| format!("https://{display}"))
            })
    });
    let public_url = public_base.map(|base| {
        mobile_access::build_mobile_url(&base, params.session.as_deref(), params.cwd.as_deref())
    });

    let mut body = info;
    body["publicUrl"] = json!(public_url);
    body["publicState"] = json!(tunnel.state.as_str());
    Json(body).into_response()
}

/// `GET /api/mobile/access`
pub async fn access_get(State(_state): State<AppState>, _trusted: Trusted) -> Response {
    Json(current_state().await).into_response()
}

/// `PUT /api/mobile/access`
pub async fn access_put(
    State(_state): State<AppState>,
    _trusted: Trusted,
    body: axum::body::Bytes,
) -> Response {
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid JSON body" })),
        )
            .into_response();
    };
    let Some(record) = payload.as_object() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid JSON body" })),
        )
            .into_response();
    };
    let Some(enabled) = record.get("enabled").and_then(Value::as_bool) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "enabled must be a boolean" })),
        )
            .into_response();
    };

    let paths = paths();
    let current = mobile_access::read_access(&paths.access);
    let mut password = current.password.clone();

    if let Some(raw) = record.get("password").and_then(Value::as_str) {
        if !raw.trim().is_empty() {
            match validate_password(raw) {
                Some(value) => password = Some(value),
                None => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "error": format!(
                                "password must be at least {MIN_PASSWORD_LENGTH} characters"
                            ),
                            "code": "too-short",
                        })),
                    )
                        .into_response();
                }
            }
        }
    }

    if enabled && password.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "a password is required before enabling phone access",
                "code": "password-required",
            })),
        )
            .into_response();
    }

    // 公网入口要求先有密码：代理没密码就不监听，隧道只会拿到 502。
    let fallback_hostname = current
        .public
        .hostname
        .clone()
        .or_else(discover_tunnel_hostname);
    let public = match merge_public(
        current.public.clone(),
        record.get("public"),
        fallback_hostname,
    ) {
        Ok(value) => value,
        Err(code) => {
            let message = match code {
                "invalid-public" => "public access settings are malformed",
                "invalid-hostname" => "the hostname is not a valid domain",
                _ => "a hostname is required for the named tunnel mode",
            };
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": message, "code": code })),
            )
                .into_response();
        }
    };
    if public.enabled && password.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "a password is required before enabling public access",
                "code": "password-required",
            })),
        )
            .into_response();
    }

    let next = LanAccess {
        enabled,
        password,
        public,
    };
    if let Err(error) = mobile_access::write_access(&paths.access, &next) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response();
    }

    let mut body = current_state().await;
    body["saved"] = json!(true);
    Json(body).into_response()
}

/// 密码规则：非空且至少 8 位（trim 后）。
fn validate_password(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (trimmed.chars().count() >= MIN_PASSWORD_LENGTH).then(|| trimmed.to_string())
}

/// 合并 `public` 分区。`Err(code)` 对应 pi 的四个错误码。
fn merge_public(
    current: PublicAccess,
    value: Option<&Value>,
    fallback_hostname: Option<String>,
) -> Result<PublicAccess, &'static str> {
    let Some(value) = value else {
        return Ok(current);
    };
    if value.is_null() {
        return Ok(current);
    }
    let Some(record) = value.as_object() else {
        return Err("invalid-public");
    };
    let named = record.get("mode").and_then(Value::as_str) == Some("named");
    let raw_hostname = record
        .get("hostname")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    // 空串表示「没填，用自动发现的值」；填了但不合法才是 invalid-hostname。
    let hostname = if raw_hostname.is_empty() {
        fallback_hostname
    } else {
        match mobile_access::normalize_hostname(raw_hostname) {
            Some(normalized) => Some(normalized),
            None => return Err("invalid-hostname"),
        }
    };
    let enabled = record
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if named && enabled && hostname.is_none() {
        return Err("hostname-required");
    }
    Ok(PublicAccess {
        enabled,
        named,
        hostname,
    })
}

async fn current_state() -> Value {
    let paths = paths();
    let access = mobile_access::read_access(&paths.access);
    let lan_port = mobile_access::read_lan_port(&paths.runtime, shell_pid());
    let tunnel = mobile_access::read_tunnel(&paths.tunnel);
    let detected = if access.public.hostname.is_some() {
        None
    } else {
        discover_tunnel_hostname()
    };
    json!({
        "enabled": access.enabled,
        "password": access.password,
        "lanPort": lan_port,
        "public": {
            "enabled": access.public.enabled,
            "mode": if access.public.named { "named" } else { "quick" },
            "hostname": access.public.hostname,
        },
        // punycode 落盘、Unicode 展示；界面直接用这个字段回填输入框。
        "publicHostnameDisplay": mobile_access::display_hostname(access.public.hostname.as_deref()),
        "publicHostnameDetected": mobile_access::display_hostname(detected.as_deref()),
        "tunnel": tunnel.to_json(),
        "minPasswordLength": MIN_PASSWORD_LENGTH,
        // 命令行启动时没有壳的代理/隧道，开关不会生效，界面需要说明。
        "desktopShell": desktop_shell(),
    })
}

/// 自动发现「这个隧道已经绑了哪个域名」。
///
/// cloudflared 没有「列出隧道绑了哪些主机名」的命令，但登录证书本身就是一个
/// 作用域受限的 API token：`~/.cloudflared/cert.pem` 是 PEM 包着的
/// `{zoneID, accountID, apiToken}`。用它查一次 DNS 记录，就能把指向本隧道的
/// CNAME 反推出来。
///
/// **这一层在 workman 里刻意留空**：它要把用户的 Cloudflare API token 发到公网，
/// 而设置页里手填一次域名就够（`publicHostnameDetected` 因此恒为 `null`）。
/// 壳那边的隧道管理器自己会读 `cert.pem` 起 named 隧道，不受影响。
fn discover_tunnel_hostname() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_requires_eight_characters_after_trim() {
        assert_eq!(validate_password("12345678").as_deref(), Some("12345678"));
        assert_eq!(
            validate_password("  12345678  ").as_deref(),
            Some("12345678")
        );
        assert_eq!(validate_password("1234567"), None);
        assert_eq!(validate_password("        "), None);
        // 按字符数而不是字节数：8 个汉字也是 8 位
        assert_eq!(
            validate_password("一二三四五六七八").as_deref(),
            Some("一二三四五六七八")
        );
    }

    #[test]
    fn public_merge_keeps_current_when_absent() {
        let current = PublicAccess {
            enabled: true,
            named: false,
            hostname: None,
        };
        assert_eq!(merge_public(current.clone(), None, None).unwrap(), current);
        assert_eq!(
            merge_public(current.clone(), Some(&Value::Null), None).unwrap(),
            current
        );
    }

    #[test]
    fn public_merge_validates_hostname() {
        let current = PublicAccess::default();
        let bad = json!({ "enabled": true, "mode": "named", "hostname": "not a domain" });
        assert_eq!(
            merge_public(current.clone(), Some(&bad), None),
            Err("invalid-hostname")
        );

        let good = json!({ "enabled": true, "mode": "named", "hostname": "π.works" });
        let merged = merge_public(current.clone(), Some(&good), None).unwrap();
        assert_eq!(merged.hostname.as_deref(), Some("xn--1xa.works"));
        assert!(merged.named && merged.enabled);
    }

    #[test]
    fn public_merge_requires_hostname_for_named() {
        let current = PublicAccess::default();
        let named_without_host = json!({ "enabled": true, "mode": "named", "hostname": "" });
        assert_eq!(
            merge_public(current.clone(), Some(&named_without_host), None),
            Err("hostname-required")
        );
        // quick 模式不需要域名
        let quick = json!({ "enabled": true, "mode": "quick", "hostname": "" });
        assert!(merge_public(current, Some(&quick), None).unwrap().enabled);
    }

    #[test]
    fn public_merge_requires_hostname_only_without_any_fallback() {
        // 当前配置里已有域名时，`hostname: ""` 会沿用它（pi 的 fallback 语义），
        // 只有当两边都拿不到域名时才是 hostname-required。
        let existing = PublicAccess {
            enabled: true,
            named: true,
            hostname: Some("m.example.com".to_string()),
        };
        let input = json!({ "enabled": true, "mode": "named", "hostname": "" });
        // 调用方传的 fallback 就是 `current.hostname || 自动发现`（与 pi 一致）。
        let merged =
            merge_public(existing.clone(), Some(&input), existing.hostname.clone()).unwrap();
        assert_eq!(merged.hostname.as_deref(), Some("m.example.com"));
    }

    #[test]
    fn public_merge_falls_back_to_detected_hostname() {
        let current = PublicAccess::default();
        let input = json!({ "enabled": true, "mode": "named", "hostname": "" });
        let merged =
            merge_public(current, Some(&input), Some("m.example.com".to_string())).unwrap();
        assert_eq!(merged.hostname.as_deref(), Some("m.example.com"));
    }

    #[test]
    fn public_merge_rejects_non_object() {
        let current = PublicAccess::default();
        assert_eq!(
            merge_public(current, Some(&json!("nope")), None),
            Err("invalid-public")
        );
        assert_eq!(
            merge_public(PublicAccess::default(), Some(&json!([1, 2])), None),
            Err("invalid-public")
        );
    }
}
