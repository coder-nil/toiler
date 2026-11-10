//! v3 条目行 ↔ `rpi_harness::session::types::Entry`。
//!
//! 读：v3 一行 → 补齐 rpi 要的 `seq`（v3 没有）、把 ISO 时间戳换成毫秒、
//! 折叠 pi 私有条目类型 → `Entry`。
//!
//! 写：`Entry` → v3 一行。**只用于我们自己新增的条目**，老行永远原样保留，
//! 所以文件里已有的字节不会被重写（pi 仍能正常打开）。

use rpi_harness::session::types::Entry;
use serde_json::{json, Map, Value};

use super::error::V3Error;
use super::message;
use crate::time::ms_to_iso;

/// pi 有、但 rpi 没有对应臂的条目类型 → 折叠为 `Entry::Custom`。
/// 反方向（写文件）时靠 `custom_type` 还原成 pi 的原始类型名。
const PI_PRIVATE_ENTRY_TYPES: &[&str] = &["session_info", "context_edit"];

/// compaction 条目在 v3 里独有的字段，rpi 没有对应字段，寄存在 `details.__v3`。
const COMPACTION_V3_EXTRAS: &[&str] = &["firstKeptEntryId", "fromHook", "systemMessage"];

/// 解析一行 v3 条目。`seq` 由调用方按文件行序给出（v3 文件没有这个字段）。
pub fn decode_entry(value: &Value, seq: u64) -> Result<Entry, V3Error> {
    let normalized = normalize(value, seq)?;
    serde_json::from_value::<Entry>(normalized).map_err(|e| {
        V3Error::schema(
            seq as usize,
            format!(
                "{} 条目无法映射到 rpi：{}",
                value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("<none>"),
                e.to_string().split_whitespace().collect::<Vec<_>>().join(" ")
            ),
        )
    })
}

/// 把 v3 行补齐成 rpi `Entry` 能直接反序列化的形状。
fn normalize(value: &Value, seq: u64) -> Result<Value, V3Error> {
    let obj = value
        .as_object()
        .ok_or_else(|| V3Error::schema(seq as usize, "条目不是 JSON 对象"))?;
    let ty = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| V3Error::schema(seq as usize, "条目缺少 type"))?;

    let mut out = Map::new();
    for (k, v) in obj {
        if k == "type" || k == "seq" {
            continue;
        }
        out.insert(k.clone(), v.clone());
    }
    out.insert("seq".into(), json!(seq));
    out.entry("parentId").or_insert(Value::Null);

    // v3 的 timestamp 是 ISO 字符串，rpi 条目层要 i64 毫秒
    let entry_ts = match obj.get("timestamp") {
        Some(Value::String(iso)) => {
            crate::time::iso_to_ms(iso).ok_or_else(|| V3Error::Timestamp {
                line: seq as usize,
                value: iso.clone(),
            })?
        }
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        _ => 0,
    };
    out.insert("timestamp".into(), json!(entry_ts));

    match ty {
        "message" => {
            let raw = obj
                .get("message")
                .ok_or_else(|| V3Error::schema(seq as usize, "message 条目缺少 message"))?;
            let agent = message::decode_message(raw, entry_ts)?;
            out.insert("message".into(), serde_json::to_value(agent).unwrap_or(Value::Null));
            out.insert("type".into(), json!("message"));
        }
        "model_change" | "thinking_level_change" | "branch_summary" | "custom" => {
            out.insert("type".into(), json!(ty));
        }
        "active_tools_change" => {
            out.insert("type".into(), json!("active_tools_change"));
        }
        "compaction" => {
            out.insert("type".into(), json!("compaction"));
            // rpi 要 retainedTail（空数组合法）；v3 用 firstKeptEntryId 表达保留位置
            out.entry("retainedTail").or_insert_with(|| Value::Array(vec![]));
            out.entry("tokensBefore").or_insert(json!(0));

            let mut details = obj
                .get("details")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let mut stash = Map::new();
            for key in COMPACTION_V3_EXTRAS {
                if let Some(v) = obj.get(*key) {
                    stash.insert((*key).to_string(), v.clone());
                }
            }
            if !stash.is_empty() {
                details.insert("__v3".into(), Value::Object(stash));
            }
            if details.is_empty() {
                out.remove("details");
            } else {
                out.insert("details".into(), Value::Object(details));
            }
            for key in COMPACTION_V3_EXTRAS {
                out.remove(*key);
            }
        }
        // pi 私有类型（session_info / context_edit）→ Custom 条目，原载荷进 data
        other => {
            out.insert("type".into(), json!("custom"));
            out.insert("customType".into(), json!(other));
            let mut data = obj.clone();
            for key in ["type", "id", "parentId", "timestamp", "seq"] {
                data.remove(key);
            }
            out.insert("data".into(), Value::Object(data));
        }
    }

    Ok(Value::Object(out))
}

