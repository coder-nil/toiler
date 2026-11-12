//! 把 OpenAI Chat Completions 请求体翻译成 Qoder 的 agent 请求体。
//!
//! 对照 `pi-desktop/lib/qoder-cn/request-body.ts`。Qoder 的对话入口不是「给一段
//! messages 就回一段 completion」，而是一次 agent 回合：`chat_context`、
//! `model_config`、`business` 这些字段共同决定上游怎么排额度、归组用量、按哪个
//! 模型路由。
//!
//! 两条与上游行为强相关的约定：
//!   - 上游**只认** `model_config.key` / `x-model-key` 里的路由键，未知键不报错，
//!     会静默回落 `auto`（用户会莫名其妙用上另一个模型）；
//!   - 工具历史有「结构化」和「文本化」两种形态：结构化更准，但只有部分模型接受
//!     配对的 `assistant.tool_calls` / `role:tool`，其余模型必须序列化成文本标记。

use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::base64;
use super::constants::{CatalogEntry, COSY_VERSION, DEFAULT_USER_TYPE};

/// 文本化工具历史时，assistant 请求调用工具的那行标记。
const TOOL_CALL_MARKER: &str = "[assistant 请求调用工具]";
/// 文本化工具历史时，工具结果消息的前缀（名字随后拼进来）。
const TOOL_RESULT_MARKER: &str = "[工具结果";

/// 允许「结构化」工具历史的模型族（按上游路由键前缀判断）。
const STRUCTURED_KEY_PREFIXES: &[&str] = &[
    "qmodel",
    "qfmodel",
    "q37fmodel", // Qwen 族
    "gmodel",
    "gfmodel",
    "gm", // GLM 族
    "dmodel",
    "dfmodel", // DeepSeek 族
];

/// DeepSeek 族的多轮一致性依赖 `reasoning_content`，必须原样回传。
fn is_deepseek_key(model_key: &str) -> bool {
    model_key.starts_with("dmodel") || model_key.starts_with("dfmodel")
}

fn is_structured_key(model_key: &str) -> bool {
    STRUCTURED_KEY_PREFIXES
        .iter()
        .any(|prefix| model_key.starts_with(prefix))
}

struct FlattenedContent {
    text: String,
    images: Vec<String>,
}

/// 压平 content：字符串直接用；部件数组抽 text 与图片 URL；单部件对象包成数组。
fn flatten_content(content: Option<&Value>) -> FlattenedContent {
    let Some(content) = content else {
        return FlattenedContent {
            text: String::new(),
            images: Vec::new(),
        };
    };
    match content {
        Value::String(text) => FlattenedContent {
            text: text.clone(),
            images: Vec::new(),
        },
        Value::Null => FlattenedContent {
            text: String::new(),
            images: Vec::new(),
        },
        other => {
            let single;
            let parts: &[Value] = match other {
                Value::Array(items) => items,
                _ => {
                    single = [other.clone()];
                    &single
                }
            };
            let mut texts = Vec::new();
            let mut images = Vec::new();
            for part in parts {
                match part {
                    Value::String(text) => texts.push(text.clone()),
                    Value::Object(map) => {
                        let kind = map.get("type").and_then(Value::as_str).unwrap_or("");
                        if matches!(kind, "text" | "input_text" | "output_text" | "summary_text") {
                            if let Some(text) = map.get("text").and_then(Value::as_str) {
                                texts.push(text.to_string());
                            }
                            continue;
                        }
                        let has_image_key =
                            map.contains_key("image_url") || map.contains_key("image");
                        if kind == "image_url"
                            || kind == "image"
                            || kind == "input_image"
                            || has_image_key
                        {
                            let raw = map.get("image_url").or_else(|| map.get("url"));
                            let url = match raw {
                                Some(Value::String(url)) => url.clone(),
                                Some(Value::Object(inner)) => inner
                                    .get("url")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                                _ => String::new(),
                            };
                            if !url.is_empty() {
                                images.push(url);
                            }
                        }
                    }
                    _ => {}
                }
            }
            FlattenedContent {
                text: texts.join(""),
                images,
            }
        }
    }
}

