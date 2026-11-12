//! `ask_user`：向用户提 1–3 个相关问题。
//!
//! 对应 pi-desktop 的 `lib/ask-user-tool.ts`。工具名、参数 schema、校验错误、
//! 返回文本与 `details`（`{status, answers}`）都与 pi 一致；问答往返走
//! [`UiBridge`]：每题一次 `extension_ui_request`（`select` / `confirm` /
//! `input` / `editor`），前端弹窗回答后回 `extension_ui_response`。
//!
//! 关键语义（照抄 pi，`executeAskUser`）：
//! - 问题是**串行**问的，一题一帧；整体 `timeoutMs` 是总预算，每题只用剩余时间。
//! - 选择题回来的是**标签**，标签匹配不上就是用户自己写的答案（`custom: true`）。
//! - `confirm` 的 `false` 是有效答案；只有 `select`/`input`/`editor` 的空答案
//!   才算 cancelled（超时则算 expired）。

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rpi_agent::agent_tool::AgentTool;
use rpi_agent::error::AgentError;
use rpi_agent::types::{AgentToolResult, TextContentOrImage, ToolExecutionMode, ToolResultPartial};
use rpi_ai::types::{Schema, Tool};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::ui::{new_request_id, UiBridge};

const MAX_QUESTIONS: usize = 3;
const MAX_OPTIONS: usize = 8;
const MAX_TEXT_LENGTH: usize = 20_000;
const MIN_TIMEOUT_MS: u64 = 1_000;
const MAX_TIMEOUT_MS: u64 = 10 * 60 * 1000;

pub struct AskUserTool {
    schema: Tool,
    ui: Arc<UiBridge>,
}

pub fn create_ask_user_tool(ui: Arc<UiBridge>) -> Arc<dyn AgentTool> {
    Arc::new(AskUserTool {
        schema: Tool {
            name: "ask_user".to_string(),
            description: "Ask the user for non-sensitive information needed to continue. \
                          Use one call with 1-3 related questions; all questions in the \
                          questions array are shown sequentially. Do not use for passwords, \
                          tokens, private keys, passphrases, OTP codes, or other secrets."
                .to_string(),
            parameters: Schema::new(json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_QUESTIONS,
                        "description": "One to three related questions. Put all questions needed for the current clarification in this array so they are presented sequentially in one interaction.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {
                                    "type": "string",
                                    "minLength": 1,
                                    "maxLength": 100,
                                    "description": "Unique identifier for this question in the current request",
                                },
                                "prompt": {
                                    "type": "string",
                                    "minLength": 1,
                                    "maxLength": 4000,
                                    "description": "The question shown to the user",
                                },
                                "kind": {
                                    "type": "string",
                                    "enum": ["single_select", "confirm", "text", "editor"],
                                },
                                "options": {
                                    "type": "array",
                                    "minItems": 0,
                                    "maxItems": MAX_OPTIONS,
                                    "description": "Options for single_select (2-8 required); use an empty array for confirm, text, and editor questions",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "value": {
                                                "type": "string",
                                                "minLength": 1,
                                                "maxLength": 500,
                                                "description": "Stable answer value returned to the model",
                                            },
                                            "label": {
                                                "type": "string",
                                                "minLength": 1,
                                                "maxLength": 500,
                                                "description": "Human-readable option label shown to the user",
                                            },
                                            "description": {
                                                "type": "string",
                                                "maxLength": 1000,
                                                "description": "Explain the impact or tradeoff of this option",
                                            },
                                        },
                                        "required": ["value", "label"],
                                        "additionalProperties": false,
                                    },
                                },
                                "required": {
                                    "type": "boolean",
                                    "description": "Whether the user must provide an answer",
                                },
                                "placeholder": {
                                    "type": "string",
                                    "maxLength": 500,
                                    "description": "Placeholder for text input",
                                },
                                "defaultValue": {
                                    "description": "Answer used when the question times out",
                                    "anyOf": [
                                        { "type": "string", "maxLength": MAX_TEXT_LENGTH },
                                        { "type": "boolean" },
                                    ],
                                },
                            },
                            "required": ["id", "prompt", "kind", "options"],
                            "additionalProperties": false,
                        },
                    },
                    "timeoutMs": {
                        "type": "integer",
                        "minimum": MIN_TIMEOUT_MS,
                        "maximum": MAX_TIMEOUT_MS,
                        "description": "Optional overall timeout for the complete questionnaire",
                    },
                },
                "required": ["questions"],
                "additionalProperties": false,
            })),
            constrained_sampling: None,
        },
        ui,
    })
}