/// 文件里能否出现这种条目类型（决定加载时是否当作致命错误）。
pub fn is_known_v3_entry_type(ty: &str) -> bool {
    matches!(
        ty,
        "message"
            | "model_change"
            | "thinking_level_change"
            | "compaction"
            | "branch_summary"
            | "active_tools_change"
            | "custom"
    ) || PI_PRIVATE_ENTRY_TYPES.contains(&ty)
}

/// `Entry` → v3 条目 JSON（不含 `seq`，`timestamp` 为 ISO 字符串）。
///
/// 返回 `Err(Unrepresentable)` 表示 v3 文件里没有对应的条目类型——调用方
/// 应当**跳过这条**（只留在内存里），而不是写出一条 pi 读不懂的行。
pub fn encode_entry(entry: &Entry) -> Result<Value, V3Error> {
    let mut flat = entry.to_flat_json();
    let obj = flat
        .as_object_mut()
        .ok_or_else(|| V3Error::Unrepresentable {
            entry_type: entry.entry_type().to_string(),
        })?;

    obj.remove("seq");
    if let Some(ts) = obj.get("timestamp").and_then(Value::as_i64) {
        obj.insert("timestamp".into(), json!(ms_to_iso(ts)));
    }

    match entry.entry_type() {
        "message" => {
            if let Some(Value::Object(msg)) = obj.get_mut("message") {
                let encoded = message::encode_message(
                    &serde_json::from_value::<rpi_agent::message::AgentMessage>(
                        Value::Object(msg.clone()),
                    )
                    .unwrap_or_else(|_| unreachable!("Entry::Message always holds an AgentMessage")),
                );
                *msg = match encoded {
                    Value::Object(m) => m,
                    _ => msg.clone(),
                };
            }
        }
        "model_change" | "thinking_level_change" => {}
        "compaction" => {
            // retainedTail 是 rpi 专有；v3 用 firstKeptEntryId
            obj.remove("retainedTail");
            let stash = obj
                .get_mut("details")
                .and_then(Value::as_object_mut)
                .and_then(|d| d.remove("__v3"))
                .and_then(|v| match v {
                    Value::Object(m) => Some(m),
                    _ => None,
                });
            let empty = obj
                .get("details")
                .and_then(Value::as_object)
                .is_some_and(|d| d.is_empty());
            if empty {
                obj.remove("details");
            }
            if let Some(stash) = stash {
                for (k, v) in stash {
                    obj.insert(k, v);
                }
            }
        }
        "custom" => {
            let custom_type = obj
                .remove("customType")
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let data = obj.remove("data").and_then(|v| match v {
                Value::Object(m) => Some(m),
                _ => None,
            });
            // customType 能对上 pi 的原始条目名才还原；否则 v3 表达不了
            if !PI_PRIVATE_ENTRY_TYPES.contains(&custom_type.as_str()) {
                return Err(V3Error::Unrepresentable {
                    entry_type: format!("custom:{custom_type}"),
                });
            }
            if let Some(data) = data {
                for (k, v) in data {
                    obj.insert(k, v);
                }
            }
            obj.insert("type".into(), json!(custom_type));
        }
        other => {
            return Err(V3Error::Unrepresentable {
                entry_type: other.to_string(),
            })
        }
    }

    Ok(flat)
}

/// 解析结果：条目本体 + 用于分支回放的原始信息。
pub struct ParsedEntry {
    pub entry_type: String,
    pub parent_id: Option<String>,
    pub entry: Entry,
}
