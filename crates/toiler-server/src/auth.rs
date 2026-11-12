//! `auth.json` 的读写与凭据解析。
//!
//! 文件格式（pi 的 `auth-storage.js` / 模块 `69429`）：
//!
//! ```json
//! {
//!   "apisets":   { "type": "api_key", "key": "sk-…" },
//!   "qoder-cn":  { "type": "oauth", "access": "…", "refresh": "…", "expires": 1796796220354 }
//! }
//! ```
//!
//! 权限 0600，原子替换。API Key 一律**不返回明文**：`GET` 只给
//! `••••1234` 这种掩码（模块 `94627` 的规则：长度 < 12 全打码，否则
//! 保留末 4 位）。

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::paths;

/// `auth.json` 的句柄。
#[derive(Debug, Clone)]
pub struct AuthStore {
    path: PathBuf,
}

/// `DELETE` 的结果（模块 `69429.C2`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveResult {
    NotFound,
    /// 存在但类型不符（例如要删 api_key 但存的是 oauth）。
    TypeMismatch(String),
    Removed,
}

/// 一次 `getProviderAuthStatus` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthStatus {
    pub configured: bool,
    /// `stored` / `models_json_key` / `models_json_command` / `environment` / `fallback` / `runtime`
    pub source: Option<&'static str>,
    /// `source == "environment"` 时的环境变量名。
    pub label: Option<String>,
}

impl AuthStatus {
    fn no() -> Self {
        Self {
            configured: false,
            source: None,
            label: None,
        }
    }
}

impl Default for AuthStore {
    fn default() -> Self {
        Self::new(paths::config_file("auth.json"))
    }
}

impl AuthStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读整个文件；不存在 / 坏 JSON 都当空对象。
    pub fn read(&self) -> Map<String, Value> {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Map::new();
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        }
    }

    /// 某个 provider 的凭据类型（`api_key` / `oauth`）。
    pub fn credential_type(&self, provider: &str) -> Option<String> {
        self.read()
            .get(provider)
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// 明文 API Key（只给服务内部用，绝不进 HTTP 响应）。
    pub fn api_key(&self, provider: &str) -> Option<String> {
        let map = self.read();
        let entry = map.get(provider)?;
        if entry.get("type").and_then(Value::as_str) != Some("api_key") {
            return None;
        }
        entry
            .get("key")
            .and_then(Value::as_str)
            .filter(|k| !k.is_empty())
            .map(str::to_string)
    }

    /// 写入（整条替换），并保证目录存在、权限 0600。
    pub fn set(&self, provider: &str, credential: Value) -> std::io::Result<()> {
        let mut map = self.read();
        map.insert(provider.to_string(), credential);
        self.write(&map)
    }

    /// 删除；`expect_type` 不匹配时不删。
    pub fn remove(&self, provider: &str, expect_type: &str) -> std::io::Result<RemoveResult> {
        let mut map = self.read();
        let Some(entry) = map.get(provider) else {
            return Ok(RemoveResult::NotFound);
        };
        let stored = entry
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        if stored != expect_type {
            return Ok(RemoveResult::TypeMismatch(stored));
        }
        map.remove(provider);
        self.write(&map)?;
        Ok(RemoveResult::Removed)
    }

    fn write(&self, map: &Map<String, Value>) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let body = serde_json::to_string_pretty(&Value::Object(map.clone()))
            .unwrap_or_else(|_| "{}".into());
        // 先写临时文件再 rename，避免半截文件；临时文件本身就用 0600 建出来。
        let tmp = self
            .path
            .with_extension(format!("tmp-{}", std::process::id()));
        {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)?;
            set_mode_600(&tmp)?;
            file.write_all(body.as_bytes())?;
            file.flush()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        set_mode_600(&self.path)?;
        Ok(())
    }
}

