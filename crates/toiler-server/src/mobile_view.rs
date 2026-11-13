//! 手机遥控页的消息投影（pi 的 `lib/mobile-state.ts`）。
//!
//! 手机端不渲染 Markdown 之外的重内容（代码高亮、KaTeX、图片预览），所以服务端就要把
//! 会话压成「谁说了什么 + 思考 + 工具调用」。**关键点是不能把原始 `AgentMessage`
//! 直接下发**：toolResult 里的 base64 图片一张就有几百 KB，阶段 5 走查实测一个
//! 120 条消息的窗口能到 474KB（其中 682KB 是 image 块），手机弱网打不开。
//!
//! 投影后的形状（与 pi 逐字段对齐）：
//!
//! ```jsonc
//! { "id": "条目 id", "role": "user" | "assistant", "text": "…",
//!   "thinking": "…",              // 仅 assistant，已截断
//!   "tools": [{ "id","name","hint","input?","output?","isError?" }],
//!   "at": "HH:MM" }
//! ```
//!
//! 窗口与降级（pi 的 `buildMobileWindow`）：
//!
//! * 只送最新的 `limit` 条（默认 30，上限 300），更早的只报数量；
//! * 窗口内**最后 40 条**保留思考与工具细节，更早的降成摘要 —— 否则翻到 300 条时
//!   单次快照又是几 MB。

use serde_json::{json, Map, Value};

/// 默认窗口（pi 的 `MOBILE_DEFAULT_LIMIT`）。
pub const DEFAULT_LIMIT: usize = 30;
/// 上限（pi 的 `MOBILE_MAX_LIMIT`）。
pub const MAX_LIMIT: usize = 300;
/// 保留细节的窗口（pi 的 `MOBILE_DETAIL_WINDOW`）。
pub const DETAIL_WINDOW: usize = 40;

const MAX_MESSAGE_CHARS: usize = 1500;
const MAX_THINKING_CHARS: usize = 1200;
const MAX_TOOL_INPUT_CHARS: usize = 600;
const MAX_TOOL_OUTPUT_CHARS: usize = 800;
const MAX_TOOL_HINT_CHARS: usize = 60;

/// 按**字符**截断（不是字节），超限时补一个 `\n…`。
fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push_str("\n…");
    out
}

/// content blocks 里的正文与图片张数（pi 的 `textFromBlocks`）。
fn text_from_blocks(blocks: Option<&Value>) -> (String, usize) {
    let Some(blocks) = blocks else {
        return (String::new(), 0);
    };
    if let Some(text) = blocks.as_str() {
        return (text.to_string(), 0);
    }
    let Some(list) = blocks.as_array() else {
        return (String::new(), 0);
    };
    let mut parts: Vec<&str> = Vec::new();
    let mut images = 0usize;
    for block in list {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    parts.push(text);
                }
            }
            Some("image") => images += 1,
            _ => {}
        }
    }
    (parts.join("\n").trim().to_string(), images)
}

