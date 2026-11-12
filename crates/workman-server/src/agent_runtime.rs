//! agent 运行时：把 rpi 的 `AgentHarness` 接到 HTTP/SSE 上。
//!
//! 三层：
//! 1. [`LiveSessions`] —— `id → LiveAgent`，一个会话同时只有一个活 agent
//!    （对应 pi 的 `A4(id)` / `OF(id, path)`）。
//! 2. [`LiveAgent`] —— 包住 `AgentHarness` + 一条 lane，把每条 `AgentEvent`
//!    投影成 pi 的 SSE 形状后广播（规则见 `docs/contracts/README.md`）。
//! 3. [`LiveAgent::send`] —— 处理 `POST /api/agent/[id]` 的 `{type:…}` 命令。
//!
//! 会话持久化走 `workman-session` 的 v3 `SessionStorage`，所以一轮对话结束后
//! 消息已经按 v3 写进同一个 `.jsonl` 文件。
//!
//! **注意**：rpi 的 `AgentEvent` 没有实现 `Serialize`（它是给 Rust 消费者的），
//! 所以事件到 JSON 的映射是这里手写的，字段名对齐 pi 的 camelCase。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rpi_agent::events::{AgentEmitter, AgentEvent};
use rpi_agent::message::{assistant_json, AgentMessage};
use rpi_agent::types::AgentToolResult;
use rpi_ai::types::AssistantMessageEvent;
use rpi_harness::agent_harness::{AgentHarness, AgentLane, NavigationOutcome};
use rpi_harness::session::types::{ProvisionedEntry, ProvisionedKind};
use rpi_harness::session::Session;
use rpi_harness::types::{AgentHarnessOptions, SendMessageOptions};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use workman_session::V3SessionStorage;

use crate::catalog::ModelCatalog;
use crate::settings::{QueueMode, QueueModePatch, SettingsFile};

/// 广播缓冲：足够一次长回答的 delta 不被丢弃。
const EVENT_BUFFER: usize = 8192;

/// 系统提示：与 pi 的默认 preamble 对齐。
const SYSTEM_PROMPT: &str = "\
You are a coding assistant running inside pi, a coding agent harness. \
Help users inspect files, run commands, and modify code.";

/// 一个活着的 agent 会话。
pub struct LiveAgent {
    pub id: String,
    pub path: PathBuf,
    pub cwd: String,
    harness: Arc<AgentHarness>,
    lane: Arc<dyn AgentLane>,
    events: broadcast::Sender<Value>,
    running: Arc<AtomicBool>,
    /// 当前模型。`set_model` 会在运行中替换它，所以不是不可变字段。
    model: Mutex<rpi_ai::Model>,
    /// 切模型时按 `provider/modelId` 查 `rpi_ai::Model`。
    ///
    /// 活会话比一次请求活得久，不能临时从 `AppState` 借引用。
    catalog: ModelCatalog,
    /// 活会话表的变化通知（`insert` 之后才挂上）
    changes: Mutex<Option<broadcast::Sender<()>>>,
    /// `settings.json`：`set_steering_mode` / `set_follow_up_mode` 要落盘。
    ///
    /// 不能借用 `AppState`（活会话比一次请求活得久），所以拿一份落定的路径句柄。
    settings: SettingsFile,
}

