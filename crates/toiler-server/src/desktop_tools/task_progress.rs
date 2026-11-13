//! `task_progress`：结构化任务列表。
//!
//! 对应 pi-desktop 的 `lib/task-progress-tool.ts`。工具名、参数 schema、
//! 校验错误、返回文本与 `details` 形状都与 pi 一致；差异只在**状态怎么出门**：
//!
//! | 项 | pi（TS） | 这里 |
//! |---|---|---|
//! | 展示 | `ctx.ui.setStatus/setWidget` | 同形状的 SSE 事件（[`UiBridge`]），并镜像进 `get_state.extensionStatuses/extensionWidgets` |
//! | 恢复 | `session_start` / `session_tree` 时从分支里的 `toolResult.details` 重建 | 开会话与 `navigate_tree` 后从 v3 文件里最后一条 `task_progress` 的 details 重建 |
//! | 进行中 | 完全由模型给定；它跳过 `in_progress` 直接写 `completed` 时，界面就一条活跃项都没有 | 程序自己维护「进行中」游标（见 [`advance`]）：没有任何 `in_progress` 时把下一条 `pending` 提上来 |
//!
//! 最后一行是唯一的语义差异：参数、校验、返回文本、`details` 形状仍然逐字对齐 pi，
//! 变的只是存进去的 `status` 值。模型经常一步到位把某条写成 `completed`，如果界面
//! 于是停在「还有 2 项未完成」却说不出在做哪一条，任务面板就等于失效了。
//!
//! 为什么要镜像进 `get_state`：前端渲染任务框读的是
//! `state.extensionStatuses` / `state.extensionWidgets`（`useAgentSession.ts`），
//! 只发事件的话刷新一次界面任务框就没了。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rpi_agent::agent_tool::AgentTool;
use rpi_agent::error::AgentError;
use rpi_agent::types::{AgentToolResult, TextContentOrImage, ToolResultPartial};
use rpi_ai::types::{Schema, Tool};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::ui::UiBridge;

/// 前端任务框的 widget key（`TASK_WIDGET_KEY`，pi 用的是中文键）。
pub const WIDGET_KEY: &str = "任务进度";
/// 状态栏 key（`TASK_STATUS_KEY`）。
pub const STATUS_KEY: &str = "task-progress";

const MAX_TASKS: usize = 20;
const MAX_TEXT_LENGTH: usize = 2_000;
const WIDGET_PLACEMENT: &str = "aboveEditor";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Blocked,
    Skipped,
}

