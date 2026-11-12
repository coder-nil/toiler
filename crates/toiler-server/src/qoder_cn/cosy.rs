//! COSY 请求签名（Qoder 数据面的鉴权方式）。
//!
//! 对照 `pi-desktop/lib/qoder-cn/cosy.ts`。上游不认单纯的 Bearer token：每个请求
//! 都要带一套 COSY 头，其中
//!
//! ```text
//! authorization = "Bearer COSY.<payload>.<sig>"
//! payload       = base64(sorted-compact JSON{cosyVersion, ideVersion, info, requestId, version})
//! info          = base64(AES-128-CBC(identity JSON, key=iv=tempKey))
//! cosy-key      = base64(RSA-PKCS1v15(tempKey))
//! sig           = md5(payload \n cosy-key \n date \n body \n path)
//! path          = URL path，去掉 "/algo" 前缀
//! ```
//!
//! 也就是说签名覆盖**请求体的最终字节**：body 任何变化都必须重新签，不能先签名再改
//! body（这也是请求体必须先 [`super::base64::encode`] 再签的原因）。
//!
//! 会话对象按 access token 存活：token 轮换后 info 里的 `security_oauth_token` 必须
//! 换新，因此整份会话要重建 —— 调用方每次请求都新建一次。

use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use md5::{Digest, Md5};
use rand::rngs::OsRng;
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::constants::{
    CLIENT_IP, COSY_CLIENT_TYPE, COSY_USER_AGENT, COSY_VERSION, DEFAULT_USER_TYPE,
    SERVER_PUBLIC_KEY, USER_AGENT,
};
use super::fingerprint::{
    derive_device_id, derive_machine_hostname, derive_machine_token, derive_machine_type,
    MACHINE_OS,
};

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

/// 一次请求要带的身份（来自 `auth.json` 的 oauth 凭据）。
#[derive(Debug, Clone, Default)]
pub struct Identity {
    pub uid: String,
    pub nickname: String,
    pub access_token: String,
    pub refresh_token: String,
    pub user_type: String,
    pub organization_id: String,
    pub organization_name: String,
}

/// 键排序 + 无空白的紧凑 JSON。
///
/// 服务端解出的 identity 字节要与客户端签的完全一致，所以这里不能用
/// `serde_json::to_string`（键序取决于结构/哈希），必须显式排序；`null` 与
/// `undefined` 一律当空串（pi 的 `sortedCompactJson` 语义）。
pub fn sorted_compact_json(entries: &[(&str, &str)]) -> String {
    let mut sorted: Vec<&(&str, &str)> = entries.iter().collect();
    sorted.sort_by(|left, right| left.0.cmp(right.0));
    let parts: Vec<String> = sorted
        .iter()
        .map(|(key, value)| {
            format!(
                "{}:{}",
                Value::String((*key).to_string()),
                Value::String((*value).to_string())
            )
        })
        .collect();
    format!("{{{}}}", parts.join(","))
}

/// map 版本：`payload` 里有 4 个静态字符串 + 1 个随机 requestId，值都是字符串。
fn sorted_compact_map(entries: &Map<String, Value>) -> String {
    let mut keys: Vec<&String> = entries.keys().collect();
    keys.sort();
    let parts: Vec<String> = keys
        .into_iter()
        .map(|key| {
            let value = match entries.get(key) {
                Some(Value::String(text)) => Value::String(text.clone()),
                _ => Value::String(String::new()),
            };
            format!("{}:{}", Value::String(key.clone()), value)
        })
        .collect();
    format!("{{{}}}", parts.join(","))
}

/// 从 URL 取签名用的 path（去掉 `/algo` 前缀，与官方一致）。
pub fn signature_path(raw_url: &str) -> String {
    let path = url::Url::parse(raw_url)
        .ok()
        .map(|url| url.path().to_string())
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| raw_url.split('?').next().unwrap_or("/").to_string());
    match path.strip_prefix("/algo") {
        Some(rest) => rest.to_string(),
        None => path,
    }
}

/// 会话：一次请求对应一份（token 换了就要换会话）。
pub struct Session {
    uid: String,
    /// `access_token` 也在 headers 里被当作 `security_oauth_token` 使用，这里只留
    /// 一套 identity 用于签 info。
    info: String,
    cosy_key: String,
    machine_id: String,
    machine_token: String,
    machine_type: String,
    machine_hostname: String,
}