/// 带图消息保留 OpenAI vision parts 形态（上游只读 content 里的 parts）。
fn vision_content(content: Option<&Value>, text: &str) -> Value {
    match content {
        Some(Value::Array(_)) => content.cloned().unwrap_or_else(|| json!(text)),
        Some(Value::String(_)) => content.cloned().unwrap_or_else(|| json!(text)),
        _ => json!(text),
    }
}

fn tool_arguments(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn reasoning_of(message: &Value) -> String {
    message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub struct FlattenOptions {
    pub structured: bool,
    pub keep_reasoning: bool,
}

pub struct FlattenResult {
    pub system_text: Option<String>,
    pub messages: Vec<Value>,
    /// 出现过图片时，顶层 `image_urls` 需要跟着填（兼容无损，仅观测用）。
    pub has_images: bool,
}

/// 压平会话：首个 system/developer 抽为系统提示词，其余按上游可接受的形态排列。
pub fn flatten_messages(messages: &[Value], options: &FlattenOptions) -> FlattenResult {
    let mut system_text: Option<String> = None;
    let mut has_images = false;
    let mut flat: Vec<Value> = Vec::new();

    for message in messages {
        if !message.is_object() {
            continue;
        }
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        let content = flatten_content(message.get("content"));
        if !content.images.is_empty() {
            has_images = true;
        }

        if role == "system" || role == "developer" {
            // 只取第一条非空系统消息作为 model 的系统提示词：pi 的会话就是这个形态。
            if system_text.is_none() && !content.text.is_empty() {
                system_text = Some(content.text.clone());
            }
            continue;
        }

        if role == "tool" {
            let name = message.get("name").and_then(Value::as_str).unwrap_or("");
            let suffix = if name.is_empty() {
                String::new()
            } else {
                format!(" ({name})")
            };
            if options.structured {
                flat.push(json!({
                    "role": "tool",
                    "tool_call_id": message.get("tool_call_id").and_then(Value::as_str).unwrap_or(""),
                    // content 用空字符串而不是 null：null 会让上游丢掉配对的 assistant 消息。
                    "content": content.text,
                }));
                continue;
            }
            flat.push(json!({
                "role": "user",
                "content": format!("{TOOL_RESULT_MARKER}{suffix}]\n{}", content.text),
            }));
            continue;
        }

        if role == "assistant"
            && message
                .get("tool_calls")
                .and_then(Value::as_array)
                .is_some_and(|calls| !calls.is_empty())
        {
            let calls = message["tool_calls"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if options.structured {
                let mapped: Vec<Value> = calls
                    .iter()
                    .map(|call| {
                        let function = call.get("function");
                        json!({
                            "id": call.get("id").and_then(Value::as_str).unwrap_or(""),
                            "type": call.get("type").and_then(Value::as_str).unwrap_or("function"),
                            "function": {
                                "name": function
                                    .and_then(|f| f.get("name"))
                                    .and_then(Value::as_str)
                                    .unwrap_or(""),
                                "arguments": tool_arguments(
                                    function.and_then(|f| f.get("arguments"))
                                ),
                            },
                        })
                    })
                    .collect();
                let mut entry = Map::new();
                entry.insert("role".into(), json!("assistant"));
                entry.insert("content".into(), json!(content.text));
                entry.insert("tool_calls".into(), Value::Array(mapped));
                if options.keep_reasoning {
                    entry.insert("reasoning_content".into(), json!(reasoning_of(message)));
                }
                flat.push(Value::Object(entry));
                continue;
            }
            let outlined: Vec<Value> = calls
                .iter()
                .map(|call| {
                    let function = call.get("function");
                    json!({
                        "name": function
                            .and_then(|f| f.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        "arguments": tool_arguments(function.and_then(|f| f.get("arguments"))),
                    })
                })
                .collect();
            let mut entry = Map::new();
            entry.insert("role".into(), json!("assistant"));
            entry.insert(
                "content".into(),
                json!(format!(
                    "{}\n\n{TOOL_CALL_MARKER}\n{}",
                    content.text,
                    Value::Array(outlined)
                )),
            );
            if options.keep_reasoning {
                entry.insert("reasoning_content".into(), json!(reasoning_of(message)));
            }
            flat.push(Value::Object(entry));
            continue;
        }

        if role == "user" || role == "assistant" {
            let mut entry = Map::new();
            entry.insert("role".into(), json!(role));
            entry.insert(
                "content".into(),
                if content.images.is_empty() {
                    json!(content.text)
                } else {
                    vision_content(message.get("content"), &content.text)
                },
            );
            if role == "assistant" && options.keep_reasoning {
                entry.insert("reasoning_content".into(), json!(reasoning_of(message)));
            }
            flat.push(Value::Object(entry));
            continue;
        }

        if !role.is_empty() {
            // 未知角色一律降级为 user，不静默丢弃内容。
            flat.push(json!({ "role": "user", "content": content.text }));
        }
    }

    FlattenResult {
        system_text,
        messages: flat,
        has_images,
    }
}

/// 上游要求 `tools[]` 是 `{type:"function", function:{…}}` 形态，平铺形态一律包一层。
pub fn normalize_tools(tools: Option<&Value>) -> Vec<Value> {
    let Some(Value::Array(items)) = tools else {
        return Vec::new();
    };
    let mut normalized = Vec::new();
    for item in items {
        let Value::Object(tool) = item else {
            continue;
        };
        let kind = tool.get("type").and_then(Value::as_str);
        if matches!(kind, Some(other) if other != "function") {
            continue;
        }
        if let (Some(name), false) = (
            tool.get("name").and_then(Value::as_str),
            tool.contains_key("function"),
        ) {
            let mut wrapped = Map::new();
            wrapped.insert("type".into(), json!("function"));
            let mut function = Map::new();
            function.insert("name".into(), json!(name));
            function.insert(
                "description".into(),
                tool.get("description").cloned().unwrap_or(json!("")),
            );
            function.insert(
                "parameters".into(),
                tool.get("parameters").cloned().unwrap_or(json!({})),
            );
            if let Some(strict) = tool.get("strict").and_then(Value::as_bool) {
                function.insert("strict".into(), json!(strict));
            }
            wrapped.insert("function".into(), Value::Object(function));
            normalized.push(Value::Object(wrapped));
            continue;
        }
        normalized.push(item.clone());
    }
    normalized
}

fn pick_max_tokens(body: &Value) -> Option<i64> {
    for key in ["max_completion_tokens", "max_tokens"] {
        if let Some(value) = body.get(key).and_then(Value::as_f64) {
            if value > 0.0 {
                return Some(value as i64);
            }
        }
    }
    None
}

fn pick_effort(body: &Value, catalog: &CatalogEntry) -> Option<String> {
    let raw = body.get("reasoning_effort").and_then(Value::as_str)?;
    if raw.is_empty() || catalog.efforts.is_empty() {
        return None;
    }
    let effort = raw.to_lowercase();
    catalog
        .efforts
        .iter()
        .find(|candidate| **candidate == effort)
        .map(|candidate| (*candidate).to_string())
}

fn last_user_prompt(messages: &[Value]) -> String {
    for message in messages.iter().rev() {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        match message.get("content") {
            Some(Value::String(text)) if !text.is_empty() => return text.clone(),
            Some(Value::Array(parts)) => {
                let joined: Vec<&str> = parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect();
                let text = joined.join("\n");
                if !text.is_empty() {
                    return text;
                }
            }
            _ => {}
        }
    }
    String::new()
}

/// 同一段对话复用同一个 `business.id` / `begin_at`：否则用量面板会把一轮拆成碎片。
#[derive(Debug, Clone)]
pub struct Conversation {
    pub id: String,
    pub begin_at: i64,
}

/// 构造并编码 Qoder 请求体。
///
/// 返回的字符串就是签名与发送用的最终字节：编码之后不能再改，否则 COSY 签名
/// （md5 覆盖 body）会与 body 不一致。
pub fn build_request_body(
    openai_body: &Value,
    model_key: &str,
    catalog: &CatalogEntry,
    user_type: Option<&str>,
    conversation: Option<&Conversation>,
) -> String {
    let empty = Vec::new();
    let raw_messages = match openai_body.get("messages") {
        Some(Value::Array(items)) => items,
        _ => &empty,
    };
    let flattened = flatten_messages(
        raw_messages,
        &FlattenOptions {
            structured: is_structured_key(model_key),
            keep_reasoning: is_deepseek_key(model_key),
        },
    );

    let model_config = json!({
        "key": model_key,
        "display_name": catalog.display_name,
        "model": "",
        "format": "openai",
        "is_vl": catalog.vision,
        "is_reasoning": catalog.reasoning,
        "api_key": "",
        "url": "",
        "source": "system",
        "max_input_tokens": catalog.max_input_tokens,
    });

    // 系统提示词：pi 总会带自己的 agent 提示词，直接用；缺失时不补（我们的模板里
    // 不再内置官方那 59KB 提示词，缺省即空）。
    let mut messages: Vec<Value> = Vec::new();
    if let Some(system_text) = &flattened.system_text {
        messages.push(json!({ "role": "system", "content": system_text }));
    }
    messages.extend(flattened.messages.iter().cloned());

    let prompt = last_user_prompt(&flattened.messages);
    let request_id = Uuid::new_v4().to_string();
    let now_ms = super::now_ms();
    let begin_at = conversation.map(|c| c.begin_at).unwrap_or(now_ms);
    let business_id = conversation
        .map(|c| c.id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let chat_record_id = conversation
        .map(|c| c.id.clone())
        .unwrap_or_else(|| request_id.clone());

    let mut parameters = Map::new();
    if let Some(max_tokens) = pick_max_tokens(openai_body) {
        parameters.insert("max_tokens".into(), json!(max_tokens));
    }
    if let Some(effort) = pick_effort(openai_body, catalog) {
        parameters.insert("reasoning_effort".into(), json!(effort));
    }

    let body = json!({
        "request_id": request_id,
        "request_set_id": Uuid::new_v4().to_string(),
        "chat_record_id": chat_record_id,
        "stream": true,
        "chat_task": "FREE_INPUT",
        "chat_context": {
            "chatPrompt": "",
            "extra": {
                "context": [],
                "modelConfig": model_config,
                "originalContent": { "type": "text", "text": prompt },
            },
            "features": [],
            "imageUrls": null,
            "text": { "type": "text", "text": prompt },
        },
        // 顶层 image_urls 官方默认不再填：上游只读 messages 里的 vision parts。
        "image_urls": null,
        "is_reply": true,
        "is_retry": false,
        "session_id": Uuid::new_v4().to_string(),
        "code_language": "",
        "source": 1,
        "version": "3",
        "chat_prompt": "",
        "parameters": Value::Object(parameters),
        "aliyun_user_type": user_type.filter(|value| !value.is_empty()).unwrap_or(DEFAULT_USER_TYPE),
        "session_type": "qodercli",
        "agent_id": "agent_common",
        "task_id": "common",
        "model_config": model_config,
        "messages": messages,
        "tools": normalize_tools(openai_body.get("tools")),
        "business": {
            "product": "cli",
            "type": "agent",
            "stage": "start",
            "id": business_id,
            "name": prompt.chars().take(30).collect::<String>(),
            "begin_at": begin_at,
            // 上游按 business.model_key 归因计费：缺了它新模型会被静默记到 auto。
            "model_key": model_key,
            "version": COSY_VERSION,
        },
    });

    base64::encode_str(&body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qoder_cn::constants::catalog_entry;

    #[test]
    fn structured_models_keep_tool_pairs() {
        let messages = json!([
            {"role": "system", "content": "sys"},
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "", "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "read", "arguments": "{\"p\":1}"}}
            ]},
            {"role": "tool", "tool_call_id": "c1", "name": "read", "content": "out"}
        ]);
        let flat = flatten_messages(
            messages.as_array().unwrap(),
            &FlattenOptions {
                structured: true,
                keep_reasoning: false,
            },
        );
        assert_eq!(flat.system_text.as_deref(), Some("sys"));
        assert_eq!(flat.messages.len(), 3);
        assert_eq!(
            flat.messages[1]["tool_calls"][0]["function"]["name"],
            "read"
        );
        assert_eq!(flat.messages[2]["role"], "tool");
        assert_eq!(flat.messages[2]["content"], "out");
    }

    #[test]
    fn text_models_degrade_tool_history() {
        let messages = json!([
            {"role": "tool", "tool_call_id": "c1", "name": "read", "content": "out"},
            {"role": "assistant", "content": "thinking", "tool_calls": [
                {"id": "c1", "function": {"name": "read", "arguments": {"p": 1}}}
            ]}
        ]);
        let flat = flatten_messages(
            messages.as_array().unwrap(),
            &FlattenOptions {
                structured: false,
                keep_reasoning: false,
            },
        );
        assert_eq!(flat.messages[0]["role"], "user");
        assert!(flat.messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("[工具结果 (read)]"));
        let assistant = flat.messages[1]["content"].as_str().unwrap();
        assert!(assistant.contains(TOOL_CALL_MARKER));
    }

    #[test]
    fn deepseek_models_keep_reasoning_content() {
        let messages = json!([
            {"role": "assistant", "content": "a", "reasoning_content": "why"}
        ]);
        let flat = flatten_messages(
            messages.as_array().unwrap(),
            &FlattenOptions {
                structured: true,
                keep_reasoning: true,
            },
        );
        assert_eq!(flat.messages[0]["reasoning_content"], "why");
    }

    #[test]
    fn body_is_qoder_base64_of_an_agent_turn() {
        let openai = json!({
            "model": "qmodel",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 1234,
            "reasoning_effort": "medium",
            "tools": [{"type": "function", "function": {"name": "read", "parameters": {}}}],
        });
        let entry = catalog_entry("qmodel_38max").unwrap();
        let encoded = build_request_body(&openai, "qmodel_38max", entry, Some("personal"), None);
        let decoded = base64::decode(&encoded).expect("解码");
        let body: Value = serde_json::from_slice(&decoded).expect("JSON");
        assert_eq!(body["model_config"]["key"], "qmodel_38max");
        assert_eq!(body["business"]["model_key"], "qmodel_38max");
        assert_eq!(body["business"]["version"], COSY_VERSION);
        assert_eq!(body["parameters"]["max_tokens"], 1234);
        assert_eq!(body["parameters"]["reasoning_effort"], "medium");
        assert_eq!(body["aliyun_user_type"], "personal");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["messages"][0]["content"], "hello");
        assert!(body["chat_context"]["extra"]["modelConfig"]["key"] == "qmodel_38max");
    }

    #[test]
    fn effort_outside_the_catalog_is_dropped() {
        let openai = json!({
            "messages": [{"role": "user", "content": "x"}],
            "reasoning_effort": "minimal",
        });
        // auto 目录没有档位 → 不接受任何 effort
        let auto = catalog_entry("auto").unwrap();
        let encoded = build_request_body(&openai, "auto", auto, None, None);
        let body: Value = serde_json::from_slice(&base64::decode(&encoded).unwrap()).unwrap();
        assert!(body["parameters"].get("reasoning_effort").is_none());
    }
}