impl TaskStatus {
    fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "pending" => Self::Pending,
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "blocked" => Self::Blocked,
            "skipped" => Self::Skipped,
            _ => return None,
        })
    }

    fn finished(self) -> bool {
        matches!(self, Self::Completed | Self::Skipped)
    }

    fn symbol(self) -> &'static str {
        match self {
            Self::InProgress => "●",
            Self::Completed => "✓",
            Self::Failed => "✕",
            Self::Blocked => "!",
            Self::Skipped => "−",
            Self::Pending => "○",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskItem {
    pub id: String,
    pub title: String,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// 任务列表 + 展示通道。`LiveAgent` 持同一份，用来填 `get_state` 的两个字段。
pub struct TaskProgressStore {
    tasks: Mutex<Vec<TaskItem>>,
    ui: Arc<UiBridge>,
}

impl TaskProgressStore {
    pub fn new(ui: Arc<UiBridge>) -> Arc<Self> {
        Arc::new(Self {
            tasks: Mutex::new(Vec::new()),
            ui,
        })
    }

    pub fn tasks(&self) -> Vec<TaskItem> {
        self.tasks.lock().expect("tasks").clone()
    }

    /// 开会话 / 切分支后从 transcript 恢复，并把界面同步过去。
    pub fn restore(&self, tasks: Vec<TaskItem>) {
        let mut tasks = tasks;
        advance(&mut tasks);
        *self.tasks.lock().expect("tasks") = tasks;
        self.broadcast();
    }

    fn replace(&self, tasks: Vec<TaskItem>) {
        let mut tasks = tasks;
        advance(&mut tasks);
        *self.tasks.lock().expect("tasks") = tasks;
        self.broadcast();
    }

    fn update(&self, id: &str, patch: TaskPatch) -> Result<Vec<TaskItem>, AgentError> {
        let mut tasks = self.tasks.lock().expect("tasks");
        let index = tasks.iter().position(|task| task.id == id).ok_or_else(|| {
            AgentError::Validation(format!(
                "Task {id} does not exist; use action=replace to create tasks"
            ))
        })?;
        if patch.status == Some(TaskStatus::InProgress)
            && tasks
                .iter()
                .enumerate()
                .any(|(other, task)| other != index && task.status == TaskStatus::InProgress)
        {
            return Err(AgentError::Validation(
                "only one task may be in_progress".to_string(),
            ));
        }
        let current = &mut tasks[index];
        if let Some(title) = patch.title {
            current.title = title;
        }
        if let Some(status) = patch.status {
            current.status = status;
            // pi：改了状态又没给 detail 时，旧 detail 描述的是上一个状态，必须清掉。
            if patch.detail.is_none() {
                current.detail = None;
            }
        }
        if let Some(detail) = patch.detail {
            current.detail = Some(detail);
        }
        advance(&mut tasks);
        let snapshot = tasks.clone();
        drop(tasks);
        self.broadcast();
        Ok(snapshot)
    }

    /// 状态栏文本（`get_state.extensionStatuses` 用）。空 / 全部完成时为 `None`。
    pub fn status_text(&self) -> Option<String> {
        presentation(&self.tasks()).0
    }

    /// 任务框行（`get_state.extensionWidgets` 用）。
    pub fn widget_lines(&self) -> Option<Vec<String>> {
        presentation(&self.tasks()).1
    }

    /// 把当前状态推给前端（事件版，与镜像版同一份计算）。
    fn broadcast(&self) {
        let (status, widget) = presentation(&self.tasks());
        self.ui.set_status(STATUS_KEY, status.as_deref());
        self.ui
            .set_widget(WIDGET_KEY, widget.as_deref(), WIDGET_PLACEMENT);
    }
}

/// 自动推进「进行中」游标：完成一条之后，下一条就是进行中的。
///
/// 模型的常见毛病是 `replace`/`update` 时直接写 `completed`，于是清单里一条
/// `in_progress` 都没有 —— 面板只能显示「还有 2 项未完成」，说不出「现在在做哪一条」。
///
/// 只在**没有任何 `in_progress`** 时生效，规则三条：
///
/// - 列表里已经有 `in_progress` → 不动（模型自己指定了游标）；
/// - 有 `blocked` / `failed` → 不动：卡住或失败需要显式处理，不能假装在推进；
/// - 否则把**第一条 `pending`** 提上来；没有 `pending`（全部收尾）也不动，
///   这时 [`presentation`] 会把面板整个收掉。
///
/// 恢复（`restore`）也走这一套：历史会话里同样是被跳过 `in_progress` 写出来的，
/// 开出来时应该与当时在跑的时候长一样。
fn advance(tasks: &mut [TaskItem]) {
    if tasks
        .iter()
        .any(|task| task.status == TaskStatus::InProgress)
    {
        return;
    }
    if tasks
        .iter()
        .any(|task| matches!(task.status, TaskStatus::Blocked | TaskStatus::Failed))
    {
        return;
    }
    if let Some(next) = tasks
        .iter_mut()
        .find(|task| task.status == TaskStatus::Pending)
    {
        // detail 不清：模型写的那句解释对新顶上来的这条同样成立，
        // 而 pi 的「改状态就清 detail」针对的是模型自己的显式改动。
        next.status = TaskStatus::InProgress;
    }
}

/// `(状态栏文本, 任务框行)`；`None` 表示该元素不该存在。
fn presentation(tasks: &[TaskItem]) -> (Option<String>, Option<Vec<String>>) {
    if tasks.is_empty() {
        return (None, None);
    }
    let finished = tasks.iter().all(|task| task.status.finished());
    if finished {
        // pi：全部完成就把任务框与状态栏收掉，界面回到干净状态。
        return (None, None);
    }
    let completed = tasks.iter().filter(|task| task.status.finished()).count();
    let suffix = if let Some(active) = tasks
        .iter()
        .find(|task| task.status == TaskStatus::InProgress)
    {
        format!(" · 正在执行：{}", active.title)
    } else if let Some(blocked) = tasks.iter().find(|task| task.status == TaskStatus::Blocked) {
        format!(" · 已阻塞：{}", blocked.title)
    } else if let Some(failed) = tasks.iter().find(|task| task.status == TaskStatus::Failed) {
        format!(" · 执行失败：{}", failed.title)
    } else {
        String::new()
    };
    let status = format!("任务 {completed}/{}{suffix}", tasks.len());

    let mut lines = vec![format!("已完成 {completed}/{}", tasks.len())];
    for task in tasks {
        let detail = task
            .detail
            .as_deref()
            .map(|detail| format!(" — {detail}"))
            .unwrap_or_default();
        lines.push(format!("{} {}{detail}", task.status.symbol(), task.title));
    }
    (Some(status), Some(lines))
}

struct TaskPatch {
    title: Option<String>,
    status: Option<TaskStatus>,
    detail: Option<String>,
}

pub struct TaskProgressTool {
    schema: Tool,
    store: Arc<TaskProgressStore>,
}

pub fn create_task_progress_tool(store: Arc<TaskProgressStore>) -> Arc<dyn AgentTool> {
    Arc::new(TaskProgressTool {
        schema: Tool {
            name: "task_progress".to_string(),
            description: "Create and update the structured task list shown in the toiler \
                          interface. Use it for substantial multi-step work so the user can see \
                          each task's live state."
                .to_string(),
            parameters: Schema::new(json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["replace", "update", "clear"],
                    },
                    "tasks": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_TASKS,
                        "description": "Required for replace: the complete ordered task list. Existing tasks are replaced atomically.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {
                                    "type": "string",
                                    "minLength": 1,
                                    "maxLength": 100,
                                    "description": "Stable task identifier",
                                },
                                "title": {
                                    "type": "string",
                                    "minLength": 1,
                                    "maxLength": 500,
                                    "description": "Short user-facing task title",
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed", "failed", "blocked", "skipped"],
                                },
                                "detail": {
                                    "type": "string",
                                    "maxLength": MAX_TEXT_LENGTH,
                                    "description": "Optional current activity, result, failure, or blocking reason",
                                },
                            },
                            "required": ["id", "title", "status"],
                            "additionalProperties": false,
                        },
                    },
                    "id": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 100,
                        "description": "Required for update: existing task identifier",
                    },
                    "title": { "type": "string", "minLength": 1, "maxLength": 500 },
                    "status": {
                        "type": "string",
                        "enum": ["pending", "in_progress", "completed", "failed", "blocked", "skipped"],
                    },
                    "detail": { "type": "string", "maxLength": MAX_TEXT_LENGTH },
                },
                "required": ["action"],
                "additionalProperties": false,
            })),
            constrained_sampling: None,
        },
        store,
    })
}