/// 请求头的可选差异（模型清单是 `sse: false` 的 GET，对话是 SSE 的 POST）。
pub struct HeaderOptions {
    pub model_key: Option<String>,
    pub sse: bool,
    pub accept: Option<String>,
}

impl Default for HeaderOptions {
    fn default() -> Self {
        Self {
            model_key: None,
            sse: true,
            accept: None,
        }
    }
}

impl Session {
    /// 建会话：随机 16 字符 tempKey → RSA 包裹 → AES-128-CBC 包身份 JSON。
    pub fn new(identity: &Identity) -> Result<Self, String> {
        if identity.access_token.is_empty() {
            return Err("qoder cosy: empty access token".to_string());
        }
        let uid = identity.uid.clone();
        // 官方取 uuid4().hex[:16]，即 16 个十六进制字符当 ASCII 用；AES 的 key 与 iv
        // 是同一串字节。长度与字符集都不要「加固」—— 服务端解出的字节必须逐字节相同。
        let temp_key: String = Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(16)
            .collect();
        let key_bytes = temp_key.as_bytes();

        let public_key = RsaPublicKey::from_public_key_pem(SERVER_PUBLIC_KEY)
            .map_err(|error| format!("qoder cosy: 解析服务端公钥失败：{error}"))?;
        let cosy_key = public_key
            .encrypt(&mut OsRng, Pkcs1v15Encrypt, key_bytes)
            .map_err(|error| format!("qoder cosy: 包裹会话密钥失败：{error}"))?;
        let cosy_key = STANDARD.encode(cosy_key);

        let user_type = if identity.user_type.is_empty() {
            DEFAULT_USER_TYPE.to_string()
        } else {
            identity.user_type.clone()
        };
        let plain_identity = sorted_compact_json(&[
            ("name", identity.nickname.as_str()),
            ("aid", uid.as_str()),
            ("uid", uid.as_str()),
            ("yx_uid", ""),
            ("organization_id", identity.organization_id.as_str()),
            ("organization_name", identity.organization_name.as_str()),
            ("user_type", user_type.as_str()),
            ("security_oauth_token", identity.access_token.as_str()),
            ("refresh_token", identity.refresh_token.as_str()),
        ]);
        // AES-128-CBC + PKCS7：Node 的 createCipheriv 默认就是这套填充。
        let mut buffer = plain_identity.clone().into_bytes();
        let message_len = buffer.len();
        buffer.resize(message_len + 16, 0);
        let cipher = Aes128CbcEnc::new(key_bytes.into(), key_bytes.into());
        let encrypted = cipher
            .encrypt_padded_mut::<Pkcs7>(&mut buffer, message_len)
            .map_err(|error| format!("qoder cosy: 加密身份失败：{error}"))?;
        let info = STANDARD.encode(encrypted);

        Ok(Self {
            machine_id: derive_device_id(&uid, "machine"),
            machine_token: derive_machine_token(&uid),
            machine_type: derive_machine_type(&uid),
            machine_hostname: derive_machine_hostname(&uid),
            uid,
            info,
            cosy_key,
        })
    }

