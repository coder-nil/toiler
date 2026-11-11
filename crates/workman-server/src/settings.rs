//! `settings.json` 的读写 + `enabledModels` 的可见模型计算。
//!
//! 只碰阶段 3 需要的字段：
//!
//! - `enabledModels`：模型选择器里可见的模型范围。条目是
//!   `provider/model`、`provider/*`、`provider/model:high` 这类模式
//!   （模块 `13888`）。空 / 缺省 = 全部可见（`unscoped`）。
//! - `defaultProvider` / `defaultModel`：默认模型。
//!
//! 匹配规则（`model-resolver.js:resolveModelScopeFromModels`）：
//! 带 `/` 的模式匹配 `provider/id` 全串；不带 `/` 的模式只匹配模型 id；
//! `*` `?` `[abc]` 是通配符；`:` 后面是 `off|minimal|low|medium|high|xhigh|max`
//! 时算思考等级后缀。

use std::path::PathBuf;

use serde_json::Value;

use crate::paths;

pub const SETTINGS_SCHEMA: &str = "https://pi.dev/schemas/settings.schema.json";
const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// 一条 `enabledModels` 模式的解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPattern {
    pub raw: String,
    pub provider: Option<String>,
    pub model: String,
    pub thinking: Option<String>,
}

impl ModelPattern {
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let (base, thinking) = match trimmed.rfind(':') {
            Some(idx) if idx > 0 && THINKING_LEVELS.contains(&&trimmed[idx + 1..]) => {
                (&trimmed[..idx], Some(trimmed[idx + 1..].to_string()))
            }
            _ => (trimmed, None),
        };
        match base.split_once('/') {
            Some((provider, model)) if !provider.is_empty() && !model.is_empty() => Some(Self {
                raw: trimmed.to_string(),
                provider: Some(provider.to_string()),
                model: model.to_string(),
                thinking,
            }),
            Some(_) => None,
            None => Some(Self {
                raw: trimmed.to_string(),
                provider: None,
                model: base.to_string(),
                thinking,
            }),
        }
    }
}

/// `settings.json` 的句柄。
#[derive(Debug, Clone)]
pub struct SettingsFile {
    path: PathBuf,
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self::at(paths::config_file("settings.json"))
    }
}

impl SettingsFile {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn read(&self) -> Value {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return serde_json::json!({});
        };
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({}))
    }

    fn write(&self, value: &Value) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let body = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into());
        std::fs::write(&self.path, format!("{body}\n"))
    }

    /// `enabledModels`；缺省 / 空数组都返回 `None`（= 全部可见）。
    pub fn enabled_models(&self) -> Option<Vec<String>> {
        let raw = self.read();
        let list = raw.get("enabledModels")?.as_array()?;
        let items: Vec<String> = list
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if items.is_empty() {
            None
        } else {
            Some(items)
        }
    }

    /// 写 `enabledModels`；`None` = 删掉该键。
    pub fn set_enabled_models(&self, patterns: Option<Vec<String>>) -> std::io::Result<()> {
        let mut root = self.read();
        let Some(obj) = root.as_object_mut() else {
            return Ok(());
        };
        match patterns {
            Some(list) if !list.is_empty() => {
                obj.insert(
                    "enabledModels".into(),
                    Value::Array(list.into_iter().map(Value::from).collect()),
                );
            }
            _ => {
                obj.remove("enabledModels");
            }
        }
        self.write(&Value::Object(obj.clone()))
    }

    /// `(defaultProvider, defaultModel)`。
    pub fn default_model(&self) -> (Option<String>, Option<String>) {
        let raw = self.read();
        (
            raw.get("defaultProvider")
                .and_then(Value::as_str)
                .map(str::to_string),
            raw.get("defaultModel")
                .and_then(Value::as_str)
                .map(str::to_string),
        )
    }
}

/// 一个待匹配的模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

/// 通配符匹配（`*` `?` `[abc]` / `[!abc]`）。
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_inner(&p, &t)
}