impl LiveAgent {
    /// 打开已有会话文件并建 agent。
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        id: &str,
        path: &Path,
        cwd: &str,
        catalog: &ModelCatalog,
        preferred: Option<(String, String)>,
        settings: &SettingsFile,
    ) -> Result<Arc<Self>> {
        let storage = V3SessionStorage::load(path)
            .with_context(|| format!("加载会话 {} 失败", path.display()))?;
        Self::start_with_storage(id, path, storage, cwd, catalog, preferred, settings).await
    }

    /// 同上，但直接用现成的 `V3SessionStorage`（新建会话时省一次重读）。
    #[allow(clippy::too_many_arguments)]
    pub async fn start_with_storage(
        id: &str,
        path: &Path,
        storage: V3SessionStorage,
        cwd: &str,
        catalog: &ModelCatalog,
        preferred: Option<(String, String)>,
        settings: &SettingsFile,
    ) -> Result<Arc<Self>> {
        let session = Session::new(Arc::new(storage), None);

        // 优先用「设置里选的默认模型」，其次退回 catalog 的默认。
        //
        // 早先只用了 `catalog.default_model()`（models.json 里第一个可用项），
        // 于是用户在任何界面选过的默认模型都不生效 —— 续聊会用错模型。
        let model = preferred
            .as_ref()
            .and_then(|(provider, id)| catalog.model_for(provider, id))
            .or_else(|| catalog.default_model())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("models.json 里没有可用模型"))?;
        let tools = crate::tools::build_tools(cwd, &model);
        let queue_modes = settings.queue_modes();

        let (events, _rx) = broadcast::channel(EVENT_BUFFER);
        let tap = Arc::new(EventTap {
            events: events.clone(),
        });

        let options = AgentHarnessOptions {
            model: model.clone(),
            thinking_level: rpi_ai::types::ThinkingLevel::Off,
            active_tool_names: tools.iter().map(|(name, _)| name.clone()).collect(),
            tools: tools.into_iter().map(|(_, tool)| tool).collect(),
            system_prompt: Some(SYSTEM_PROMPT.to_string()),
            session,
            allow_existing_session: true,
            models: catalog.providers.clone(),
            agent_emitter: Some(tap),
            // 队列模式在 `AgentHarness::create` 时定下来（rpi 没有运行期 setter），
            // 所以这里每次开会话都重新读一遍 `settings.json`：用户在设置页改完，
            // 新开的会话立刻按新模式投递。
            steering_mode: queue_modes.steering_mode.into(),
            follow_up_mode: queue_modes.follow_up_mode.into(),
            ..Default::default()
        };

        let harness = Arc::new(AgentHarness::create(options).await?);
        let lane = harness.lane("main");

        Ok(Arc::new(Self {
            id: id.to_string(),
            path: path.to_path_buf(),
            cwd: cwd.to_string(),
            harness,
            lane,
            events,
            running: Arc::new(AtomicBool::new(false)),
            model: Mutex::new(model),
            catalog: catalog.clone(),
            changes: Mutex::new(None),
            settings: settings.clone(),
        }))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }

    /// 把「活会话集合变化」的通知通道接上（由 `LiveSessions::insert` 调用）。
    fn attach_changes(&self, sender: broadcast::Sender<()>) {
        *self.changes.lock().expect("changes") = Some(sender);
    }

    /// 运行状态变了 → 让 `/api/agent/running/events` 推一帧。
    fn notify_running_changed(&self) {
        if let Some(sender) = self.changes.lock().expect("changes").as_ref() {
            let _ = sender.send(());
        }
    }

    pub fn is_streaming(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// `{"type":…}` 命令（pi 的 `session.send(command)`）。
    ///
    /// `prompt` 会在后台跑整轮、立刻返回 `accepted`，这样 HTTP 连接不被占住，
    /// 前端照常用 `/events` 的 SSE 收流。
    pub async fn send(self: &Arc<Self>, command: &Value) -> Result<Value> {
        let ty = command
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();

        match ty {
            "get_state" => self.state_json().await,
            "prompt" => {
                let message = prompt_message(command)?;
                let this = Arc::clone(self);
                this.running.store(true, Ordering::Relaxed);
                tokio::spawn(async move {
                    let result = this
                        .lane
                        .send_message(
                            message,
                            SendMessageOptions {
                                trigger_turn: Some(true),
                                ..Default::default()
                            },
                        )
                        .await;
                    this.running.store(false, Ordering::Relaxed);
                    this.notify_running_changed();
                    tracing::info!(
                        "prompt 结束：session={} ok={} err={:?}",
                        this.id,
                        result.is_ok(),
                        result.as_ref().err().map(|e| e.to_string())
                    );
                    if let Err(e) = result {
                        let _ = this.events.send(json!({
                            "type": "run_error",
                            "errorMessage": e.to_string(),
                        }));
                        let _ = this.events.send(json!({ "type": "agent_end" }));
                    }
                });
                Ok(json!({ "accepted": true }))
            }
            "steer" => {
                let message = prompt_message(command)?;
                self.lane.steer(message).await?;
                Ok(json!({ "accepted": true }))
            }
            "follow_up" => {
                let message = prompt_message(command)?;
                self.lane.follow_up(message).await?;
                Ok(json!({ "accepted": true }))
            }
            "abort" => {
                self.running.store(false, Ordering::Relaxed);
                let _ = self.lane.abort().await;
                self.notify_running_changed();
                Ok(json!({ "accepted": true }))
            }
            "set_model" => self.set_model(command).await,
            "set_steering_mode" => self.set_queue_mode(command, true).await,
            "set_follow_up_mode" => self.set_queue_mode(command, false).await,
            "navigate_tree" => self.navigate_tree(command).await,
            other => Err(anyhow::anyhow!("不支持的 agent 命令：{other}")),
        }
    }

    /// `{"type":"set_steering_mode"|"set_follow_up_mode","mode":…}`。
    ///
    /// # 与 pi 的差异：只落盘，不改当前会话
    ///
    /// pi 的这两个命令直接改 `AgentSession` 内存里的队列模式，当前会话立刻生效。
    /// rpi 的队列模式在 `AgentHarness::create` 时就定下来了（`AgentHarnessOptions`
    /// 的 `steering_mode` / `follow_up_mode`），没有运行期 setter；重建 harness 又会
    /// 把正在跑的会话拆掉（丢流、丢正在执行的工具）。所以这里只能写
    /// `settings.json`，与 `PUT /api/queue-modes` 同语义：**对之后新建的会话生效**。
    ///
    /// 不报错是故意的：前端保存完接着就调这两个命令，报错会在设置页弹一行红字，
    /// 而用户要的结果（模式已保存）实际上已经达成了。
    async fn set_queue_mode(&self, command: &Value, steering: bool) -> Result<Value> {
        let raw = command
            .get("mode")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("缺少 mode"))?;
        let mode = QueueMode::parse(raw)
            .ok_or_else(|| anyhow::anyhow!("mode 只能是 all 或 one-at-a-time"))?;
        let patch = if steering {
            QueueModePatch {
                steering_mode: Some(mode),
                follow_up_mode: None,
            }
        } else {
            QueueModePatch {
                steering_mode: None,
                follow_up_mode: Some(mode),
            }
        };
        let modes = self.settings.set_queue_modes(patch)?;
        Ok(json!({
            "accepted": true,
            // 当前会话仍是旧模式：rpi 没有运行期 setter。
            "applied": false,
            "note": "rpi 的队列模式在会话创建时确定，新会话生效",
            "steeringMode": modes.steering_mode.as_str(),
            "followUpMode": modes.follow_up_mode.as_str(),
        }))
    }

    /// `{"type":"set_model","provider":…,"modelId":…}`。
    ///
    /// pi 的 `setModel` 会往会话文件追加一条 `model_change`，前端切完立刻
    /// `loadSession` 读回的 `context.model` 就是它。只改内存不落盘的话，界面
    /// 会在切换成功的下一秒弹回旧模型 —— 看起来像「切了但没生效」。
    async fn set_model(&self, command: &Value) -> Result<Value> {
        let provider = command
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let model_id = command
            .get("modelId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let model = self
            .catalog
            .model_for(provider, model_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Model not found: {provider}/{model_id}"))?;

        let session = self.harness.session();
        let entry = ProvisionedEntry {
            id: session.id_generator().next(),
            kind: ProvisionedKind::ModelChange {
                provider: model.provider.as_str().to_string(),
                model_id: model.id.clone(),
            },
        };
        session
            .append_entry(entry, "main")
            .await
            .context("写 model_change 失败")?;

        self.lane.set_model(model.clone()).await?;
        *self.model.lock().expect("model") = model.clone();

        Ok(json!({ "id": model.id, "provider": model.provider.as_str() }))
    }

    /// `{"type":"navigate_tree","targetId":…}` → `{cancelled, leafId}`。
    ///
    /// 语义与 pi 的 `navigateTree(targetId)` 一致：把 main lane 的叶子**移到**
    /// 目标条目上（不是它的父节点），于是目标消息仍在分支里，下一条消息成为
    /// 它的子节点 —— 这就是「从此处编辑」在同一会话里开分支的做法。
    ///
    /// 前端的三个入口都发这一条命令：消息上的「从此处编辑」、消息树点节点、
    /// 分支下拉切换叶子。返回的 `leafId` 会被原样带去
    /// `GET /api/sessions/{id}/context?leafId=…`，所以这里必须是**导航后**的叶子。
    ///
    /// v3 文件里没有 leaf 记录（pi 也一样）：只挪内存指针，重开文件时叶子
    /// 退回最后一条条目。真按新分支续聊时新条目会带着 `parentId` 落盘，
    /// 那一行自己就成了叶子，行为与 pi 相同。
    async fn navigate_tree(&self, command: &Value) -> Result<Value> {
        let target = command
            .get("targetId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("navigate_tree 缺少 targetId"))?;

        let result = self
            .lane
            .navigate_tree(Some(target), false, None, None)
            .await?;
        match result.outcome {
            NavigationOutcome::Completed { new_leaf_id, .. } => Ok(json!({
                "cancelled": false,
                "leafId": new_leaf_id,
            })),
            NavigationOutcome::Declined { leaf_id } | NavigationOutcome::Aborted { leaf_id } => {
                Ok(json!({ "cancelled": true, "leafId": leaf_id }))
            }
            NavigationOutcome::Failed { leaf_id, error } => Err(anyhow::anyhow!(
                "导航到 {target} 失败（叶子仍是 {}）：{}",
                leaf_id.as_deref().unwrap_or("null"),
                error.message
            )),
        }
    }

    /// `get_state` 形状。`/api/agent/[id]`、`/api/mobile/state` 都用它。
    pub async fn state_json(&self) -> Result<Value> {
        let (steering, follow_up) = match self.lane.queued_messages().await {
            Ok(queued) => (queued.steering.len(), queued.follow_up.len()),
            Err(_) => (0, 0),
        };
        let model = self.model.lock().expect("model").clone();
        Ok(json!({
            "isStreaming": self.is_streaming(),
            "queuedMessages": { "steering": steering, "followUp": follow_up },
            "sessionId": self.id,
            "cwd": self.cwd,
            "model": {
                "provider": model.provider.as_str(),
                "id": model.id,
                "name": model.name,
            },
            "thinkingLevel": "off",
        }))
    }

    pub async fn shutdown(&self) {
        self.running.store(false, Ordering::Relaxed);
        let _ = self.lane.abort().await;
    }
}

/// `{message:"文本"}` / `{message:{…}}` / `{message:[blocks]}` → `AgentMessage`。
fn prompt_message(command: &Value) -> Result<AgentMessage> {
    let now = now_ms();
    match command.get("message") {
        Some(Value::String(text)) => Ok(AgentMessage::User(rpi_ai::types::UserMessage::new(
            text.clone(),
            now,
        ))),
        Some(Value::Array(blocks)) => {
            let content: Vec<rpi_ai::types::Content> =
                serde_json::from_value(Value::Array(blocks.clone()))
                    .context("message 不是合法的内容块数组")?;
            Ok(AgentMessage::User(rpi_ai::types::UserMessage::new(
                rpi_ai::types::UserContent::Blocks(content),
                now,
            )))
        }
        Some(Value::Object(_)) => Ok(serde_json::from_value(command["message"].clone())
            .context("message 不是合法的 AgentMessage")?),
        _ => Err(anyhow::anyhow!("prompt 缺少 message")),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 事件投影：AgentEvent → pi 的 SSE JSON
// ---------------------------------------------------------------------------

/// 把 `AgentEvent` 投成广播用的 JSON 的 emitter。
pub struct EventTap {
    events: broadcast::Sender<Value>,
}

impl AgentEmitter for EventTap {
    fn emit(&self, event: AgentEvent) -> futures::future::BoxFuture<'static, ()> {
        let events = self.events.clone();
        Box::pin(async move {
            if let Some(payload) = project_event(&event) {
                let _ = events.send(payload);
            }
        })
    }

    fn try_emit(&self, event: AgentEvent) {
        if let Some(payload) = project_event(&event) {
            let _ = self.events.send(payload);
        }
    }
}

/// 复刻 pi `29859.q()` 的裁剪：丢 turn 边界、丢 system 消息、
/// `message_update` 去掉 partial、`tool_execution_update` 只留三字段。
pub fn project_event(event: &AgentEvent) -> Option<Value> {
    match event {
        AgentEvent::TurnStart | AgentEvent::TurnEnd { .. } => None,
        AgentEvent::AgentStart => Some(json!({ "type": "agent_start" })),
        AgentEvent::AgentEnd { .. } => Some(json!({ "type": "agent_end" })),
        AgentEvent::RetryScheduled {
            attempt,
            max_retries,
            delay_ms,
            error,
        } => Some(json!({
            "type": "retry_scheduled",
            "attempt": attempt,
            "maxRetries": max_retries,
            "delayMs": delay_ms,
            "error": error,
        })),
        AgentEvent::MessageStart {
            message,
            message_id,
        } => {
            if is_system(message) {
                return None;
            }
            let mut out = json!({
                "type": "message_start",
                "message": message_json(message),
            });
            if let Some(id) = message_id {
                out["messageId"] = json!(id);
            }
            Some(out)
        }
        AgentEvent::MessageEnd {
            message,
            message_id,
        } => {
            if is_system(message) {
                return None;
            }
            let mut out = json!({
                "type": "message_end",
                "message": message_json(message),
            });
            if let Some(id) = message_id {
                out["messageId"] = json!(id);
            }
            Some(out)
        }
        AgentEvent::MessageUpdate {
            message_id,
            message,
            assistant_message_event,
        } => {
            let mut out = json!({
                "type": "message_update",
                "message": assistant_json(message),
                "assistantMessageEvent": assistant_event_json(assistant_message_event),
            });
            if let Some(id) = message_id {
                out["messageId"] = json!(id);
            }
            Some(out)
        }
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => Some(json!({
            "type": "tool_execution_start",
            "toolCallId": tool_call_id,
            "toolName": tool_name,
            "args": args,
        })),
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            tool_name,
            partial_result,
            ..
        } => Some(json!({
            "type": "tool_execution_update",
            "toolCallId": tool_call_id,
            "toolName": tool_name,
            "partialResult": tool_result_json(partial_result),
        })),
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
            is_error,
        } => Some(json!({
            "type": "tool_execution_end",
            "toolCallId": tool_call_id,
            "toolName": tool_name,
            "result": tool_result_json(result),
            "isError": is_error,
        })),
    }
}

fn is_system(message: &AgentMessage) -> bool {
    matches!(message.role().as_str(), "system")
}

fn message_json(message: &AgentMessage) -> Value {
    serde_json::to_value(message).unwrap_or(Value::Null)
}

fn tool_result_json(result: &AgentToolResult) -> Value {
    serde_json::to_value(result).unwrap_or(Value::Null)
}

/// `AssistantMessageEvent` → pi 的 JSON（内层同样去掉 `partial`）。
fn assistant_event_json(event: &AssistantMessageEvent) -> Value {
    match event {
        AssistantMessageEvent::Start { .. } => json!({ "type": "start" }),
        AssistantMessageEvent::TextStart { content_index, .. } => json!({
            "type": "text_start",
            "contentIndex": content_index,
        }),
        AssistantMessageEvent::TextDelta {
            content_index,
            delta,
            ..
        } => json!({
            "type": "text_delta",
            "contentIndex": content_index,
            "delta": delta,
        }),
        AssistantMessageEvent::TextEnd {
            content_index,
            content,
            ..
        } => json!({
            "type": "text_end",
            "contentIndex": content_index,
            "content": content,
        }),
        AssistantMessageEvent::ThinkingStart { content_index, .. } => json!({
            "type": "thinking_start",
            "contentIndex": content_index,
        }),
        AssistantMessageEvent::ThinkingDelta {
            content_index,
            delta,
            ..
        } => json!({
            "type": "thinking_delta",
            "contentIndex": content_index,
            "delta": delta,
        }),
        AssistantMessageEvent::ThinkingEnd {
            content_index,
            content,
            ..
        } => json!({
            "type": "thinking_end",
            "contentIndex": content_index,
            "content": content,
        }),
        AssistantMessageEvent::ToolCallStart { content_index, .. } => json!({
            "type": "toolcall_start",
            "contentIndex": content_index,
        }),
        AssistantMessageEvent::ToolCallDelta {
            content_index,
            delta,
            ..
        } => json!({
            "type": "toolcall_delta",
            "contentIndex": content_index,
            "delta": delta,
        }),
        AssistantMessageEvent::ToolCallEnd {
            content_index,
            tool_call,
            ..
        } => json!({
            "type": "toolcall_end",
            "contentIndex": content_index,
            "toolCall": serde_json::to_value(tool_call).unwrap_or(Value::Null),
        }),
        AssistantMessageEvent::Done { reason, message } => json!({
            "type": "done",
            "reason": reason.as_str(),
            "message": serde_json::to_value(message).unwrap_or(Value::Null),
        }),
        AssistantMessageEvent::Error { reason, error } => json!({
            "type": "error",
            "reason": error_reason_str(*reason),
            "error": serde_json::to_value(error).unwrap_or(Value::Null),
        }),
    }
}

fn error_reason_str(reason: rpi_ai::types::ErrorReason) -> &'static str {
    use rpi_ai::types::ErrorReason::*;
    match reason {
        Aborted => "aborted",
        Error => "error",
    }
}

