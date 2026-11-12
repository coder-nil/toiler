//! `models.json` 的读写（模块 `17551`）。
//!
//! 写回时的两条归一规则，逐字复刻 pi：
//!
//! 1. **模型 id 清洗**：`models` 里 `id` 不是非空字符串的条目直接丢掉。
//! 2. **cost 补零**：某模型带 `cost`，且 `input/output/cacheRead/cacheWrite`
//!    里至少有一个是有限数字时，缺的补 `0`；一个数字都没有就把 `cost` 整个删掉。
//!
//! 另外**始终保留 `$schema`**：文件里没有就补
//! `https://pi.dev/schemas/models.schema.json` 并放在最前面。

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::paths;

pub const MODELS_SCHEMA: &str = "https://pi.dev/schemas/models.schema.json";
const COST_KEYS: [&str; 4] = ["input", "output", "cacheRead", "cacheWrite"];

/// `models.json` 的句柄。
#[derive(Debug, Clone)]
pub struct ModelsFile {
    path: PathBuf,
}

impl Default for ModelsFile {
    fn default() -> Self {
        Self::at(default_path())
    }
}

/// 默认路径：`$TOILER_MODELS` > `<toilerHome>/configs/models.json`。
pub fn default_path() -> PathBuf {
    if let Ok(p) = std::env::var("TOILER_MODELS") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    paths::config_file("models.json")
}

impl ModelsFile {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读；不存在或坏 JSON 一律给 `{"providers":{}}`（pi 的行为）。
    pub fn read(&self) -> Value {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return serde_json::json!({ "providers": {} });
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(value) => value,
            Err(_) => serde_json::json!({ "providers": {} }),
        }
    }

    /// 归一后写回（见模块文档），返回落盘的内容。
    pub fn write(&self, value: &Value) -> std::io::Result<Value> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let normalized = normalize_for_write(value);
        let body = serde_json::to_string_pretty(&normalized).unwrap_or_else(|_| "{}".into());
        std::fs::write(&self.path, format!("{body}\n"))?;
        Ok(normalized)
    }

    /// 删掉一个 provider。
    pub fn delete_provider(&self, provider: &str) -> std::io::Result<Value> {
        let mut root = self.read();
        if let Some(map) = root.get_mut("providers").and_then(Value::as_object_mut) {
            map.remove(provider);
        }
        self.write(&root)
    }

    /// 删掉某个 provider 下的一个模型（顺带清 `modelOverrides` 里的同名键）。
    pub fn delete_model(&self, provider: &str, model_id: &str) -> std::io::Result<Value> {
        let mut root = self.read();
        let Some(providers) = root.get_mut("providers").and_then(Value::as_object_mut) else {
            return self.write(&root);
        };
        let Some(entry) = providers.get_mut(provider).and_then(Value::as_object_mut) else {
            return self.write(&root);
        };
        if let Some(models) = entry.get_mut("models").and_then(Value::as_array_mut) {
            models.retain(|m| match m.get("id").and_then(Value::as_str) {
                Some(id) => id != model_id,
                None => true,
            });
        }
        if let Some(overrides) = entry
            .get_mut("modelOverrides")
            .and_then(Value::as_object_mut)
        {
            overrides.remove(model_id);
            if overrides.is_empty() {
                entry.remove("modelOverrides");
            }
        }
        self.write(&root)
    }
}

fn is_object(value: &Value) -> bool {
    value.is_object()
}

/// 写盘前的归一（模块 `17551.n` 里的两段 `function`）。
pub fn normalize_for_write(value: &Value) -> Value {
    let mut root = value.clone();
    let Some(providers) = root.get_mut("providers").and_then(Value::as_object_mut) else {
        return ensure_schema(value);
    };
    for provider in providers.values_mut() {
        if !is_object(provider) {
            continue;
        }
        let Some(models) = provider.get_mut("models").and_then(Value::as_array_mut) else {
            continue;
        };
        // 1. 丢掉没有有效 id 的模型
        models.retain(|m| match m.get("id").and_then(Value::as_str) {
            Some(id) => !id.trim().is_empty(),
            None => false,
        });
        // 2. cost 补零
        for model in models.iter_mut() {
            if !is_object(model) {
                continue;
            }
            let Some(cost) = model.get("cost") else {
                continue;
            };
            match normalize_cost(cost) {
                Some(fixed) => {
                    if let Some(obj) = model.as_object_mut() {
                        obj.insert("cost".into(), fixed);
                    }
                }
                None => {
                    if let Some(obj) = model.as_object_mut() {
                        obj.remove("cost");
                    }
                }
            }
        }
    }
    ensure_schema(&root)
}