#[async_trait]
impl AgentTool for AskUserTool {
    fn schema(&self) -> &Tool {
        &self.schema
    }

    fn label(&self) -> &str {
        "Ask User"
    }

    /// 一问一答会挂起整轮等待，不能和别的工具并发跑。
    fn execution_mode(&self) -> ToolExecutionMode {
        ToolExecutionMode::Sequential
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        params: Value,
        signal: CancellationToken,
        _on_update: Arc<dyn Fn(ToolResultPartial) + Send + Sync>,
    ) -> Result<AgentToolResult, AgentError> {
        let (questions, deadline) = normalize_input(&params)?;
        let total = questions.len();
        let mut answers = serde_json::Map::new();
        let mut status = "answered";

        for (index, question) in questions.iter().enumerate() {
            if signal.is_cancelled() {
                status = "aborted";
                break;
            }
            let timeout = match deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining <= Duration::from_millis(1) {
                        status = "expired";
                        break;
                    }
                    Some(remaining)
                }
                None => None,
            };

            let id = new_request_id();
            let frame = request_frame(question, index, total, &id);
            let response = self.ui.request(&id, frame, timeout, signal.clone()).await;

            match question.kind {
                QuestionKind::SingleSelect => {
                    let Some(label) = response.get("value").and_then(Value::as_str) else {
                        status = unresolved_status(&signal, deadline);
                        break;
                    };
                    let matched = question.options.iter().find(|option| option.label == label);
                    let (value, custom) = match matched {
                        Some(option) => (option.value.clone(), false),
                        None => {
                            let typed = label.trim();
                            if typed.is_empty() {
                                status = "cancelled";
                                break;
                            }
                            (typed.to_string(), true)
                        }
                    };
                    let label_field = matched.map(|option| option.label.clone());
                    answers.insert(
                        question.id.clone(),
                        answer_json(&Value::String(value), label_field, custom),
                    );
                }
                QuestionKind::Confirm => {
                    if signal.is_cancelled() {
                        status = "aborted";
                        break;
                    }
                    let confirmed = response
                        .get("confirmed")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    answers.insert(
                        question.id.clone(),
                        answer_json(&Value::Bool(confirmed), None, false),
                    );
                }
                QuestionKind::Text | QuestionKind::Editor => {
                    let Some(text) = response.get("value").and_then(Value::as_str) else {
                        status = unresolved_status(&signal, deadline);
                        break;
                    };
                    if text.chars().count() > MAX_TEXT_LENGTH {
                        status = "aborted";
                        break;
                    }
                    answers.insert(
                        question.id.clone(),
                        answer_json(&Value::String(text.to_string()), None, false),
                    );
                }
            }
        }

        let result = json!({ "status": status, "answers": answers });
        Ok(AgentToolResult {
            content: vec![TextContentOrImage::text(summary(&result))],
            details: result,
            ..Default::default()
        })
    }
}

/// `select` / `input` / `editor` 的空答案：超时算 expired，否则算 cancelled。
fn unresolved_status(signal: &CancellationToken, deadline: Option<Instant>) -> &'static str {
    if signal.is_cancelled() {
        "aborted"
    } else if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        "expired"
    } else {
        "cancelled"
    }
}

fn answer_json(value: &Value, label: Option<String>, custom: bool) -> Value {
    let mut answer = json!({ "value": value });
    if let Some(label) = label {
        answer["label"] = json!(label);
    }
    if custom {
        answer["custom"] = json!(true);
    }
    answer
}