// ---------------------------------------------------------------------------
// 活会话表
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct LiveSessions {
    map: Arc<Mutex<HashMap<String, Arc<LiveAgent>>>>,
    /// 正在建 agent 的会话（避免并发重复建）
    starting: Arc<Mutex<std::collections::HashSet<String>>>,
    /// 活会话集合 / 运行状态变化（`/api/agent/running/events` 用）
    changed: broadcast::Sender<()>,
}

impl Default for LiveSessions {
    fn default() -> Self {
        Self {
            map: Arc::new(Mutex::new(HashMap::new())),
            starting: Arc::new(Mutex::new(std::collections::HashSet::new())),
            changed: broadcast::channel(64).0,
        }
    }
}

impl LiveSessions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, id: &str) -> Option<Arc<LiveAgent>> {
        self.map.lock().expect("live").get(id).cloned()
    }

    pub fn insert(&self, agent: Arc<LiveAgent>) {
        agent.attach_changes(self.changed.clone());
        self.map
            .lock()
            .expect("live")
            .insert(agent.id.clone(), Arc::clone(&agent));
        self.notify();
    }

    pub async fn remove(&self, id: &str) -> Option<Arc<LiveAgent>> {
        let removed = self.map.lock().expect("live").remove(id);
        if let Some(agent) = &removed {
            agent.shutdown().await;
        }
        self.notify();
        removed
    }

    /// 订阅「活会话变化」（对应 pi 的 `56429.V()`）。
    pub fn subscribe_changes(&self) -> broadcast::Receiver<()> {
        self.changed.subscribe()
    }

    /// 主动触发一次变化通知。
    pub fn notify(&self) {
        let _ = self.changed.send(());
    }

    /// 当前运行快照（pi 的 `56429.am()`）。
    ///
    /// `pendingUiSessionIds` 恒为空：workman 没有「等待用户输入的 UI 请求」
    /// 这条链路（阶段 3 的说明里已记录）。
    pub fn running_snapshot(&self) -> Value {
        json!({
            "runningSessionIds": self.running(),
            "pendingUiSessionIds": Vec::<String>::new(),
        })
    }

    /// 正在运行的会话所在目录（pi 的 `56429._I`）。
    pub fn running_cwds(&self) -> Vec<String> {
        self.map
            .lock()
            .expect("live")
            .values()
            .filter(|a| a.is_streaming())
            .map(|a| a.cwd.clone())
            .collect()
    }

    /// 正在运行的会话 id（pi 的 `HG()`），用于 mobile 状态判定。
    pub fn running(&self) -> Vec<String> {
        self.map
            .lock()
            .expect("live")
            .values()
            .filter(|a| a.is_streaming())
            .map(|a| a.id.clone())
            .collect()
    }

    pub fn guard_starting(&self, id: &str) -> bool {
        self.starting
            .lock()
            .expect("starting")
            .insert(id.to_string())
    }

    pub fn clear_starting(&self, id: &str) {
        self.starting.lock().expect("starting").remove(id);
    }
}