#[async_trait]
impl AgentTool for TaskProgressTool {
    fn schema(&self) -> &Tool {
        &self.schema
    }

    fn label(&self) -> &str {
        "Task Progress"
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        params: Value,
        _signal: CancellationToken,
        _on_update: Arc<dyn Fn(ToolResultPartial) + Send + Sync>,
    ) -> Result<AgentToolResult, AgentError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError::Validation("task_progress requires an action".into()))?;

        let tasks = match action {
            "clear" => {
                self.store.replace(Vec::new());
                Vec::new()
            }
            "replace" => {
                let items = parse_tasks(&params)?;
                self.store.replace(items);
                // 返回文本用 store 的最终状态（已被 [`advance`] 推进过），
                // 不是模型给的原始列表：模型看到的确认必须与界面一致，
                // 否则它会以为「下一条还没开始」，下一轮又去重写整张表。
                self.store.tasks()
            }
            "update" => self
                .store
                .update(&patch_id(&params)?, patch_from(&params)?)?,
            other => {
                return Err(AgentError::Validation(format!(
                    "task_progress action is invalid: {other}"
                )))
            }
        };

        let text = if tasks.is_empty() {
            "Task progress cleared.".to_string()
        } else {
            presentation(&tasks).1.unwrap_or_default().join("\n")
        };
        Ok(AgentToolResult {
            content: vec![TextContentOrImage::text(text)],
            details: json!({ "tasks": tasks }),
            ..Default::default()
        })
    }
}

