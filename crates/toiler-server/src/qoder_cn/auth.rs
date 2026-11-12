//! Qoder CN 登录与 token 刷新（OAuth 设备码 + PKCE S256）。
//!
//! 对照 `pi-desktop/lib/qoder-cn/auth.ts`。链路与官方 qodercli / 国内版客户端一致：
//!
//!   1. 本地生成 verifier，challenge = base64url(sha256(verifier))
//!   2. 浏览器打开 `{website}/device/selectAccounts?challenge=…&challenge_method=S256
//!      &nonce=…&redirect_uri=…&client_id=…&machine_id=…`
//!   3. 轮询 `GET {openapi}/api/v1/deviceToken/poll?nonce=&verifier=&challenge_method=S256`
//!      —— 404 / 202 表示「用户还没点确认」，200 才拿到 token
//!   4. token 过期前用 `POST /api/v1/deviceToken/refresh {refresh_token}` 续期
//!
//! 设备码这条链路**不带账号凭证**，因此走纯 Bearer 的 openapi，不参与 COSY 签名。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::Rng;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::constants::{
    CLIENT_ID, DEVICE_POLL_PATH, DEVICE_REFRESH_PATH, OPENAPI, POLL_INTERVAL_MS, POLL_WINDOW_MS,
    REDIRECT_URI, USERINFO_PATH, USER_AGENT, WEBSITE,
};
use super::cosy::{bearer_headers, Identity};

/// PKCE 的字符集（RFC 7636 的 unreserved 集合）。
const PKCE_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 一次设备码登录的产出：凭证 + 身份。
#[derive(Debug, Clone)]
pub struct TokenSet {
    pub access: String,
    pub refresh: String,
    /// 毫秒时间戳。
    pub expires: i64,
    pub uid: String,
    pub nickname: String,
    pub user_type: String,
    pub organization_id: String,
    pub organization_name: String,
}

/// 设备码登录的过程：先拿授权 URL，再轮询。
pub struct DeviceLogin {
    pub auth_url: String,
    nonce: String,
    verifier: String,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .unwrap_or_default()
}

fn make_pkce() -> (String, String) {
    let mut rng = rand::thread_rng();
    let verifier: String = (0..64)
        .map(|_| PKCE_ALPHABET[rng.gen_range(0..PKCE_ALPHABET.len())] as char)
        .collect();
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());
    (verifier, challenge)
}