/// 多个 thinking block 按顺序拼接后截断（pi 的 `thinkingFromBlocks`）。
fn thinking_from_blocks(blocks: Option<&Value>) -> String {
    let Some(list) = blocks.and_then(Value::as_array) else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for block in list {
        if block.get("type").and_then(Value::as_str) != Some("thinking") {
            continue;
        }
        let Some(text) = block.get("thinking").and_then(Value::as_str) else {
            continue;
        };
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    truncate(&parts.join("\n\n"), MAX_THINKING_CHARS)
}

/// 工具调用的「一行摘要」：按固定优先级的几个键取第一行。
fn tool_hint(args: Option<&Value>) -> Option<String> {
    let record = args?.as_object()?;
    for key in [
        "path",
        "file_path",
        "filePath",
        "command",
        "pattern",
        "query",
        "url",
        "prompt",
    ] {
        let Some(value) = record.get(key).and_then(Value::as_str) else {
            continue;
        };
        let first_line = value.trim().split('\n').next().unwrap_or("").trim();
        if first_line.is_empty() {
            continue;
        }
        return Some(if first_line.chars().count() > MAX_TOOL_HINT_CHARS {
            let mut out: String = first_line.chars().take(MAX_TOOL_HINT_CHARS - 3).collect();
            out.push_str("...");
            out
        } else {
            first_line.to_string()
        });
    }
    None
}

/// 工具入参：流式中的 `rawInput` 优先，其次美化 JSON。
fn format_tool_input(args: Option<&Value>, raw_input: Option<&Value>) -> Option<String> {
    if let Some(raw) = raw_input.and_then(Value::as_str) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(truncate(trimmed, MAX_TOOL_INPUT_CHARS));
        }
    }
    let args = args?;
    if args.is_null() {
        return None;
    }
    if let Some(text) = args.as_str() {
        let trimmed = text.trim();
        return (!trimmed.is_empty()).then(|| truncate(trimmed, MAX_TOOL_INPUT_CHARS));
    }
    let pretty = serde_json::to_string_pretty(args).ok()?;
    Some(truncate(&pretty, MAX_TOOL_INPUT_CHARS))
}

/// 一次工具调用（pi 的 `MobileToolCall`）。
struct ToolCall {
    id: String,
    name: String,
    hint: String,
    input: Option<String>,
    output: Option<String>,
    is_error: bool,
}

impl ToolCall {
    fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("id".into(), json!(self.id));
        map.insert("name".into(), json!(self.name));
        map.insert("hint".into(), json!(self.hint));
        if let Some(input) = &self.input {
            map.insert("input".into(), json!(input));
        }
        if let Some(output) = &self.output {
            map.insert("output".into(), json!(output));
        }
        if self.is_error {
            map.insert("isError".into(), json!(true));
        }
        Value::Object(map)
    }
}

/// assistant 消息的 content → 「正文 + 思考 + 工具调用」。
fn project_assistant_blocks(blocks: Option<&Value>) -> (String, String, Vec<ToolCall>) {
    let (text, _) = text_from_blocks(blocks);
    let thinking = thinking_from_blocks(blocks);
    let mut tools: Vec<ToolCall> = Vec::new();

    if let Some(list) = blocks.and_then(Value::as_array) {
        for (index, block) in list.iter().enumerate() {
            if block.get("type").and_then(Value::as_str) != Some("toolCall") {
                continue;
            }
            let name = block
                .get("name")
                .or_else(|| block.get("toolName"))
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string();
            let args = block.get("arguments").or_else(|| block.get("input"));
            let input = format_tool_input(args, block.get("rawInput"));
            let id = block
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    block
                        .get("toolCallId")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                })
                .map(str::to_string)
                .unwrap_or_else(|| format!("tool-{index}"));
            tools.push(ToolCall {
                id,
                name,
                hint: tool_hint(args).unwrap_or_default(),
                input,
                output: None,
                is_error: false,
            });
        }
    }

    (text, thinking, tools)
}

/// 收集 `toolResult` 消息，按 `toolCallId` 索引。
fn collect_tool_results(messages: &[Value]) -> std::collections::HashMap<String, (String, bool)> {
    let mut results = std::collections::HashMap::new();
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("toolResult") {
            continue;
        }
        let Some(id) = message
            .get("toolCallId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let (text, _) = text_from_blocks(message.get("content"));
        let truncated = truncate(text.trim(), MAX_TOOL_OUTPUT_CHARS);
        let is_error = message
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        results.insert(id.to_string(), (truncated, is_error));
    }
    results
}

/// 毫秒时间戳 → `HH:MM`（本地时区）。
fn format_clock(timestamp: Option<&Value>) -> Option<String> {
    let millis = timestamp?.as_i64()?;
    let seconds = millis.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);
    // 不引 chrono：只要时分，用本地 UTC 偏移算一次即可。
    let local_offset = local_utc_offset_seconds();
    let local_secs = (secs_of_day + i64::from(local_offset)).rem_euclid(86_400);
    let _ = days;
    let hour = local_secs / 3600;
    let minute = (local_secs % 3600) / 60;
    Some(format!("{hour:02}:{minute:02}"))
}