fn patch_id(params: &Value) -> Result<String, AgentError> {
    let id = params.get("id").and_then(Value::as_str).unwrap_or_default();
    normalize_text(id, "task id", 100)
}

fn parse_tasks(params: &Value) -> Result<Vec<TaskItem>, AgentError> {
    let raw = params
        .get("tasks")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AgentError::Validation(format!(
                "task_progress replace requires between 1 and {MAX_TASKS} tasks"
            ))
        })?;
    if raw.is_empty() || raw.len() > MAX_TASKS {
        return Err(AgentError::Validation(format!(
            "task_progress replace requires between 1 and {MAX_TASKS} tasks"
        )));
    }
    let mut tasks = Vec::with_capacity(raw.len());
    for (index, item) in raw.iter().enumerate() {
        let prefix = format!("Task {}", index + 1);
        let id = normalize_text(
            item.get("id").and_then(Value::as_str).unwrap_or_default(),
            &format!("{prefix} id"),
            100,
        )?;
        let title = normalize_text(
            item.get("title")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            &format!("{prefix} title"),
            500,
        )?;
        let status = normalize_status(item.get("status").and_then(Value::as_str))?;
        let detail = match item.get("detail").and_then(Value::as_str) {
            Some(text) if !text.trim().is_empty() => Some(normalize_text(
                text,
                &format!("{prefix} detail"),
                MAX_TEXT_LENGTH,
            )?),
            _ => None,
        };
        tasks.push(TaskItem {
            id,
            title,
            status,
            detail,
        });
    }
    let mut seen = std::collections::HashSet::new();
    if !tasks.iter().all(|task| seen.insert(task.id.clone())) {
        return Err(AgentError::Validation("task ids must be unique".into()));
    }
    if tasks
        .iter()
        .filter(|task| task.status == TaskStatus::InProgress)
        .count()
        > 1
    {
        return Err(AgentError::Validation(
            "only one task may be in_progress".into(),
        ));
    }
    Ok(tasks)
}

fn patch_from(params: &Value) -> Result<TaskPatch, AgentError> {
    let title = match params.get("title").and_then(Value::as_str) {
        Some(text) => Some(normalize_text(text, "task title", 500)?),
        None => None,
    };
    let status = match params.get("status").and_then(Value::as_str) {
        Some(raw) => Some(normalize_status(Some(raw))?),
        None => None,
    };
    let detail = match params.get("detail").and_then(Value::as_str) {
        Some(text) => Some(normalize_text(text, "task detail", MAX_TEXT_LENGTH)?),
        None => None,
    };
    if title.is_none() && status.is_none() && detail.is_none() {
        return Err(AgentError::Validation(
            "task_progress update requires title, status, or detail".into(),
        ));
    }
    Ok(TaskPatch {
        title,
        status,
        detail,
    })
}