/// 一题的请求帧。`title` 在多题时带序号（pi 的 `titleFor`）。
fn request_frame(question: &Question, index: usize, total: usize, id: &str) -> Value {
    let title = if total == 1 {
        question.prompt.clone()
    } else {
        format!("[{}/{}] {}", index + 1, total, question.prompt)
    };
    match question.kind {
        QuestionKind::SingleSelect => json!({
            "type": "extension_ui_request",
            "id": id,
            "method": "select",
            "title": title,
            "options": question.options.iter().map(|option| option.label.clone()).collect::<Vec<_>>(),
        }),
        QuestionKind::Confirm => json!({
            "type": "extension_ui_request",
            "id": id,
            "method": "confirm",
            "title": title,
            "message": "请确认是否继续。",
        }),
        QuestionKind::Text => {
            let mut frame = json!({
                "type": "extension_ui_request",
                "id": id,
                "method": "input",
                "title": title,
            });
            if let Some(placeholder) = &question.placeholder {
                frame["placeholder"] = json!(placeholder);
            }
            frame
        }
        QuestionKind::Editor => {
            let mut frame = json!({
                "type": "extension_ui_request",
                "id": id,
                "method": "editor",
                "title": title,
            });
            if let Some(default) = question.default_value.as_ref().and_then(Value::as_str) {
                frame["prefill"] = json!(default);
            }
            frame
        }
    }
}

/// pi 的 `formatAskUserSummary`。
fn summary(result: &Value) -> String {
    let status = result["status"].as_str().unwrap_or("answered");
    if status != "answered" {
        return format!("User {status} the questions.");
    }
    let answers = result["answers"].as_object().cloned().unwrap_or_default();
    if answers.is_empty() {
        return "User answered the questions.".to_string();
    }
    let lines: Vec<String> = answers
        .iter()
        .map(|(id, answer)| {
            let value = &answer["value"];
            let text = answer["label"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| render_value(value));
            format!("{id}: {text}")
        })
        .collect();
    format!("User answered:\n{}", lines.join("\n"))
}

fn render_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuestionKind {
    SingleSelect,
    Confirm,
    Text,
    Editor,
}

struct QuestionOption {
    value: String,
    label: String,
}

struct Question {
    id: String,
    prompt: String,
    kind: QuestionKind,
    options: Vec<QuestionOption>,
    placeholder: Option<String>,
    default_value: Option<Value>,
}

