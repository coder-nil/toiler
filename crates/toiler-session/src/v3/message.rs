//! v3 消息（pi 线格式，`role` 判别）↔ `rpi_agent::AgentMessage`（`kind` 判别）。
//!
//! pi 的 v3 文件里，message 条目长这样：
//!
//! ```json
//! {"type":"message","id":"…","parentId":"…","timestamp":"ISO",
//!  "message":{"role":"assistant","content":[…],"stopReason":"toolUse",…}}
//! ```
//!
//! rpi 的持久化消息是四臂枚举（`kind: user|assistant|tool_result|custom`）。
//! 三处必须归一，都是 pi 与 rpi 的**词法差异**，不是语义差异：
//!
//! | pi 写 | rpi 认 | 处理 |
//! |---|---|---|
//! | `stopReason: "toolUse"` | `"tooluse"` | 读时小写、写时还原 |
//! | `role: "system"` / `"bashExecution"` | 无此臂 | 映射为 `CustomMessage` |
//! | bashExecution 无 `content` | `CustomMessage.content` 必填 | 用 `output` 合成文本块 |

use rpi_agent::message::AgentMessage;
use rpi_ai::types::Content;
use serde_json::{json, Map, Value};

use super::error::V3Error;

/// pi 里出现在 `role` 位置、但 rpi 没有对应臂的取值，统一走 `CustomMessage`。
fn is_custom_role(role: &str) -> bool {
    !matches!(role, "user" | "assistant" | "toolResult")
}

/// v3 的 `message` 对象 → `AgentMessage`。
pub fn decode_message(value: &Value, fallback_ts: i64) -> Result<AgentMessage, V3Error> {
    let obj = value
        .as_object()
        .ok_or_else(|| V3Error::schema(0, "message 不是 JSON 对象"))?;
    let role = obj
        .get("role")
        .and_then(Value::as_str)
        .ok_or_else(|| V3Error::schema(0, "message 缺少 role"))?;

    let mut msg = obj.clone();
    msg.entry("timestamp").or_insert(json!(fallback_ts));

    if is_custom_role(role) {
        return Ok(AgentMessage::Custom(build_custom(role, obj, fallback_ts)));
    }

    let kind = match role {
        "user" => "user",
        "assistant" => "assistant",
        "toolResult" => "tool_result",
        _ => unreachable!("covered by is_custom_role"),
    };
    msg.insert("kind".into(), json!(kind));
    if role == "assistant" {
        normalize_stop_reason(&mut msg);
    }

    serde_json::from_value(Value::Object(msg)).map_err(|e| {
        V3Error::schema(
            0,
            format!("message 无法映射到 rpi：{}", one_line(&e.to_string())),
        )
    })
}

/// `stopReason` 归一：pi 的 camelCase → rpi 的小写枚举名。
fn normalize_stop_reason(msg: &mut Map<String, Value>) {
    if let Some(Value::String(raw)) = msg.get("stopReason").cloned() {
        let normalized = match raw.as_str() {
            "toolUse" => "tooluse".to_string(),
            other => other.to_ascii_lowercase(),
        };
        msg.insert("stopReason".into(), json!(normalized));
    }
}

/// 把 pi 的非标准 role 摊成一个 `CustomMessage`：`data` 存原载荷，`content` 存可渲染文本。
fn build_custom(
    role: &str,
    obj: &Map<String, Value>,
    fallback_ts: i64,
) -> rpi_agent::message::CustomMessage {
    let mut data = obj.clone();
    data.remove("role");
    data.remove("content");
    data.remove("timestamp");

    let content = match obj.get("content") {
        // pi 的 system 消息写 `"content": ""`（正文在 sections 里）
        Some(Value::String(s)) if s.is_empty() => Vec::new(),
        Some(Value::String(s)) => {
            // bashExecution 把正文放在 output 里，content 缺失
            vec![text_block(s)]
        }
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| serde_json::from_value::<Content>(item.clone()).ok())
            .collect(),
        _ => match obj.get("output").and_then(Value::as_str) {
            Some(out) if !out.is_empty() => vec![text_block(out)],
            _ => Vec::new(),
        },
    };

    let timestamp = obj
        .get("timestamp")
        .and_then(Value::as_i64)
        .unwrap_or(fallback_ts);

    rpi_agent::message::CustomMessage::new(role, content, Value::Object(data), timestamp)
}

fn text_block(text: &str) -> Content {
    Content::text(text)
}

/// `AgentMessage` → v3 的 `message` 对象（`kind` 去掉，`stopReason` 还原 camelCase）。
pub fn encode_message(message: &AgentMessage) -> Value {
    let mut value = serde_json::to_value(message).unwrap_or(Value::Null);
    let Some(obj) = value.as_object_mut() else {
        return value;
    };
    let kind = obj
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("user")
        .to_string();
    obj.remove("kind");

    if kind == "custom" {
        return encode_custom(obj);
    }
    // pi 的 v3 里 user 的 content 一律是块数组；rpi 的 UserContent 在
    // 单段纯文本时会退化成裸字符串，这里补回数组形态。
    if kind == "user" {
        if let Some(Value::String(text)) = obj.get("content").cloned() {
            obj.insert("content".into(), json!([{ "type": "text", "text": text }]));
        }
    }
    if kind == "assistant" {
        if let Some(Value::String(raw)) = obj.get("stopReason").cloned() {
            let pi_style = match raw.as_str() {
                "tooluse" => "toolUse".to_string(),
                other => other.to_string(),
            };
            obj.insert("stopReason".into(), json!(pi_style));
        }
    }
    value
}

/// pi 认可的 message role 集合（`dist/core/messages.js` 的 `convertToLlm` 分支）。
/// 不在这个集合里的 role，写进 v3 时统一落成 `"custom"` + `customType`。
const PI_KNOWN_ROLES: &[&str] = &[
    "bashExecution",
    "branchSummary",
    "compactionSummary",
    "custom",
    "system",
];

/// `CustomMessage {role, content, data, timestamp}` → pi 的扁平 `{role, …payload}`。
fn encode_custom(obj: &mut Map<String, Value>) -> Value {
    let role = obj.remove("role").unwrap_or(json!("custom"));
    let content = obj.remove("content");
    let timestamp = obj.remove("timestamp");

    let mut out = Map::new();
    if let Some(Value::Object(data)) = obj.remove("data") {
        for (k, v) in data {
            out.insert(k, v);
        }
    }
    let role = match role.as_str() {
        Some(r) if PI_KNOWN_ROLES.contains(&r) => role,
        // pi 不认识这个 role：降级成官方的 custom 消息，原名进 customType
        other => {
            let custom_type = out
                .get("customType")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| other.map(str::to_string))
                .unwrap_or_else(|| "custom".into());
            out.entry("customType".to_string())
                .or_insert(json!(custom_type));
            json!("custom")
        }
    };
    out.insert("role".into(), role);
    // 只有真的带文本块时才写 content：pi 的 bashExecution 本来就没有这个键
    let keep = match &content {
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        _ => false,
    };
    if keep {
        if let Some(c) = content {
            out.insert("content".into(), c);
        }
    }
    if let Some(t) = timestamp {
        out.insert("timestamp".into(), t);
    }
    Value::Object(out)
}

fn one_line(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect()
}