fn normalize_status(raw: Option<&str>) -> Result<TaskStatus, AgentError> {
    raw.and_then(TaskStatus::parse).ok_or_else(|| {
        AgentError::Validation("task status must be one of pending / in_progress / completed / failed / blocked / skipped".into())
    })
}

fn normalize_text(raw: &str, field: &str, max_length: usize) -> Result<String, AgentError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(AgentError::Validation(format!("{field} must not be empty")));
    }
    if text.chars().count() > max_length {
        return Err(AgentError::Validation(format!("{field} is too long")));
    }
    Ok(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::broadcast;

    fn store() -> (Arc<TaskProgressStore>, broadcast::Receiver<Value>) {
        let (events, rx) = broadcast::channel(32);
        let ui = UiBridge::new(events);
        (TaskProgressStore::new(ui), rx)
    }

    fn item(id: &str, status: TaskStatus) -> TaskItem {
        TaskItem {
            id: id.to_string(),
            title: format!("任务 {id}"),
            status,
            detail: None,
        }
    }

    #[test]
    fn presentation_hides_widget_when_everything_is_finished() {
        let (status, widget) = presentation(&[item("a", TaskStatus::Completed)]);
        assert!(status.is_none() && widget.is_none());

        let (status, widget) = presentation(&[
            item("a", TaskStatus::Completed),
            item("b", TaskStatus::InProgress),
        ]);
        let status = status.unwrap();
        assert!(status.starts_with("任务 1/2"), "{status}");
        assert!(status.contains("正在执行：任务 b"), "{status}");
        let lines = widget.unwrap();
        assert_eq!(lines[0], "已完成 1/2");
        assert_eq!(lines[1], "✓ 任务 a");
        assert_eq!(lines[2], "● 任务 b");
    }

    #[test]
    fn replace_rejects_duplicate_ids_and_two_in_progress() {
        let duplicate = json!({
            "action": "replace",
            "tasks": [
                { "id": "a", "title": "A", "status": "pending" },
                { "id": "a", "title": "B", "status": "pending" },
            ],
        });
        assert!(parse_tasks(&duplicate).is_err());

        let two_active = json!({
            "action": "replace",
            "tasks": [
                { "id": "a", "title": "A", "status": "in_progress" },
                { "id": "b", "title": "B", "status": "in_progress" },
            ],
        });
        assert!(parse_tasks(&two_active).is_err());
    }

    #[tokio::test]
    async fn update_clears_stale_detail_when_status_changes() {
        let (store, mut events) = store();
        store.replace(vec![TaskItem {
            id: "a".into(),
            title: "A".into(),
            status: TaskStatus::InProgress,
            detail: Some("正在跑测试".into()),
        }]);
        let updated = store
            .update(
                "a",
                TaskPatch {
                    title: None,
                    status: Some(TaskStatus::Completed),
                    detail: None,
                },
            )
            .unwrap();
        assert_eq!(updated[0].detail, None);
        // 每次变更都会向前端推 setStatus + setWidget 两帧
        let first = events.try_recv().expect("setStatus");
        assert_eq!(first["method"], "setStatus");
        let second = events.try_recv().expect("setWidget");
        assert_eq!(second["method"], "setWidget");
    }

    #[test]
    fn advance_moves_cursor_to_first_pending() {
        let mut tasks = vec![
            item("a", TaskStatus::Completed),
            item("b", TaskStatus::Pending),
            item("c", TaskStatus::Pending),
        ];
        advance(&mut tasks);
        assert_eq!(tasks[1].status, TaskStatus::InProgress);
        assert_eq!(tasks[2].status, TaskStatus::Pending);
    }

    #[test]
    fn advance_keeps_model_chosen_cursor() {
        let mut tasks = vec![
            item("a", TaskStatus::Pending),
            item("b", TaskStatus::InProgress),
        ];
        advance(&mut tasks);
        assert_eq!(tasks[0].status, TaskStatus::Pending);
        assert_eq!(tasks[1].status, TaskStatus::InProgress);
    }

    #[test]
    fn advance_stops_on_blocked_or_failed() {
        for exception in [TaskStatus::Blocked, TaskStatus::Failed] {
            let mut tasks = vec![
                item("a", TaskStatus::Completed),
                item("b", exception),
                item("c", TaskStatus::Pending),
            ];
            advance(&mut tasks);
            assert_eq!(tasks[2].status, TaskStatus::Pending, "{exception:?}");
        }
    }

    #[test]
    fn advance_does_nothing_when_everything_is_finished() {
        let mut tasks = vec![
            item("a", TaskStatus::Completed),
            item("b", TaskStatus::Skipped),
        ];
        advance(&mut tasks);
        assert_eq!(tasks[0].status, TaskStatus::Completed);
        assert_eq!(tasks[1].status, TaskStatus::Skipped);
    }

    #[tokio::test]
    async fn completing_a_task_hands_the_cursor_to_the_next_one() {
        let (store, _events) = store();
        store.replace(vec![
            item("a", TaskStatus::InProgress),
            item("b", TaskStatus::Pending),
        ]);
        let updated = store
            .update(
                "a",
                TaskPatch {
                    title: None,
                    status: Some(TaskStatus::Completed),
                    detail: None,
                },
            )
            .unwrap();
        // 模型只说了 a 完成；b 由程序推上来（模型看到的返回文本也是这份）。
        assert_eq!(updated[0].status, TaskStatus::Completed);
        assert_eq!(updated[1].status, TaskStatus::InProgress);
        assert_eq!(store.tasks()[1].status, TaskStatus::InProgress);
        let lines = store.widget_lines().expect("widget");
        assert_eq!(lines[1], "✓ 任务 a");
        assert_eq!(lines[2], "● 任务 b");
    }

    #[tokio::test]
    async fn replace_without_in_progress_promotes_the_first_pending() {
        let (store, _events) = store();
        // 模型常见写法：一步到位把第一条写成 completed，剩下全是 pending。
        store.replace(vec![
            item("a", TaskStatus::Completed),
            item("b", TaskStatus::Pending),
            item("c", TaskStatus::Pending),
        ]);
        let tasks = store.tasks();
        assert_eq!(tasks[1].status, TaskStatus::InProgress);
        let status = store.status_text().expect("status");
        assert!(status.contains("正在执行：任务 b"), "{status}");
    }

    #[tokio::test]
    async fn restore_promotes_the_cursor_too() {
        let (store, _events) = store();
        store.restore(vec![
            item("a", TaskStatus::Completed),
            item("b", TaskStatus::Pending),
        ]);
        assert_eq!(store.tasks()[1].status, TaskStatus::InProgress);
    }

    #[tokio::test]
    async fn execute_reports_the_advanced_list() {
        let (store, _events) = store();
        let tool = create_task_progress_tool(store.clone());
        let params = json!({
            "action": "replace",
            "tasks": [
                { "id": "a", "title": "任务 a", "status": "completed" },
                { "id": "b", "title": "任务 b", "status": "pending" },
            ],
        });
        let result = tool
            .execute("call-1", params, CancellationToken::new(), Arc::new(|_| {}))
            .await
            .unwrap();
        // 模型看到的文本与 details 必须和界面是同一份（都是推进后的），
        // 否则它会以为「下一条还没开始」，下一轮又去重写整张表。
        let TextContentOrImage::Text(text) = &result.content[0] else {
            panic!("expected text content");
        };
        assert!(text.text.contains("● 任务 b"), "{}", text.text);
        assert!(!text.text.contains("○ 任务 b"), "{}", text.text);
        assert_eq!(result.details["tasks"][1]["status"], "in_progress");
    }
}