/// 当前时区相对 UTC 的偏移秒数（拿不到时按 0 = UTC）。
fn local_utc_offset_seconds() -> i32 {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut local: libc::tm = std::mem::zeroed();
        // SAFETY: 两个分支都只写我们给的 tm。非 unix 没有 `localtime_r`，
        // 也没有 `tm_gmtoff` / `mktime`（见 `desktop_tools/system_time.rs`）。
        #[cfg(unix)]
        {
            if libc::localtime_r(&now, &mut local).is_null() {
                return 0;
            }
            local.tm_gmtoff as i32
        }
        #[cfg(not(unix))]
        {
            if libc::localtime_s(&mut local, &now) != 0 {
                return 0;
            }
            let mut utc: libc::tm = std::mem::zeroed();
            if libc::gmtime_s(&mut utc, &now) != 0 {
                return 0;
            }
            let local_secs =
                local.tm_hour as i64 * 3600 + local.tm_min as i64 * 60 + local.tm_sec as i64;
            let utc_secs = utc.tm_hour as i64 * 3600 + utc.tm_min as i64 * 60 + utc.tm_sec as i64;
            // 同一天的墙上时间差，跨日时向 24h 内回落。
            let mut diff = local_secs - utc_secs;
            const HALF_DAY: i64 = 12 * 3600;
            if diff > HALF_DAY {
                diff -= 24 * 3600;
            } else if diff < -HALF_DAY {
                diff += 24 * 3600;
            }
            diff as i32
        }
    }
}

/// 一条消息 → 手机端的投影（`None` 表示这条不单独成条）。
fn project_message(
    message: &Value,
    id: &str,
    tool_results: &std::collections::HashMap<String, (String, bool)>,
) -> Option<Value> {
    let at = format_clock(message.get("timestamp"));
    let role = message.get("role").and_then(Value::as_str).unwrap_or("");

    if role == "user" {
        let (text, images) = text_from_blocks(message.get("content"));
        let mut body = text;
        if images > 0 {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&format!(
                "[{images} image{}]",
                if images > 1 { "s" } else { "" }
            ));
        }
        if body.is_empty() {
            return None;
        }
        let mut map = Map::new();
        map.insert("id".into(), json!(id));
        map.insert("role".into(), json!("user"));
        map.insert("text".into(), json!(truncate(&body, MAX_MESSAGE_CHARS)));
        if let Some(at) = at {
            map.insert("at".into(), json!(at));
        }
        return Some(Value::Object(map));
    }

    if role == "assistant" {
        let (text, thinking, mut tools) = project_assistant_blocks(message.get("content"));
        for call in tools.iter_mut() {
            let Some((output, is_error)) = tool_results.get(&call.id) else {
                continue;
            };
            // 「(no output)」是 pi 对空结果的占位，展开时不必显示。
            let empty = output.is_empty() || output.trim() == "(no output)";
            if !empty {
                call.output = Some(output.clone());
            }
            call.is_error = *is_error;
        }
        if text.is_empty() && thinking.is_empty() && tools.is_empty() {
            return None;
        }
        let mut map = Map::new();
        map.insert("id".into(), json!(id));
        map.insert("role".into(), json!("assistant"));
        map.insert("text".into(), json!(truncate(&text, MAX_MESSAGE_CHARS)));
        if !thinking.is_empty() {
            map.insert("thinking".into(), json!(thinking));
        }
        if !tools.is_empty() {
            map.insert(
                "tools".into(),
                Value::Array(tools.iter().map(ToolCall::to_json).collect()),
            );
        }
        if let Some(at) = at {
            map.insert("at".into(), json!(at));
        }
        return Some(Value::Object(map));
    }

    // 工具结果与思考过程已并进上一条助手消息，不单独成条。
    None
}