/// 校验与归一化。错误信息照 pi 的文案（模型看得到，便于自我纠正）。
fn normalize_input(params: &Value) -> Result<(Vec<Question>, Option<Instant>), AgentError> {
    let raw = params
        .get("questions")
        .and_then(Value::as_array)
        .ok_or_else(|| AgentError::Validation("ask_user requires a questions array".into()))?;
    if raw.is_empty() || raw.len() > MAX_QUESTIONS {
        return Err(AgentError::Validation(format!(
            "ask_user accepts between 1 and {MAX_QUESTIONS} questions"
        )));
    }

    let mut questions = Vec::with_capacity(raw.len());
    for (index, item) in raw.iter().enumerate() {
        let number = index + 1;
        let id = match item.get("id").and_then(Value::as_str) {
            Some(text) if !text.trim().is_empty() => text.trim().to_string(),
            _ => {
                return Err(AgentError::Validation(format!(
                    "Question {number} is missing id"
                )))
            }
        };
        let prompt = match item.get("prompt").and_then(Value::as_str) {
            Some(text) if !text.trim().is_empty() => text.trim().to_string(),
            _ => {
                return Err(AgentError::Validation(format!(
                    "Question {id} is missing prompt"
                )))
            }
        };
        let kind = match item.get("kind").and_then(Value::as_str) {
            Some("single_select") => QuestionKind::SingleSelect,
            Some("confirm") => QuestionKind::Confirm,
            Some("text") => QuestionKind::Text,
            Some("editor") => QuestionKind::Editor,
            other => {
                return Err(AgentError::Validation(format!(
                    "Question {id} has an invalid kind: {}",
                    other.unwrap_or("(missing)")
                )))
            }
        };

        let mut options = Vec::new();
        match item.get("options") {
            Some(Value::Array(raw_options)) if !raw_options.is_empty() => {
                if kind != QuestionKind::SingleSelect {
                    return Err(AgentError::Validation(format!(
                        "Question {id} must use an empty options array for {}",
                        kind_name(kind)
                    )));
                }
                if raw_options.len() < 2 || raw_options.len() > MAX_OPTIONS {
                    return Err(AgentError::Validation(format!(
                        "Question {id} must provide between 2 and {MAX_OPTIONS} options"
                    )));
                }
                for option in raw_options.iter() {
                    let value = option.get("value").and_then(Value::as_str).unwrap_or("");
                    let label = option.get("label").and_then(Value::as_str).unwrap_or("");
                    if value.is_empty() || label.is_empty() {
                        return Err(AgentError::Validation(format!(
                            "Question {id} has an invalid option"
                        )));
                    }
                    options.push(QuestionOption {
                        value: value.to_string(),
                        label: label.to_string(),
                    });
                }
                let mut seen = std::collections::HashSet::new();
                if !options
                    .iter()
                    .all(|option| seen.insert(option.value.clone()))
                {
                    return Err(AgentError::Validation(format!(
                        "Question {id} has duplicate option values"
                    )));
                }
            }
            Some(Value::Array(_)) => {
                // 空数组：选题必须有选项，其它 kind 必须正好是空数组（pi 的规则）。
                if kind == QuestionKind::SingleSelect {
                    return Err(AgentError::Validation(format!(
                        "Question {id} must provide between 2 and {MAX_OPTIONS} options"
                    )));
                }
            }
            Some(_) => {
                return Err(AgentError::Validation(format!(
                    "Question {id} must use an empty options array for {}",
                    kind_name(kind)
                )))
            }
            None if kind == QuestionKind::SingleSelect => {
                return Err(AgentError::Validation(format!(
                    "Question {id} must provide between 2 and {MAX_OPTIONS} options"
                )))
            }
            None => {}
        }

        questions.push(Question {
            id,
            prompt,
            kind,
            options,
            placeholder: item
                .get("placeholder")
                .and_then(Value::as_str)
                .map(str::to_string),
            default_value: item.get("defaultValue").cloned(),
        });
    }

    let mut seen = std::collections::HashSet::new();
    if !questions
        .iter()
        .all(|question| seen.insert(question.id.clone()))
    {
        return Err(AgentError::Validation(
            "ask_user question ids must be unique".into(),
        ));
    }

    // `timeoutMs` 是**整份问卷**的预算；pi 把 1s–10min 之外的值夹回区间。
    let deadline = params
        .get("timeoutMs")
        .and_then(Value::as_u64)
        .map(|millis| millis.clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS))
        .map(|millis| Instant::now() + Duration::from_millis(millis));
    Ok((questions, deadline))
}