fn glob_inner(p: &[char], t: &[char]) -> bool {
    if p.is_empty() {
        return t.is_empty();
    }
    match p[0] {
        '*' => {
            // 合并连续 *
            let rest = &p[1..];
            if rest.is_empty() {
                return true;
            }
            for i in 0..=t.len() {
                if glob_inner(rest, &t[i..]) {
                    return true;
                }
            }
            false
        }
        '?' => !t.is_empty() && glob_inner(&p[1..], &t[1..]),
        '[' => {
            if t.is_empty() {
                return false;
            }
            let Some(end) = p.iter().position(|c| *c == ']') else {
                return !t.is_empty() && p[0] == t[0] && glob_inner(&p[1..], &t[1..]);
            };
            let mut idx = 1;
            let negated = p.get(1).is_some_and(|c| *c == '!' || *c == '^');
            if negated {
                idx = 2;
            }
            let mut matched = false;
            let mut i = idx;
            while i < end {
                if i + 2 < end && p[i + 1] == '-' {
                    let (lo, hi) = (p[i], p[i + 2]);
                    if t[0] >= lo && t[0] <= hi {
                        matched = true;
                    }
                    i += 3;
                } else {
                    if p[i] == t[0] {
                        matched = true;
                    }
                    i += 1;
                }
            }
            if matched == negated {
                return false;
            }
            glob_inner(&p[end + 1..], &t[1..])
        }
        c => !t.is_empty() && c == t[0] && glob_inner(&p[1..], &t[1..]),
    }
}

/// 模式是否命中某个模型。
pub fn pattern_matches(pattern: &ModelPattern, model: &ModelRef) -> bool {
    match &pattern.provider {
        Some(provider) => {
            if !provider.eq_ignore_ascii_case(&model.provider) {
                return false;
            }
            glob_match(&pattern.model, &model.id)
        }
        // 没有 provider 前缀：只比模型 id
        None => glob_match(&pattern.model, &model.id),
    }
}

/// 当前可见的模型（保持 `models` 的顺序）；`patterns` 为 `None` 时全部可见。
pub fn visible_models(patterns: Option<&[String]>, models: &[ModelRef]) -> Vec<ModelRef> {
    let Some(patterns) = patterns else {
        return models.to_vec();
    };
    let parsed: Vec<ModelPattern> = patterns
        .iter()
        .filter_map(|p| ModelPattern::parse(p))
        .collect();
    models
        .iter()
        .filter(|m| parsed.iter().any(|p| pattern_matches(p, m)))
        .cloned()
        .collect()
}

/// 复刻 `13888.Sm()`：把某个 provider 的选择换成 `modelIds`，其它 provider 的模式原样保留。
pub fn patterns_with_provider_selection(
    visible: &[ModelRef],
    provider: &str,
    model_ids: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = visible
        .iter()
        .filter(|m| !m.provider.eq_ignore_ascii_case(provider))
        .map(|m| format!("{}/{}", m.provider, m.id))
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    for id in model_ids {
        if seen.insert(id.clone()) {
            out.push(format!("{provider}/{id}"));
        }
    }
    out
}