/// 历史消息降成摘要：留正文 + 工具名/摘要行，丢掉思考与展开用的入参/结果。
fn skim(message: &Value) -> Value {
    let tools = message.get("tools").and_then(Value::as_array);
    let has_detail = message
        .get("thinking")
        .and_then(Value::as_str)
        .map(|text| !text.is_empty())
        .unwrap_or(false)
        || tools
            .map(|list| {
                list.iter()
                    .any(|call| call.get("input").is_some() || call.get("output").is_some())
            })
            .unwrap_or(false);
    if !has_detail {
        return message.clone();
    }

    let mut map = Map::new();
    if let Some(id) = message.get("id") {
        map.insert("id".into(), id.clone());
    }
    if let Some(role) = message.get("role") {
        map.insert("role".into(), role.clone());
    }
    if let Some(text) = message.get("text") {
        map.insert("text".into(), text.clone());
    }
    if let Some(list) = tools {
        let slim: Vec<Value> = list
            .iter()
            .map(|call| {
                let mut entry = Map::new();
                if let Some(id) = call.get("id") {
                    entry.insert("id".into(), id.clone());
                }
                if let Some(name) = call.get("name") {
                    entry.insert("name".into(), name.clone());
                }
                if let Some(hint) = call.get("hint") {
                    entry.insert("hint".into(), hint.clone());
                }
                if call.get("isError").and_then(Value::as_bool) == Some(true) {
                    entry.insert("isError".into(), json!(true));
                }
                Value::Object(entry)
            })
            .collect();
        if !slim.is_empty() {
            map.insert("tools".into(), Value::Array(slim));
        }
    }
    if let Some(at) = message.get("at") {
        map.insert("at".into(), at.clone());
    }
    Value::Object(map)
}

/// 手机端要的那一窗消息。
pub struct MobileWindow {
    pub messages: Vec<Value>,
    pub earlier_count: usize,
}