#[cfg(unix)]
fn set_mode_600(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_mode_600(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// 复刻 `94627.o()`：`sk-abcdef…` → `••••1234`。
pub fn mask_key(raw: &str) -> Option<String> {
    let key = raw.trim();
    if key.is_empty() {
        return None;
    }
    let chars: Vec<char> = key.chars().collect();
    if chars.len() < 12 {
        return Some("•".repeat(chars.len()));
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    Some(format!("{}{}", "•".repeat(chars.len() - 4), tail))
}

/// 从 `models.json` 的 provider 配置里取 `apiKey` 字段（可能是字面量、
/// `$ENV` 模板或 `!command`）。
pub fn provider_api_key_config(provider: &Value) -> Option<&Value> {
    provider.get("apiKey").filter(|v| !v.is_null())
}

/// 把 `apiKey` 配置解析成明文（`$ENV` 插值；`!command` 返回 `Command`）。
pub enum ResolvedKey {
    Literal(String),
    /// 需要跑命令才知道，调用方决定要不要执行。
    Command(String),
    Missing,
}

pub fn resolve_key_config(config: &Value) -> ResolvedKey {
    let Some(raw) = config.as_str() else {
        return ResolvedKey::Missing;
    };
    if raw.trim().is_empty() {
        return ResolvedKey::Missing;
    }
    if let Some(cmd) = raw.strip_prefix('!') {
        return ResolvedKey::Command(cmd.to_string());
    }
    // `$VAR` / `${VAR}` 插值；`$$` → `$`，`$!` → `!`
    let mut out = String::new();
    let bytes: Vec<char> = raw.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if ch == '$' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                '$' => {
                    out.push('$');
                    i += 2;
                    continue;
                }
                '!' => {
                    out.push('!');
                    i += 2;
                    continue;
                }
                '{' => {
                    if let Some(end) = bytes[i + 2..].iter().position(|c| *c == '}') {
                        let name: String = bytes[i + 2..i + 2 + end].iter().collect();
                        match std::env::var(&name) {
                            Ok(value) => out.push_str(&value),
                            Err(_) => return ResolvedKey::Missing,
                        }
                        i = i + end + 3;
                        continue;
                    }
                }
                _ => {
                    let start = i + 1;
                    let mut end = start;
                    while end < bytes.len()
                        && (bytes[end].is_ascii_alphanumeric() || bytes[end] == '_')
                    {
                        end += 1;
                    }
                    if end > start {
                        let name: String = bytes[start..end].iter().collect();
                        match std::env::var(&name) {
                            Ok(value) => out.push_str(&value),
                            Err(_) => return ResolvedKey::Missing,
                        }
                        i = end;
                        continue;
                    }
                }
            }
        }
        out.push(ch);
        i += 1;
    }
    if out.is_empty() {
        ResolvedKey::Missing
    } else {
        ResolvedKey::Literal(out)
    }
}

/// `getProviderAuthStatus`：auth.json 优先（`stored`），其次 models.json 的
/// `apiKey` 配置。
pub fn provider_status(
    auth: &AuthStore,
    provider_id: &str,
    provider_config: Option<&Value>,
) -> AuthStatus {
    if let Some(kind) = auth.credential_type(provider_id) {
        if auth.api_key(provider_id).is_some() {
            return AuthStatus {
                configured: true,
                source: Some("stored"),
                label: None,
            };
        }
        // oauth 凭据：pi 也把它算作「已配置」（source: stored）
        if kind == "oauth" {
            return AuthStatus {
                configured: true,
                source: Some("stored"),
                label: None,
            };
        }
    }
    let Some(config) = provider_config else {
        return AuthStatus::no();
    };
    let Some(api_key) = provider_api_key_config(config) else {
        return AuthStatus::no();
    };
    match resolve_key_config(api_key) {
        ResolvedKey::Command(_) => AuthStatus {
            configured: true,
            source: Some("models_json_command"),
            label: None,
        },
        ResolvedKey::Literal(_) => {
            // 字面量里只要含 `$`，pi 就归为 environment；这里按同一规则分类。
            if api_key.as_str().is_some_and(|s| s.contains('$')) {
                AuthStatus {
                    configured: true,
                    source: Some("environment"),
                    label: env_names_of(api_key.as_str().unwrap_or_default()),
                }
            } else {
                AuthStatus {
                    configured: true,
                    source: Some("models_json_key"),
                    label: None,
                }
            }
        }
        ResolvedKey::Missing => {
            if api_key.as_str().is_some_and(|s| s.contains('$')) {
                AuthStatus {
                    configured: false,
                    source: Some("environment"),
                    label: env_names_of(api_key.as_str().unwrap_or_default()),
                }
            } else {
                AuthStatus::no()
            }
        }
    }
}