fn kind_name(kind: QuestionKind) -> &'static str {
    match kind {
        QuestionKind::SingleSelect => "single_select",
        QuestionKind::Confirm => "confirm",
        QuestionKind::Text => "text",
        QuestionKind::Editor => "editor",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_option_count_and_kind() {
        let missing = json!({
            "questions": [{ "id": "q", "prompt": "选一个", "kind": "single_select", "options": [] }]
        });
        assert!(normalize_input(&missing).is_err());

        let wrong_kind = json!({
            "questions": [{
                "id": "q",
                "prompt": "确认",
                "kind": "confirm",
                "options": [{ "value": "a", "label": "A" }],
            }]
        });
        assert!(normalize_input(&wrong_kind).is_err());
    }

    #[test]
    fn multi_question_titles_are_numbered() {
        let question = Question {
            id: "q".into(),
            prompt: "选哪个".into(),
            kind: QuestionKind::Confirm,
            options: Vec::new(),
            placeholder: None,
            default_value: None,
        };
        assert_eq!(request_frame(&question, 0, 1, "r1")["title"], "选哪个");
        assert_eq!(request_frame(&question, 0, 1, "r1")["id"], "r1");
        assert_eq!(
            request_frame(&question, 1, 3, "r2")["title"],
            "[2/3] 选哪个"
        );
    }

    #[tokio::test]
    async fn single_select_round_trip_returns_option_value() {
        let (events, mut rx) = tokio::sync::broadcast::channel(16);
        let ui = UiBridge::new(events);
        let tool = create_ask_user_tool(ui.clone());
        let params = json!({
            "questions": [{
                "id": "scope",
                "prompt": "选一个",
                "kind": "single_select",
                "options": [
                    { "value": "a", "label": "选项 A" },
                    { "value": "b", "label": "选项 B" }
                ]
            }]
        });
        let handle = tokio::spawn(async move {
            tool.execute(
                "call-1",
                params,
                CancellationToken::new(),
                Arc::new(|_: ToolResultPartial| {}),
            )
            .await
        });

        // 工具必须先把请求帧发给前端（`select` + 选项标签）
        let frame = rx.recv().await.unwrap();
        assert_eq!(frame["type"], "extension_ui_request");
        assert_eq!(frame["method"], "select");
        assert_eq!(frame["title"], "选一个");
        assert_eq!(frame["options"], json!(["选项 A", "选项 B"]));
        assert!(
            frame["expiresAt"].is_null(),
            "没设 timeout 时不该带 expiresAt"
        );

        let id = frame["id"].as_str().unwrap().to_string();
        assert!(ui.respond(&json!({
            "type": "extension_ui_response",
            "id": id,
            "value": "选项 A",
        })));

        let result = handle.await.unwrap().unwrap();
        assert_eq!(result.details["status"], "answered");
        assert_eq!(result.details["answers"]["scope"]["value"], "a");
        assert_eq!(result.details["answers"]["scope"]["label"], "选项 A");

        // 请求作废也要广播，前端才能把弹窗收掉
        let resolved = rx.recv().await.unwrap();
        assert_eq!(resolved["type"], "extension_ui_resolved");
        assert_eq!(resolved["id"], id);
    }

    #[tokio::test]
    async fn unmatched_select_label_is_the_users_custom_answer() {
        let (events, mut rx) = tokio::sync::broadcast::channel(16);
        let ui = UiBridge::new(events);
        let tool = create_ask_user_tool(ui.clone());
        let params = json!({
            "questions": [{
                "id": "scope",
                "prompt": "选一个",
                "kind": "single_select",
                "options": [
                    { "value": "a", "label": "选项 A" },
                    { "value": "b", "label": "选项 B" }
                ]
            }]
        });
        let handle = tokio::spawn(async move {
            tool.execute(
                "call-2",
                params,
                CancellationToken::new(),
                Arc::new(|_: ToolResultPartial| {}),
            )
            .await
        });

        let frame = rx.recv().await.unwrap();
        let id = frame["id"].as_str().unwrap().to_string();
        // 前端允许选择题自由作答：回来的文本匹配不上任何标签
        assert!(ui.respond(&json!({
            "type": "extension_ui_response",
            "id": id,
            "value": "我自己写的答案",
        })));

        let result = handle.await.unwrap().unwrap();
        assert_eq!(
            result.details["answers"]["scope"]["value"],
            "我自己写的答案"
        );
        assert_eq!(result.details["answers"]["scope"]["custom"], true);
    }

    #[test]
    fn summary_matches_pi_wording() {
        assert_eq!(
            summary(&json!({ "status": "cancelled", "answers": {} })),
            "User cancelled the questions."
        );
        assert_eq!(
            summary(&json!({ "status": "answered", "answers": {} })),
            "User answered the questions."
        );
        assert_eq!(
            summary(&json!({ "status": "answered", "answers": {
                "scope": { "value": "a", "label": "选项 A" }
            } })),
            "User answered:\nscope: 选项 A"
        );
    }
}