/// 把 context 里的消息（`messages` 与 `entry_ids` 平行）投影成手机窗口。
///
/// `limit` 会被夹在 1..=[`MAX_LIMIT`]；`detail` 为 true 时按 [`DETAIL_WINDOW`]
/// 把更早的历史降级成摘要。
pub fn build_window(
    messages: &[Value],
    entry_ids: &[String],
    limit: usize,
    detail: bool,
) -> MobileWindow {
    let mut projected: Vec<Value> = Vec::with_capacity(messages.len());
    let results = collect_tool_results(messages);
    for (index, message) in messages.iter().enumerate() {
        let id = entry_ids
            .get(index)
            .cloned()
            .unwrap_or_else(|| format!("m{index}"));
        if let Some(item) = project_message(message, &id, &results) {
            projected.push(item);
        }
    }

    let size = limit.clamp(1, MAX_LIMIT);
    let start = projected.len().saturating_sub(size);
    let earlier_count = start;
    let mut visible: Vec<Value> = projected[start..].to_vec();

    if detail {
        let detail_from = visible.len().saturating_sub(DETAIL_WINDOW);
        for item in visible.iter_mut().take(detail_from) {
            *item = skim(item);
        }
    }

    MobileWindow {
        messages: visible,
        earlier_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn images_never_reach_the_phone_as_base64() {
        // 500KB 的 base64 图片块：投影后只剩「[1 image]」几个字符。
        let big = "A".repeat(500_000);
        let messages = vec![json!({
            "role": "user",
            "timestamp": 1794292905000i64,
            "content": [
                {"type": "text", "text": "看图"},
                {"type": "image", "data": big, "mimeType": "image/png"},
            ],
        })];
        let window = build_window(&messages, &ids(&["m1"]), 30, true);
        assert_eq!(window.messages.len(), 1);
        assert_eq!(window.messages[0]["text"], "看图\n[1 image]");
        let serialized = serde_json::to_string(&window.messages).unwrap();
        assert!(
            serialized.len() < 500,
            "投影后不该包含图片字节，实际 {} 字节",
            serialized.len()
        );
    }

    #[test]
    fn tool_results_fold_into_the_assistant_message() {
        let messages = vec![
            json!({"role": "user", "content": [{"type": "text", "text": "跑一下"}]}),
            json!({
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "好的"},
                    {"type": "toolCall", "toolCallId": "c1", "toolName": "bash", "input": {"command": "ls -la /tmp"}},
                ],
            }),
            json!({
                "role": "toolResult",
                "toolCallId": "c1",
                "toolName": "bash",
                "content": [{"type": "text", "text": "file-a\nfile-b"}],
            }),
        ];
        let window = build_window(&messages, &ids(&["m1", "m2", "m3"]), 30, true);
        assert_eq!(window.messages.len(), 2, "toolResult 不单独成条");
        let assistant = &window.messages[1];
        assert_eq!(assistant["text"], "好的");
        let tool = &assistant["tools"][0];
        assert_eq!(tool["id"], "c1");
        assert_eq!(tool["name"], "bash");
        assert_eq!(tool["hint"], "ls -la /tmp");
        assert!(tool["input"].as_str().unwrap().contains("ls -la /tmp"));
        assert!(tool["output"].as_str().unwrap().contains("file-a"));
    }

    #[test]
    fn thinking_is_projected_and_truncated() {
        let long = "想".repeat(2000);
        let messages = vec![json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": long},
                {"type": "thinking", "thinking": "   "},
                {"type": "text", "text": "答案"},
            ],
        })];
        let window = build_window(&messages, &ids(&["m1"]), 30, true);
        let thinking = window.messages[0]["thinking"].as_str().unwrap();
        assert!(
            thinking.chars().count() <= MAX_THINKING_CHARS + 2,
            "思考要被截断"
        );
        assert!(thinking.ends_with('…'), "{thinking}");
        assert_eq!(window.messages[0]["text"], "答案");
    }

    #[test]
    fn empty_assistant_messages_are_dropped() {
        let messages = vec![
            json!({"role": "assistant", "content": [{"type": "text", "text": "   "}]}),
            json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
        ];
        let window = build_window(&messages, &ids(&["m1", "m2"]), 30, true);
        assert_eq!(window.messages.len(), 1);
        assert_eq!(window.messages[0]["role"], "user");
    }

    #[test]
    fn window_keeps_the_tail_and_reports_earlier() {
        let messages: Vec<Value> = (0..100)
            .map(|index| {
                json!({
                    "role": "user",
                    "content": [{"type": "text", "text": format!("消息 {index}")}],
                })
            })
            .collect();
        let entry_ids: Vec<String> = (0..100).map(|i| format!("m{i}")).collect();
        let window = build_window(&messages, &entry_ids, 30, true);
        assert_eq!(window.messages.len(), 30);
        assert_eq!(window.earlier_count, 70);
        assert_eq!(window.messages[0]["text"], "消息 70");
        assert_eq!(window.messages[29]["text"], "消息 99");
    }

    #[test]
    fn limit_is_clamped() {
        let messages: Vec<Value> = (0..400)
            .map(|index| {
                json!({"role": "user", "content": [{"type": "text", "text": format!("m{index}")}]})
            })
            .collect();
        let entry_ids: Vec<String> = (0..400).map(|i| format!("m{i}")).collect();
        // 0 会被夹到 1
        assert_eq!(
            build_window(&messages, &entry_ids, 0, false).messages.len(),
            1
        );
        // 超过上限夹到 300
        assert_eq!(
            build_window(&messages, &entry_ids, 9999, false)
                .messages
                .len(),
            MAX_LIMIT
        );
    }

    #[test]
    fn old_history_is_skimmed_but_recent_keeps_detail() {
        // 60 条，DETAIL_WINDOW=40 → 前 20 条降级，后 40 条保留细节。
        let messages: Vec<Value> = (0..60)
            .map(|index| {
                json!({
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": format!("推理 {index}")},
                        {"type": "toolCall", "id": format!("c{index}"), "name": "bash", "arguments": {"command": "ls"}},
                        {"type": "text", "text": format!("结果 {index}")},
                    ],
                })
            })
            .collect();
        let entry_ids: Vec<String> = (0..60).map(|i| format!("m{i}")).collect();
        let window = build_window(&messages, &entry_ids, 60, true);
        // 前 20 条被降级：没有 thinking、工具只剩 name/hint
        assert!(window.messages[0].get("thinking").is_none());
        let old_tool = &window.messages[0]["tools"][0];
        assert!(old_tool.get("input").is_none(), "降级后不该有入参");
        assert_eq!(old_tool["name"], "bash");
        assert_eq!(old_tool["hint"], "ls");
        // 最后 40 条保留细节
        let recent = &window.messages[window.messages.len() - 1];
        assert!(recent.get("thinking").is_some());
        assert!(recent["tools"][0].get("input").is_some());
    }

    #[test]
    fn detail_off_keeps_everything() {
        let messages: Vec<Value> = (0..60)
            .map(|index| {
                json!({
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "推理"},
                        {"type": "text", "text": format!("结果 {index}")},
                    ],
                })
            })
            .collect();
        let entry_ids: Vec<String> = (0..60).map(|i| format!("m{i}")).collect();
        let window = build_window(&messages, &entry_ids, 60, false);
        assert!(
            window.messages[0].get("thinking").is_some(),
            "detail=false 时不降级"
        );
    }

    #[test]
    fn error_results_are_flagged() {
        let messages = vec![
            json!({
                "role": "assistant",
                "content": [{"type": "toolCall", "id": "c1", "name": "bash", "arguments": {"command": "false"}}],
            }),
            json!({
                "role": "toolResult",
                "toolCallId": "c1",
                "content": [{"type": "text", "text": "exit 1"}],
                "isError": true,
            }),
        ];
        let window = build_window(&messages, &ids(&["m1", "m2"]), 30, true);
        assert_eq!(window.messages[0]["tools"][0]["isError"], true);
    }

    #[test]
    fn empty_tool_output_is_omitted() {
        let messages = vec![
            json!({
                "role": "assistant",
                "content": [{"type": "toolCall", "id": "c1", "name": "bash", "arguments": {}}],
            }),
            json!({
                "role": "toolResult",
                "toolCallId": "c1",
                "content": [{"type": "text", "text": "(no output)"}],
            }),
        ];
        let window = build_window(&messages, &ids(&["m1", "m2"]), 30, true);
        assert!(
            window.messages[0]["tools"][0].get("output").is_none(),
            "pi 的 `(no output)` 占位不该出现在手机上"
        );
    }

    #[test]
    fn clock_is_hh_mm() {
        // 2026-11-10T06:41:45Z 的毫秒值
        let formatted = format_clock(Some(&json!(1794292905000i64))).unwrap();
        assert_eq!(formatted.len(), 5);
        assert_eq!(formatted.chars().nth(2), Some(':'));
    }

    #[test]
    fn tool_input_falls_back_to_pretty_json() {
        let pretty = format_tool_input(Some(&json!({"a": 1})), None).unwrap();
        assert!(pretty.contains("\"a\": 1"), "{pretty}");
        // 流式片段优先
        let streaming = format_tool_input(Some(&json!({})), Some(&json!("{\"a\":"))).unwrap();
        assert_eq!(streaming, "{\"a\":");
    }

    #[test]
    fn tool_hint_uses_the_first_line() {
        let hint = tool_hint(Some(&json!({"command": "cd /tmp &&\nls"}))).unwrap();
        assert_eq!(hint, "cd /tmp &&");
        let long = tool_hint(Some(&json!({"path": "a".repeat(200)}))).unwrap();
        assert!(long.ends_with("..."));
        assert!(long.chars().count() <= MAX_TOOL_HINT_CHARS);
    }
}