/// 设备授权 URL 里的 machine_id：官方客户端读自己的 `~/.qoder-cn/.auth/machine_id`，
/// 没有就临时生成。这里沿用同一规则，让「本机已装 Qoder CN」的环境看起来一致。
fn resolve_login_machine_id() -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    if let Some(home) = home {
        for candidate in [
            home.join(".qoder-cn/.auth/machine_id"),
            home.join(".qoder/.auth/machine_id"),
        ] {
            if let Ok(value) = std::fs::read_to_string(&candidate) {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    Uuid::new_v4().to_string()
}

fn string_field(payload: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = payload.get(*key).and_then(Value::as_str) {
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    String::new()
}

/// `expires_in`（上游按毫秒给）/ `expires_at` / 兜底 30 天。
fn device_expiry(payload: &Value) -> i64 {
    if let Some(expires_in) = payload.get("expires_in").and_then(Value::as_i64) {
        if expires_in > 0 {
            return now_ms() + expires_in;
        }
    }
    if let Some(expires_at) = payload.get("expires_at").and_then(Value::as_str) {
        if let Ok(parsed) = chrono_like_parse(expires_at) {
            return parsed;
        }
    }
    now_ms() + 30 * 24 * 3600 * 1000
}

/// 只认 RFC 3339（`2026-01-01T00:00:00Z`）这种形态，解析失败就交给兜底。
fn chrono_like_parse(text: &str) -> Result<i64, ()> {
    // `time`/`chrono` 都不在依赖里，这里只处理最常见的 `YYYY-MM-DDTHH:MM:SS(.sss)Z`。
    let trimmed = text.trim();
    let core = trimmed.trim_end_matches('Z');
    let (date, time) = core.split_once('T').ok_or(())?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next().ok_or(())?.parse().map_err(|_| ())?;
    let month: i64 = date_parts.next().ok_or(())?.parse().map_err(|_| ())?;
    let day: i64 = date_parts.next().ok_or(())?.parse().map_err(|_| ())?;
    let time = time.split('.').next().unwrap_or(time);
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next().ok_or(())?.parse().map_err(|_| ())?;
    let minute: i64 = time_parts.next().ok_or(())?.parse().map_err(|_| ())?;
    let second: i64 = time_parts.next().ok_or(())?.parse().map_err(|_| ())?;
    // days since epoch（Howard Hinnant 的 civil_from_days 反算）
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Ok((days * 86_400 + hour * 3600 + minute * 60 + second) * 1000)
}

/// 拉取账号身份（uid / 昵称 / 用户类型 / 组织）。失败时返回空字段，不阻塞登录。
pub async fn fetch_identity(access_token: &str) -> (String, String, String, String, String) {
    let url = format!("{OPENAPI}{USERINFO_PATH}");
    let response = http()
        .get(url)
        .headers(headers_to_map(&bearer_headers(Some(access_token))))
        .send()
        .await;
    let Ok(response) = response else {
        return Default::default();
    };
    if !response.status().is_success() {
        return Default::default();
    }
    let Ok(payload) = response.json::<Value>().await else {
        return Default::default();
    };
    if !payload.is_object() {
        return Default::default();
    }
    (
        string_field(&payload, &["id"]),
        string_field(&payload, &["name"]),
        string_field(&payload, &["user_type"]),
        string_field(&payload, &["organization_id", "organizationId"]),
        string_field(&payload, &["organization_name", "organizationName"]),
    )
}

fn headers_to_map(pairs: &[(String, String)]) -> reqwest::header::HeaderMap {
    let mut map = reqwest::header::HeaderMap::new();
    for (key, value) in pairs {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(key.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            map.insert(name, value);
        }
    }
    map
}

/// 发起一次设备码登录，返回浏览器授权地址与轮询器。
pub fn start_device_login() -> DeviceLogin {
    let (verifier, challenge) = make_pkce();
    let nonce = Uuid::new_v4().to_string();
    let machine_id = resolve_login_machine_id();
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("challenge", &challenge)
        .append_pair("challenge_method", "S256")
        .append_pair("nonce", &nonce)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("client_id", CLIENT_ID)
        .append_pair("machine_id", &machine_id)
        .finish();
    DeviceLogin {
        auth_url: format!("{WEBSITE}/device/selectAccounts?{query}"),
        nonce,
        verifier,
    }
}

/// 设备码轮询地址。
///
/// **`?` 必须自己写**：`Serializer::new` 只是把键值对拼到前缀字符串后面，不会
/// 补查询分隔符。少了它上游收到的是 `/deviceToken/pollnonce=…` 这种垃圾路径，
/// 回的是 `401 {"errorMessage":"User not authenticated"}` —— 与「用户还没在浏览器
/// 里确认」的回答一模一样，于是轮询把 5 分钟窗口耗光、登录永远不成功。
fn poll_url(nonce: &str, verifier: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("nonce", nonce)
        .append_pair("verifier", verifier)
        .append_pair("challenge_method", "S256")
        .finish();
    format!("{OPENAPI}{DEVICE_POLL_PATH}?{query}")
}

impl DeviceLogin {
    /// 轮询直到用户完成授权（404/202 = 还在等），最长 [`POLL_WINDOW_MS`]。
    pub async fn poll(&self) -> Result<TokenSet, String> {
        let deadline = SystemTime::now() + Duration::from_millis(POLL_WINDOW_MS);
        let poll_url = poll_url(&self.nonce, &self.verifier);
        let client = http();
        loop {
            if SystemTime::now() > deadline {
                return Err("Qoder CN 登录超时：请在浏览器完成授权后重试".to_string());
            }
            match client
                .get(&poll_url)
                .headers(headers_to_map(&bearer_headers(None)))
                .send()
                .await
            {
                Ok(response) => {
                    let status = response.status().as_u16();
                    // 401 既可能是「还没确认」也可能是地址拼错了，留下状态码是排查
                    // 这类问题的唯一线索（原因见 [`poll_url`]）。
                    tracing::debug!(status, "qoder-cn 设备码轮询");
                    if status == 404 || status == 202 {
                        // 用户还没在浏览器里点确认（pi 的判定）
                    } else if status == 200 {
                        let payload = response.json::<Value>().await.unwrap_or(Value::Null);
                        let access = string_field(&payload, &["token", "device_token"]);
                        if access.is_empty() {
                            // 200 但还没 token：上游偶发，按仍待授权处理（pi 同）。
                        } else {
                            let (uid, nickname, user_type, organization_id, organization_name) =
                                fetch_identity(&access).await;
                            return Ok(TokenSet {
                                access,
                                refresh: string_field(&payload, &["refresh_token"]),
                                expires: device_expiry(&payload),
                                uid,
                                nickname,
                                user_type: if user_type.is_empty() {
                                    super::constants::DEFAULT_USER_TYPE.to_string()
                                } else {
                                    user_type
                                },
                                organization_id,
                                organization_name,
                            });
                        }
                    } else {
                        let text = response.text().await.unwrap_or_default();
                        // 实测：设备码还没被浏览器确认时，上游回的是
                        // `401 {"errorCode":"Unauthorized","errorMessage":"User not authenticated"}`，
                        // 而不是 pi 注释里写的 404/202 —— 按「等待中」处理，否则用户
                        // 还没打开链接就先看到一条失败。
                        let awaiting = text.contains("User not authenticated")
                            || text.contains("authorization_pending");
                        if !awaiting {
                            return Err(format!(
                                "Qoder CN 登录失败：HTTP {status} {}",
                                text.chars().take(200).collect::<String>()
                            ));
                        }
                    }
                }
                Err(error) => {
                    return Err(format!("Qoder CN 登录轮询失败：{error}"));
                }
            }
            tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
        }
    }
}

/// 用 refresh_token 换新的 access token（上游字段同设备码响应）。
pub async fn refresh_token(refresh: &str) -> Result<(String, String, i64), String> {
    if refresh.is_empty() {
        return Err("Qoder CN 凭证缺少 refresh_token，请重新登录".to_string());
    }
    let response = http()
        .post(format!("{OPENAPI}{DEVICE_REFRESH_PATH}"))
        .headers(headers_to_map(&bearer_headers(None)))
        .header("content-type", "application/json")
        .header("user-agent", USER_AGENT)
        .body(serde_json::json!({ "refresh_token": refresh }).to_string())
        .send()
        .await
        .map_err(|error| format!("Qoder CN token 刷新失败：{error}"))?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        // TOKEN_EXPIRE / 12153：上游已吊销离线会话，刷新没有意义，只能重新登录。
        let dead = ["TOKEN_EXPIRE", "12153", "Offline user session not found"]
            .iter()
            .any(|needle| text.contains(needle));
        return Err(if dead {
            "Qoder CN 会话已失效（TOKEN_EXPIRE），请重新登录".to_string()
        } else {
            format!(
                "Qoder CN token 刷新失败：HTTP {status} {}",
                text.chars().take(200).collect::<String>()
            )
        });
    }
    let payload = response
        .json::<Value>()
        .await
        .map_err(|_| "Qoder CN token 刷新返回了非 JSON 响应".to_string())?;
    let access = string_field(&payload, &["token", "device_token"]);
    if access.is_empty() {
        return Err("Qoder CN token 刷新未返回 token".to_string());
    }
    let next_refresh = string_field(&payload, &["refresh_token"]);
    Ok((
        access,
        if next_refresh.is_empty() {
            refresh.to_string()
        } else {
            next_refresh
        },
        device_expiry(&payload),
    ))
}

/// 从 `auth.json` 的一条 oauth 凭据里取出签名用的身份。
pub fn identity_from_credential(credential: &Value) -> Option<Identity> {
    if credential.get("type").and_then(Value::as_str) != Some("oauth") {
        return None;
    }
    let access_token = credential
        .get("access")
        .and_then(Value::as_str)?
        .to_string();
    if access_token.is_empty() {
        return None;
    }
    Some(Identity {
        uid: string_field(credential, &["uid"]),
        nickname: string_field(credential, &["nickname"]),
        access_token,
        refresh_token: string_field(credential, &["refresh"]),
        user_type: string_field(credential, &["userType", "user_type"]),
        organization_id: string_field(credential, &["organizationId", "organization_id"]),
        organization_name: string_field(credential, &["organizationName", "organization_name"]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc7636_shape() {
        let (verifier, challenge) = make_pkce();
        assert_eq!(verifier.len(), 64);
        assert!(verifier.bytes().all(|byte| PKCE_ALPHABET.contains(&byte)));
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        assert_eq!(challenge, URL_SAFE_NO_PAD.encode(hasher.finalize()));
    }

    #[test]
    fn auth_url_carries_every_required_parameter() {
        let login = start_device_login();
        for needle in [
            "https://qoder.com.cn/device/selectAccounts?",
            "challenge=",
            "challenge_method=S256",
            "nonce=",
            "redirect_uri=qoder-work-cn",
            &format!("client_id={CLIENT_ID}"),
            "machine_id=",
        ] {
            assert!(
                login.auth_url.contains(needle),
                "缺少 {needle}：{}",
                login.auth_url
            );
        }
    }

    #[test]
    fn poll_url_keeps_the_query_separator() {
        let url = poll_url("n-1", "v-1");
        assert_eq!(
            url,
            format!("{OPENAPI}{DEVICE_POLL_PATH}?nonce=n-1&verifier=v-1&challenge_method=S256")
        );
        assert!(
            !url.contains("pollnonce"),
            "缺 `?` 会把 nonce 拼进路径：{url}"
        );
    }

    #[test]
    fn expiry_reads_millisecond_expires_in() {
        let payload = serde_json::json!({ "expires_in": 3_600_000 });
        let expires = device_expiry(&payload);
        let delta = expires - super::now_ms();
        assert!((3_500_000..=3_600_000).contains(&delta), "delta={delta}");
        // 缺字段时兜底 30 天
        let fallback = device_expiry(&serde_json::json!({}));
        let delta = fallback - super::now_ms();
        assert!((29 * 24 * 3600 * 1000..=30 * 24 * 3600 * 1000).contains(&delta));
    }

    #[test]
    fn identity_requires_an_oauth_credential() {
        assert!(identity_from_credential(&serde_json::json!({"type": "api_key"})).is_none());
        let identity = identity_from_credential(&serde_json::json!({
            "type": "oauth",
            "access": "tok",
            "refresh": "r",
            "uid": "u-1",
            "nickname": "nick",
            "userType": "personal",
        }))
        .unwrap();
        assert_eq!(identity.uid, "u-1");
        assert_eq!(identity.user_type, "personal");
        assert_eq!(identity.access_token, "tok");
    }
}