fn normalize_cost(cost: &Value) -> Option<Value> {
    let obj = cost.as_object()?;
    let keys: Vec<&str> = COST_KEYS
        .iter()
        .copied()
        .filter(|k| obj.contains_key(*k))
        .collect();
    if keys.is_empty() {
        return None;
    }
    // 只要某个键不是有限数字，整块 cost 删掉（pi 的 allOf 判定）
    for key in &keys {
        match obj.get(*key).and_then(Value::as_f64) {
            Some(v) if v.is_finite() => {}
            _ => return None,
        }
    }
    let mut out = Map::new();
    for (k, v) in obj {
        out.insert(k.clone(), v.clone());
    }
    for key in COST_KEYS {
        out.entry(key.to_string()).or_insert_with(|| Value::from(0));
    }
    Some(Value::Object(out))
}

fn ensure_schema(value: &Value) -> Value {
    let mut root = match value.clone() {
        Value::Object(map) => map,
        other => {
            let mut map = Map::new();
            map.insert("providers".into(), other);
            map
        }
    };
    if !root.contains_key("$schema") {
        let mut with_schema = Map::new();
        with_schema.insert("$schema".into(), Value::from(MODELS_SCHEMA));
        for (k, v) in root {
            with_schema.insert(k, v);
        }
        root = with_schema;
    }
    Value::Object(root)
}

/// 取 provider 配置对象。
pub fn provider_config<'a>(root: &'a Value, provider: &str) -> Option<&'a Value> {
    root.get("providers")?.get(provider)
}

/// provider 的显示名（`name` 字段，缺省用 id）。
pub fn provider_display_name(root: &Value, provider: &str) -> String {
    provider_config(root, provider)
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| provider.to_string())
}

/// provider 下的模型 id 列表。
pub fn provider_model_ids(root: &Value, provider: &str) -> Vec<String> {
    provider_config(root, provider)
        .and_then(|p| p.get("models"))
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|m| m.get("id").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// 遍历所有 provider：`(id, config)`。
pub fn providers(root: &Value) -> Vec<(String, Value)> {
    root.get("providers")
        .and_then(Value::as_object)
        .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_adds_schema_and_normalizes_cost() {
        let dir = std::env::temp_dir().join(format!("toiler-models-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = ModelsFile::at(dir.join("models.json"));
        let written = file
            .write(&serde_json::json!({
                "providers": {
                    "p": {
                        "api": "openai-completions",
                        "models": [
                            {"id": "a", "cost": {"input": 1, "output": 2}},
                            {"id": "   "},
                            {"name": "no id"},
                            {"id": "b", "cost": {"note": "x"}}
                        ]
                    }
                }
            }))
            .unwrap();

        assert_eq!(written["$schema"], MODELS_SCHEMA);
        let models = written["providers"]["p"]["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["cost"]["cacheRead"], 0);
        assert_eq!(models[0]["cost"]["cacheWrite"], 0);
        assert!(models[1].get("cost").is_none());

        // 已有 $schema 不覆盖
        let kept = file
            .write(&serde_json::json!({"$schema": "custom", "providers": {}}))
            .unwrap();
        assert_eq!(kept["$schema"], "custom");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deletes_provider_and_model() {
        let dir = std::env::temp_dir().join(format!("toiler-models-del-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = ModelsFile::at(dir.join("models.json"));
        file.write(&serde_json::json!({
            "providers": {
                "p": {
                    "models": [{"id": "a"}, {"id": "b"}],
                    "modelOverrides": {"a": {"x": 1}, "b": {"y": 2}}
                },
                "q": {"models": [{"id": "z"}]}
            }
        }))
        .unwrap();

        let after = file.delete_model("p", "a").unwrap();
        assert_eq!(
            after["providers"]["p"]["models"].as_array().unwrap().len(),
            1
        );
        assert!(after["providers"]["p"]["modelOverrides"].get("a").is_none());

        let after = file.delete_provider("q").unwrap();
        assert!(after["providers"].get("q").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