    /// 对一次请求体签名并返回完整请求头。
    pub fn headers(
        &self,
        body: &str,
        raw_url: &str,
        options: &HeaderOptions,
    ) -> Vec<(String, String)> {
        let mut payload = Map::new();
        payload.insert("cosyVersion".into(), Value::String(COSY_VERSION.into()));
        payload.insert("ideVersion".into(), Value::String(String::new()));
        payload.insert("info".into(), Value::String(self.info.clone()));
        payload.insert(
            "requestId".into(),
            Value::String(Uuid::new_v4().to_string()),
        );
        payload.insert("version".into(), Value::String("v1".into()));
        let payload_b64 = STANDARD.encode(sorted_compact_map(&payload));

        let date = now_seconds().to_string();
        let raw = [
            payload_b64.as_str(),
            self.cosy_key.as_str(),
            date.as_str(),
            body,
            signature_path(raw_url).as_str(),
        ]
        .join("\n");
        let mut hasher = Md5::new();
        hasher.update(raw.as_bytes());
        let sig = format!("{:x}", hasher.finalize());
        let bearer = format!("Bearer COSY.{payload_b64}.{sig}");

        let accept = options.accept.clone().unwrap_or_else(|| {
            if options.sse {
                "text/event-stream".to_string()
            } else {
                "application/json".to_string()
            }
        });

        let mut headers: Vec<(String, String)> = vec![
            ("cosy-data-policy".into(), "agree".into()),
            ("content-type".into(), "application/json".into()),
            ("cosy-business-product".into(), "cli".into()),
            ("cosy-business-type".into(), "agent".into()),
            ("cosy-scene".into(), "assistant".into()),
            ("cosy-clienttype".into(), COSY_CLIENT_TYPE.into()),
            ("cosy-date".into(), date),
            ("cosy-user".into(), self.uid.clone()),
            ("cosy-key".into(), self.cosy_key.clone()),
            ("accept".into(), accept),
            ("authorization".into(), bearer),
            ("accept-encoding".into(), "identity".into()),
            ("cosy-version".into(), COSY_VERSION.into()),
            ("cosy-machineid".into(), self.machine_id.clone()),
            ("cosy-machinetoken".into(), self.machine_token.clone()),
            ("cosy-machinetype".into(), self.machine_type.clone()),
            ("cosy-clientip".into(), CLIENT_IP.into()),
            ("login-version".into(), "v2".into()),
            ("user-agent".into(), COSY_USER_AGENT.into()),
            ("cosy-machineos".into(), MACHINE_OS.into()),
        ];
        if options.sse {
            headers.push(("cosy-machinehostname".into(), self.machine_hostname.clone()));
            headers.push(("cache-control".into(), "no-cache".into()));
        }
        if let Some(model_key) = &options.model_key {
            headers.push(("x-model-key".into(), model_key.clone()));
            headers.push(("x-model-source".into(), "system".into()));
        }
        headers
    }
}

/// 普通 Bearer 请求头（设备码登录、token 刷新这些 openapi 端点不参与 COSY 签名）。
pub fn bearer_headers(access_token: Option<&str>) -> Vec<(String, String)> {
    let mut headers = vec![
        ("accept".to_string(), "application/json".to_string()),
        ("user-agent".to_string(), USER_AGENT.to_string()),
    ];
    if let Some(token) = access_token {
        headers.push(("authorization".to_string(), format!("Bearer {token}")));
    }
    headers
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorted_json_orders_keys_and_normalizes_nulls() {
        // pi: sortedCompactJson({name:"n", aid:"u", yx_uid:""}) → 键按字典序、无空格
        assert_eq!(
            sorted_compact_json(&[("name", "n"), ("aid", "u"), ("yx_uid", "")]),
            "{\"aid\":\"u\",\"name\":\"n\",\"yx_uid\":\"\"}"
        );
    }

    #[test]
    fn signature_path_strips_the_algo_prefix() {
        assert_eq!(
            signature_path("https://gateway.qoder.com.cn/algo/api/v2/model/list?Encode=1"),
            "/api/v2/model/list"
        );
        assert_eq!(
            signature_path("https://gateway.qoder.com.cn/v1/chat"),
            "/v1/chat"
        );
    }

    #[test]
    fn session_requires_a_token() {
        assert!(Session::new(&Identity::default()).is_err());
    }

    #[test]
    fn headers_cover_the_body_and_expose_model_key() {
        let identity = Identity {
            uid: "u-123".into(),
            nickname: "nick".into(),
            access_token: "token".into(),
            ..Default::default()
        };
        let session = Session::new(&identity).unwrap();
        let options = HeaderOptions {
            model_key: Some("qmodel".into()),
            ..Default::default()
        };
        let headers = session.headers("body-bytes", "https://gw/algo/api/x", &options);
        let map: std::collections::HashMap<&str, &str> = headers
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        assert_eq!(map["cosy-user"], "u-123");
        assert_eq!(map["x-model-key"], "qmodel");
        assert_eq!(map["x-model-source"], "system");
        assert!(map["authorization"].starts_with("Bearer COSY."));
        assert_eq!(map["cosy-version"], COSY_VERSION);
        // 同一份 body + 不同时间戳会得到不同签名（md5 覆盖 body）
        let other = session.headers("body-bytes-2", "https://gw/algo/api/x", &options);
        let other_auth = other
            .iter()
            .find(|(key, _)| key == "authorization")
            .map(|(_, value)| value.clone())
            .unwrap();
        assert_ne!(map["authorization"], other_auth);
    }
}