/// 复刻 `13888.uz()`：把「选中的 `provider/id` 集合」压回最简模式串。
///
/// 返回值语义与 pi 一致：
///
/// - `None` = 全部模型都被选中且没有思考等级 → 调用方应**删掉**
///   `enabledModels`（= 全部可见）。
/// - `Some(list)` = 要写入的模式串（可能是 `provider/*` 折叠形式）。
///
/// 折叠条件：provider 名里没有通配元字符、该 provider 的**全部**模型都被选中。
/// 任一 provider 只被部分选中时不折叠（跟 pi 一样逐条列）。
pub fn collapse_patterns(models: &[ModelRef], selected: &[String]) -> Option<Vec<String>> {
    if models.is_empty() || selected.is_empty() {
        return Some(Vec::new());
    }
    let selected_set: std::collections::BTreeSet<String> = selected.iter().cloned().collect();
    let mut by_provider: Vec<(String, Vec<ModelRef>)> = Vec::new();
    for model in models {
        match by_provider
            .iter_mut()
            .find(|(p, _)| p.eq_ignore_ascii_case(&model.provider))
        {
            Some((_, list)) => list.push(model.clone()),
            None => by_provider.push((model.provider.clone(), vec![model.clone()])),
        }
    }

    let mut all_covered = true;
    let mut out: Vec<String> = Vec::new();
    for (provider, list) in &by_provider {
        let hits: Vec<&ModelRef> = list
            .iter()
            .filter(|m| selected_set.contains(&format!("{}/{}", m.provider, m.id)))
            .collect();
        if hits.len() != list.len() {
            all_covered = false;
        }
        if hits.is_empty() {
            continue;
        }
        if hits.len() == list.len() && !has_glob_chars(provider) {
            out.push(format!("{provider}/*"));
            continue;
        }
        for model in hits {
            out.push(format!("{}/{}", model.provider, model.id));
        }
    }
    if all_covered {
        None
    } else {
        Some(out)
    }
}

fn has_glob_chars(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(
            c,
            '*' | '?' | '[' | ']' | '{' | '}' | '(' | ')' | '!' | '+' | '|' | '@' | ','
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(provider: &str, id: &str) -> ModelRef {
        ModelRef {
            provider: provider.into(),
            id: id.into(),
        }
    }

    #[test]
    fn parses_patterns_with_levels() {
        let p = ModelPattern::parse("qoder/*:high").unwrap();
        assert_eq!(p.provider.as_deref(), Some("qoder"));
        assert_eq!(p.model, "*");
        assert_eq!(p.thinking.as_deref(), Some("high"));

        let p = ModelPattern::parse("deepseek-flash").unwrap();
        assert!(p.provider.is_none());
        assert_eq!(p.model, "deepseek-flash");

        assert!(ModelPattern::parse("  ").is_none());
        assert!(ModelPattern::parse("/x").is_none());
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("gpt-*", "gpt-5.4"));
        assert!(!glob_match("gpt-*", "claude"));
        assert!(glob_match("glm-?.?", "glm-5.3"));
        assert!(glob_match("g[lm]m*", "glm-5"));
        assert!(!glob_match("g[!lm]m*", "glm-5"));
    }

    #[test]
    fn visibility_respects_patterns() {
        let models = vec![model("a", "x"), model("a", "y"), model("b", "z")];
        assert_eq!(visible_models(None, &models).len(), 3);
        let visible = visible_models(Some(&["a/*".into()]), &models);
        assert_eq!(visible.len(), 2);
        let visible = visible_models(Some(&["b/z".into(), "y".into()]), &models);
        assert_eq!(visible, vec![model("a", "y"), model("b", "z")]);
    }

    #[test]
    fn selection_replaces_one_provider() {
        let visible = vec![model("a", "x"), model("b", "z")];
        let patterns = patterns_with_provider_selection(&visible, "b", &["q".into(), "r".into()]);
        assert_eq!(patterns, vec!["a/x", "b/q", "b/r"]);
    }

    #[test]
    fn collapse_folds_fully_selected_providers() {
        let models = vec![model("a", "x"), model("a", "y"), model("b", "z")];
        // 两个 provider 全选 → None（= 删掉 enabledModels）
        assert_eq!(
            collapse_patterns(&models, &["a/x".into(), "a/y".into(), "b/z".into()]),
            None
        );

        // a 全选、b 未选 → 折叠成 a/*
        assert_eq!(
            collapse_patterns(&models, &["a/x".into(), "a/y".into()]),
            Some(vec!["a/*".into()])
        );

        // 部分选中 → 逐条列举
        assert_eq!(
            collapse_patterns(&models, &["a/x".into()]),
            Some(vec!["a/x".into()])
        );
    }
}