/// 从 `$A`/`${B}` 里抠出变量名，逗号拼接（pi 的 `label`）。
fn env_names_of(raw: &str) -> Option<String> {
    let mut names: Vec<String> = Vec::new();
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' || i + 1 >= chars.len() {
            i += 1;
            continue;
        }
        match chars[i + 1] {
            '$' | '!' => i += 2,
            '{' => {
                if let Some(end) = chars[i + 2..].iter().position(|c| *c == '}') {
                    names.push(chars[i + 2..i + 2 + end].iter().collect());
                    i = i + end + 3;
                } else {
                    i += 2;
                }
            }
            _ => {
                let start = i + 1;
                let mut end = start;
                while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_')
                {
                    end += 1;
                }
                if end > start {
                    names.push(chars[start..end].iter().collect());
                }
                i = end.max(i + 1);
            }
        }
    }
    if names.is_empty() {
        None
    } else {
        Some(names.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_like_pi() {
        assert_eq!(mask_key("short").unwrap(), "•••••");
        assert_eq!(
            mask_key("sk-1234567890abcdef").unwrap(),
            "•••••••••••••••cdef"
        );
        assert!(mask_key("   ").is_none());
    }

    #[test]
    fn stores_and_removes_api_key() {
        let dir = std::env::temp_dir().join(format!("toiler-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = AuthStore::new(dir.join("auth.json"));

        store
            .set(
                "demo",
                serde_json::json!({"type": "api_key", "key": "sk-1"}),
            )
            .unwrap();
        assert_eq!(store.api_key("demo").as_deref(), Some("sk-1"));
        assert_eq!(store.credential_type("demo").as_deref(), Some("api_key"));

        assert_eq!(
            store.remove("demo", "oauth").unwrap(),
            RemoveResult::TypeMismatch("api_key".into())
        );
        assert_eq!(
            store.remove("demo", "api_key").unwrap(),
            RemoveResult::Removed
        );
        assert_eq!(
            store.remove("demo", "api_key").unwrap(),
            RemoveResult::NotFound
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn file_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("toiler-auth-mode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = AuthStore::new(dir.join("auth.json"));
        store
            .set("demo", serde_json::json!({"type": "api_key", "key": "x"}))
            .unwrap();
        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolves_env_templates() {
        std::env::set_var("TOILER_TEST_KEY", "abc123");
        assert!(matches!(
            resolve_key_config(&serde_json::json!("$TOILER_TEST_KEY")),
            ResolvedKey::Literal(v) if v == "abc123"
        ));
        assert!(matches!(
            resolve_key_config(&serde_json::json!("x-${TOILER_TEST_KEY}-y")),
            ResolvedKey::Literal(v) if v == "x-abc123-y"
        ));
        assert!(matches!(
            resolve_key_config(&serde_json::json!("!op read key")),
            ResolvedKey::Command(c) if c == "op read key"
        ));
        assert!(matches!(
            resolve_key_config(&serde_json::json!("$TOILER_NOPE_MISSING")),
            ResolvedKey::Missing
        ));
    }

    #[test]
    fn status_prefers_stored_then_models_json() {
        let dir = std::env::temp_dir().join(format!("toiler-auth-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = AuthStore::new(dir.join("auth.json"));

        let models_provider = serde_json::json!({"apiKey": "sk-literal"});
        assert_eq!(
            provider_status(&store, "p", Some(&models_provider)).source,
            Some("models_json_key")
        );
        assert_eq!(provider_status(&store, "p", None), AuthStatus::no());

        store
            .set(
                "p",
                serde_json::json!({"type": "api_key", "key": "sk-stored"}),
            )
            .unwrap();
        assert_eq!(
            provider_status(&store, "p", Some(&models_provider)).source,
            Some("stored")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
