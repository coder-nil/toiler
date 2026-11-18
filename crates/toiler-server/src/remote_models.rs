//! 内置 API Key 供应商的远端模型目录缓存。
//!
//! `models.json` 是用户编辑的配置，不应被「刷新模型」动作整份重写；远端目录
//! 放在同目录的 `models-store.json`，这样刷新后重启仍能看到新模型，同时保留
//! 用户在 `models.json` 里写的模型与覆盖项。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// 缓存格式版本。修改模型定义映射时递增，避免复用旧形状。
pub const STORE_ETAG: &str = "builtin-models-api-v1";

/// 与 Coding 共用缓存文件，读改写必须串行，防止同时刷新不同供应商互相覆盖。
pub(crate) static STORE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// rpi 的 AnthropicProvider 固定报告 `anthropic`，自定义供应商需要保持自己的 id。
pub struct NamedProvider {
    pub id: String,
    pub inner: std::sync::Arc<dyn rpi_ai::Provider>,
}

#[async_trait::async_trait]
impl rpi_ai::Provider for NamedProvider {
    fn id(&self) -> &str {
        &self.id
    }
    fn models(&self) -> &[rpi_ai::Model] {
        self.inner.models()
    }
    async fn stream_simple(
        &self,
        model: &rpi_ai::Model,
        ctx: &rpi_ai::Context,
        opts: &rpi_ai::SimpleStreamOptions,
    ) -> rpi_ai::AssistantMessageEventStream {
        self.inner.stream_simple(model, ctx, opts).await
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredProvider {
    pub models: Vec<Value>,
    pub checked_at: i64,
    pub api: String,
    pub base_url: String,
}

pub fn store_path(models_json: &Path) -> PathBuf {
    models_json.with_file_name("models-store.json")
}

pub fn read(models_json: &Path, provider: &str) -> Option<StoredProvider> {
    let text = std::fs::read_to_string(store_path(models_json)).ok()?;
    let root: Value = serde_json::from_str(&text).ok()?;
    let entry = root.get(provider)?;
    if entry.get("etag").and_then(Value::as_str) != Some(STORE_ETAG) {
        return None;
    }
    let models = entry.get("models").and_then(Value::as_array)?.clone();
    let api = entry.get("api").and_then(Value::as_str)?.trim();
    let base_url = entry.get("baseUrl").and_then(Value::as_str)?.trim();
    if models.is_empty() || api.is_empty() || base_url.is_empty() {
        return None;
    }
    Some(StoredProvider {
        models,
        checked_at: entry
            .get("checkedAt")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        api: api.to_string(),
        base_url: base_url.to_string(),
    })
}

pub fn write(
    models_json: &Path,
    provider: &str,
    api: &str,
    base_url: &str,
    models: &[Value],
) -> std::io::Result<()> {
    write_entry(
        models_json,
        provider,
        json!({
            "models": models,
            "checkedAt": now_ms(),
            "etag": STORE_ETAG,
            "api": api,
            "baseUrl": base_url,
        }),
    )
}

/// 各供应商共用同一个读改写锁，专用订阅目录可保留自己的版本和账号标记。
pub(crate) fn write_entry(models_json: &Path, provider: &str, entry: Value) -> std::io::Result<()> {
    let _guard = STORE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = store_path(models_json);
    let mut root = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or_else(|| json!({}));
    if !root.is_object() {
        root = json!({});
    }
    let Some(obj) = root.as_object_mut() else {
        return Ok(());
    };
    obj.insert(provider.to_string(), entry);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".into());
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, body)?;
    crate::auth::set_mode_600(&tmp)?;
    std::fs::rename(&tmp, &path)?;
    crate::auth::set_mode_600(&path)?;
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_only_one_provider_and_reads_it_back() {
        let root = std::env::temp_dir().join(format!(
            "toiler-remote-models-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let models_json = root.join("models.json");
        write(
            &models_json,
            "openai",
            "openai-completions",
            "https://api.openai.com/v1",
            &[json!({"id": "gpt-new", "name": "GPT New"})],
        )
        .unwrap();
        write(
            &models_json,
            "deepseek",
            "openai-completions",
            "https://api.deepseek.com",
            &[json!({"id": "deepseek-new"})],
        )
        .unwrap();

        let cached = read(&models_json, "openai").unwrap();
        assert_eq!(cached.models[0]["id"], "gpt-new");
        assert_eq!(cached.api, "openai-completions");
        assert!(read(&models_json, "deepseek").is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ignores_unknown_cache_format() {
        let root = std::env::temp_dir().join(format!(
            "toiler-remote-models-etag-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let models_json = root.join("models.json");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            store_path(&models_json),
            r#"{"openai":{"etag":"old","models":[]}}"#,
        )
        .unwrap();
        assert!(read(&models_json, "openai").is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}
