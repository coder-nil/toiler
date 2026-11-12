//! agent 运行时：把 rpi 的 `AgentHarness` 接到 HTTP/SSE 上。
//!
//! 三层：
//! 1. [`LiveSessions`] —— `id → LiveAgent`，一个会话同时只有一个活 agent
//!    （对应 pi 的 `A4(id)` / `OF(id, path)`）。
//! 2. [`LiveAgent`] —— 包住 `AgentHarness` + 一条 lane，把每条 `AgentEvent`
//!    投影成 pi 的 SSE 形状后广播（规则见 `docs/contracts/README.md`）。
//! 3. [`LiveAgent::send`] —— 处理 `POST /api/agent/[id]` 的 `{type:…}` 命令。
//!
//! 会话持久化走 `toiler-session` 的 v3 `SessionStorage`，所以一轮对话结束后
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
use rpi_harness::agent_harness::{
    AgentHarness, AgentLane, NavigationOutcome, QueuedMessages, SendMessageResult,
};
use rpi_harness::session::types::{ProvisionedEntry, ProvisionedKind};
use rpi_harness::session::Session;
use rpi_harness::types::{AgentHarnessOptions, MessageDelivery, SendMessageOptions};
use serde_json::{json, Value};
use toiler_session::V3SessionStorage;
use tokio::sync::broadcast;

use crate::catalog::ModelCatalog;
use crate::desktop_tools::task_progress::TaskItem;
use crate::desktop_tools::{TaskProgressStore, UiBridge};
use crate::settings::{QueueMode, QueueModePatch, SettingsFile};
use crate::system_prompt::{Locale, PromptInputs};

/// 广播缓冲：足够一次长回答的 delta 不被丢弃。
const EVENT_BUFFER: usize = 8192;

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
    /// 系统提示词的原料（cwd / 工具清单 / AGENTS.md）。
    ///
    /// `set_ui_locale` / `set_tools` 都要按新状态重新组装，所以留下原料而不是只留成品：
    /// `<project_context>` 的形态（路径列表还是全文）与 `<tools>` 段都取决于这些原料。
    prompt_inputs: Mutex<PromptInputs>,
    /// 界面语言：决定提示词文案与末尾的语言指令。
    locale: Mutex<Locale>,
    /// 当前生效的系统提示词。
    /// `get_state.systemPrompt` 必须与实际请求逐字相同（pi 的 `effectiveSystemPrompt()`
    /// 注释里写得很清楚：面板显示得等于真正发出去的那份），所以放在这里由
    /// [`Self::set_ui_locale`] 和组装点统一维护。
    system_prompt: Mutex<String>,
    /// 工具 ↔ 前端的通道（`ask_user` 的问答往返、`task_progress` 的状态推送）。
    ui: Arc<UiBridge>,
    /// 任务列表：既喂工具，也喂 `get_state.extensionStatuses/extensionWidgets`。
    tasks: Arc<TaskProgressStore>,
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
        let (events, _rx) = broadcast::channel(EVENT_BUFFER);
        let tap = Arc::new(EventTap {
            events: events.clone(),
        });

        // 桌面自带工具的通道与状态：`ask_user` 的问答往返、`task_progress` 的展示。
        let ui = UiBridge::new(events.clone());
        let tasks = TaskProgressStore::new(ui.clone());
        let tools = crate::tools::build_tools(cwd, &model, ui.clone(), tasks.clone());
        let tool_names: Vec<String> = tools.iter().map(|(name, _)| name.clone()).collect();
        // 任务列表从 transcript 恢复：pi 在 `session_start` 时做同一件事。
        tasks.restore(restore_tasks(path, None));
        let queue_modes = settings.queue_modes();

        // 系统提示词：preamble / tools / rules / project_context / cwd + 语言指令。
        //
        // AGENTS.md 在这里读一次：它是会话级常量，不必每轮重新发现。全局那一份
        // 从 toiler 自己的 agent 目录取（对应 pi 的 `~/.pi/agent`）。
        let prompt_inputs =
            PromptInputs::load(cwd, &tool_names, &crate::paths::toiler_home()).await; // 默认英文：pi 的 `normalizeUiLocale` 对未知值也给 en。桌面前端打开会话后
                                                                                      // 会发 `set_ui_locale`（它记得用户选的界面语言），这里随即换成中文文案。
        let locale = Locale::En;
        let system_prompt = prompt_inputs.compose(locale);

        let options = AgentHarnessOptions {
            model: model.clone(),
            thinking_level: rpi_ai::types::ThinkingLevel::Off,
            active_tool_names: tool_names,
            tools: tools.into_iter().map(|(_, tool)| tool).collect(),
            system_prompt: Some(system_prompt.clone()),
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
            prompt_inputs: Mutex::new(prompt_inputs),
            locale: Mutex::new(locale),
            system_prompt: Mutex::new(system_prompt),
            ui,
            tasks,
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
                // pi 的 `prompt` 在运行中按 `streamingBehavior` 决定投递方式：
                // `steer` 在工具批处理之后注入，`followUp` 等本轮跑完再投递。
                // 不映射的话运行中的 follow-up 会被当成 steer 提前塞进去 ——
                // 排队顺序与语义都不对（见 `docs/contracts/README.md`）。
                let deliver_as = streaming_delivery(command);
                let this = Arc::clone(self);
                this.running.store(true, Ordering::Relaxed);
                tokio::spawn(async move {
                    let result = this
                        .lane
                        .send_message(
                            message,
                            SendMessageOptions {
                                trigger_turn: Some(true),
                                deliver_as,
                            },
                        )
                        .await;
                    let queued = matches!(result, Ok(SendMessageResult::Queued { .. }));
                    // 运行中排队时这一轮**没有**开起来：此时清 running 会让
                    // `GET /api/agent/{id}` 报 idle，前端连排队条一起清空。
                    if !queued {
                        this.running.store(false, Ordering::Relaxed);
                        this.notify_running_changed();
                    }
                    // 入队 / 出队（本轮结束）都推一帧，前端排队条才是实时的。
                    this.broadcast_queue_update().await;
                    tracing::info!(
                        "prompt 结束：session={} queued={} ok={} err={:?}",
                        this.id,
                        queued,
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
                self.broadcast_queue_update().await;
                Ok(json!({ "accepted": true }))
            }
            "follow_up" => {
                let message = prompt_message(command)?;
                self.lane.follow_up(message).await?;
                self.broadcast_queue_update().await;
                Ok(json!({ "accepted": true }))
            }
            // 前端的「撤回排队」按钮（`handleRecallQueue`）：清空两条队列并把
            // **被清掉的文本**还回去，前端拿它回填输入框。pi 的 `clearQueue()`
            // 同义；不实现的话按钮会拿到 500，提示「Failed to recall」。
            "clear_queue" => {
                let cleared = self.lane.clear_queue().await?;
                let steering = texts_of(&cleared).0;
                let follow_up = texts_of(&cleared).1;
                self.broadcast_queue_update().await;
                Ok(json!({ "steering": steering, "followUp": follow_up }))
            }
            "abort" => {
                self.running.store(false, Ordering::Relaxed);
                let _ = self.lane.abort().await;
                self.notify_running_changed();
                // abort 会把队列里的 steer/follow-up 回收掉，排队条得跟着清。
                self.broadcast_queue_update().await;
                Ok(json!({ "accepted": true }))
            }
            "set_model" => self.set_model(command).await,
            // 前端每次打开会话、切换界面语言都会发它，并直接用返回值刷新
            // 「系统提示词」面板；不实现的话面板永远停在「尚未加载」。
            "set_ui_locale" => self.set_ui_locale(command).await,
            // 工具预设（参数面板 / 设置页）读 `get_tools`、改 `set_tools`。
            // 少了前者，`getPresetFromTools` 拿不到东西，工具列表就是空的。
            "get_tools" => self.tool_infos().await,
            "set_tools" => self.set_tools(command).await,
            // `ask_user` 的问答往返：前端弹窗回答后发回来。请求已作废（超时/中止）
            // 时不算错误 —— 前端只是晚了一步。
            "extension_ui_response" => {
                self.ui.respond(command);
                Ok(Value::Null)
            }
            // custom UI 的逐键输入；toiler 不提供 custom 渲染，收到就忽略。
            "extension_ui_input" => Ok(Value::Null),
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

    /// `{"type":"navigate_tree","targetId":…,"hideTarget":bool}` → `{cancelled, leafId}`。
    ///
    /// 默认语义与 pi 的 `navigateTree(targetId)` 一致：把 main lane 的叶子**移到**
    /// 目标条目上（不是它的父节点），于是目标消息仍在分支里，下一条消息成为
    /// 它的子节点。
    ///
    /// `hideTarget: true` 是「从此处编辑」专用的：用户要把这条消息（连同它之后的
    /// 内容）从对话里**消失**，改完重发。所以叶子退到它的**父节点**，目标条目
    /// 自己也不在分支上；重发的新消息挂在父节点下，原消息留在旧分支里。
    /// 目标条目的父节点是 `null`（改的是第一条消息）时叶子被移到「无」，
    /// 前端据此清空对话区。
    ///
    /// 前端的三个入口都发这一条命令：消息上的「从此处编辑」（带 `hideTarget`）、
    /// 消息树点节点、分支下拉切换叶子。返回的 `leafId` 会被原样带去
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

        let hide_target = command
            .get("hideTarget")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // `None` 也要真的导航（回到「没有叶子」）：不能回退成「保持当前叶子」，
        // 否则编辑第一条消息时对话区会原样留在最后一条上。
        let landing = if hide_target {
            let entry = self
                .harness
                .session()
                .get_entry(target)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Entry {target} not found"))?;
            entry.base().parent_id.clone()
        } else {
            Some(target.to_string())
        };

        let result = self
            .lane
            .navigate_tree(landing.as_deref(), false, None, None)
            .await?;
        match result.outcome {
            NavigationOutcome::Completed { new_leaf_id, .. } => {
                // 切分支后任务列表跟着分支走（pi 在 `session_tree` 事件里重建）。
                self.tasks
                    .restore(restore_tasks(&self.path, new_leaf_id.as_deref()));
                Ok(json!({
                    "cancelled": false,
                    "leafId": new_leaf_id,
                }))
            }
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

    /// 排队消息的文本快照（steering, follow-up）。
    ///
    /// 前端 `queuedMessages` 要的是**文本数组**（排队条逐条渲染，撤回按钮还要
    /// 把它们回填输入框），不是数量。早先这里发的是 `usize`，前端 `?.length`
    /// 恒为 `undefined` → 排队条永远不出现，看起来就是「排队没生效」。
    async fn queued_texts(&self) -> (Vec<String>, Vec<String>) {
        match self.lane.queued_messages().await {
            Ok(queued) => texts_of(&queued),
            Err(_) => (Vec::new(), Vec::new()),
        }
    }

    /// 排队条数：`/api/mobile/state.queue` 是数量（手机端只显示 `N queued`）。
    pub async fn queued_counts(&self) -> (usize, usize) {
        let (steering, follow_up) = self.queued_texts().await;
        (steering.len(), follow_up.len())
    }

    /// 推一帧 `queue_update`：前端 `ChatInput` 的排队条与手机端都读它。
    async fn broadcast_queue_update(&self) {
        let (steering, follow_up) = self.queued_texts().await;
        let _ = self.events.send(json!({
            "type": "queue_update",
            "steering": steering,
            "followUp": follow_up,
        }));
    }

    /// `get_state` 形状。`/api/agent/[id]`、`/api/agent/new` 都用它。
    ///
    /// 注意 `queuedMessages` 是文本数组，`/api/mobile/state.queue` 是数量 ——
    /// 两个消费方要的东西不同，不要再从同一份 `queuedMessages` 里取。
    pub async fn state_json(&self) -> Result<Value> {
        let (steering, follow_up) = self.queued_texts().await;
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
            // pi 的面板读的就是这一项；缺了它会显示「系统提示词尚未加载」。
            "systemPrompt": self.current_system_prompt(),
            // 任务进度与未回答的 UI 请求：前端靠这三项恢复界面状态。
            "extensionStatuses": self.tasks.status_text().map_or_else(
                || json!([]),
                |text| json!([{ "key": crate::desktop_tools::task_progress::STATUS_KEY, "text": text }]),
            ),
            "extensionWidgets": self.tasks.widget_lines().map_or_else(
                || json!([]),
                |lines| json!([{
                    "key": crate::desktop_tools::task_progress::WIDGET_KEY,
                    "lines": lines,
                    "placement": "aboveEditor",
                }]),
            ),
            "pendingUiRequests": self.ui.pending_requests(),
        }))
    }

    /// 有未回答的 UI 请求（`ask_user` 正卡在提问上）。
    pub fn has_pending_ui_request(&self) -> bool {
        self.ui.has_pending()
    }

    /// 未回答的 UI 请求（手机端 `mobile/state.pendingUiRequest` 用）。
    pub fn pending_ui_requests(&self) -> Vec<Value> {
        self.ui.pending_requests()
    }

    /// 当前生效的系统提示词。
    fn current_system_prompt(&self) -> String {
        self.system_prompt.lock().expect("system prompt").clone()
    }

    /// 按当前原料与界面语言重算提示词并缓存，返回新文本。
    ///
    /// 锁顺序固定为 locale → prompt_inputs → system_prompt，所有调用点共用这一个
    /// 函数就不会互相等到对方手里的锁。
    fn recompose(&self) -> String {
        let locale = *self.locale.lock().expect("locale");
        let prompt = self
            .prompt_inputs
            .lock()
            .expect("prompt inputs")
            .compose(locale);
        *self.system_prompt.lock().expect("system prompt") = prompt.clone();
        prompt
    }

    /// `{"type":"get_tools"}` → `[{name, description, active}]`。
    ///
    /// pi 的面板用它反推工具预设（`getPresetFromTools`）；`active` 必须跟着
    /// `set_tools` 变，否则切换预设后界面又跳回旧值。
    async fn tool_infos(&self) -> Result<Value> {
        let tools = self.harness.get_tools().await?;
        let active = self
            .prompt_inputs
            .lock()
            .expect("prompt inputs")
            .tool_names()
            .to_vec();
        Ok(Value::Array(
            tools
                .iter()
                .map(|tool| {
                    let schema = tool.tool.schema();
                    json!({
                        "name": schema.name,
                        "description": schema.description,
                        "active": active.contains(&schema.name),
                    })
                })
                .collect(),
        ))
    }

    /// `{"type":"set_tools","toolNames":[…]}` → `null`（与 pi 同形）。
    ///
    /// 空数组就是「全关」（pi 的工具预设 `none` / 普通对话模式）：此时提示词只剩
    /// preamble、通用准则与 cwd。注册表本身不删工具 —— rpi 的 `set_tools` 会整张
    /// 替换，所以把原有工具原样传回去，只换 active 集合。
    async fn set_tools(&self, command: &Value) -> Result<Value> {
        let requested: Vec<String> = command
            .get("toolNames")
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let tools = self.harness.get_tools().await?;
        // 顺序沿用注册表（即 pi 的默认工具顺序），前端传的顺序不影响提示词。
        // 桌面工具（`system_time` / `task_progress` / `ask_user`）不受预设控制：
        // pi 的 `set_tools` 用 `withExtensionTools` 把它们加回激活集合。
        let ordered: Vec<String> = tools
            .iter()
            .map(|tool| tool.tool.schema().name.clone())
            .filter(|name| {
                requested.contains(name) || crate::tools::ALWAYS_ON_TOOLS.contains(&name.as_str())
            })
            .collect();
        self.harness.set_tools(tools, Some(ordered.clone())).await?;

        self.prompt_inputs
            .lock()
            .expect("prompt inputs")
            .set_tool_names(ordered);
        let prompt = self.recompose();
        self.harness.set_system_prompt(Some(prompt)).await?;
        Ok(Value::Null)
    }

    /// `{"type":"set_ui_locale","locale":"zh-CN"}` → `{systemPrompt}`。
    ///
    /// 界面语言决定提示词的中英文文案，所以换语言要重新组装整段提示词并把
    /// 它推回 harness。rpi 的 `set_system_prompt` 在下一轮 run 的快照里生效
    /// （进行中的一轮仍用旧文案），这与 pi 的 `before_agent_start` 时机一致。
    async fn set_ui_locale(&self, command: &Value) -> Result<Value> {
        let locale = Locale::parse(command.get("locale").and_then(Value::as_str));
        *self.locale.lock().expect("locale") = locale;
        let prompt = self.recompose();
        self.harness.set_system_prompt(Some(prompt.clone())).await?;
        Ok(json!({ "systemPrompt": prompt }))
    }

    pub async fn shutdown(&self) {
        self.running.store(false, Ordering::Relaxed);
        let _ = self.lane.abort().await;
    }
}

/// 从 v3 文件里恢复任务列表：沿分支回溯，取最后一条 `task_progress` 的
/// `toolResult.details.tasks`。
///
/// `leaf` 是当前分支的叶子（`None` = 文件末尾）。pi 用
/// `sessionManager.getBranch()` 做同一件事；这里顺着 `parentId` 链得到同一条链。
/// 出错一律返回空列表：任务框是辅助信息，不值得让开会话失败。
fn restore_tasks(path: &Path, leaf: Option<&str>) -> Vec<TaskItem> {
    use std::collections::HashMap;

    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut entries: HashMap<String, (Option<String>, Value)> = HashMap::new();
    let mut last: Option<String> = None;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(id) = value.get("id").and_then(Value::as_str).map(str::to_string) else {
            continue;
        };
        let parent = value
            .get("parentId")
            .and_then(Value::as_str)
            .map(str::to_string);
        entries.insert(id.clone(), (parent, value));
        last = Some(id);
    }

    let mut cursor = leaf.map(str::to_string).or(last);
    let mut branch: Vec<Value> = Vec::new();
    let mut guard = 0usize;
    while let Some(id) = cursor {
        let Some((parent, value)) = entries.get(&id) else {
            break;
        };
        branch.push(value.clone());
        cursor = parent.clone();
        guard += 1;
        if guard > 200_000 {
            break;
        }
    }
    branch
        .iter()
        .rev()
        .find_map(task_progress_tasks)
        .unwrap_or_default()
}

fn task_progress_tasks(entry: &Value) -> Option<Vec<TaskItem>> {
    if entry.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let message = entry.get("message")?;
    if message.get("role").and_then(Value::as_str) != Some("toolResult") {
        return None;
    }
    if message.get("toolName").and_then(Value::as_str) != Some("task_progress") {
        return None;
    }
    serde_json::from_value(message.get("details")?.get("tasks")?.clone()).ok()
}

/// `QueuedMessages` → 两条队列的文本（图片不参与排队条展示）。
///
/// 不用 rpi 的 `texts()`：它把两条队列摊平成一个迭代器，分不出 steering 与
/// follow-up，而排队条要按队列分组渲染。
fn texts_of(queued: &QueuedMessages) -> (Vec<String>, Vec<String>) {
    (
        queued.steering.iter().map(|m| m.text.clone()).collect(),
        queued.follow_up.iter().map(|m| m.text.clone()).collect(),
    )
}

/// `{"streamingBehavior":"steer"|"followUp"}` → rpi 的 `deliver_as`。
///
/// 缺省是 `steer`（pi 的默认值）：会话空闲时这一项不影响行为，`send_message`
/// 照常开新一轮；只有运行中才决定进哪条队列。
fn streaming_delivery(command: &Value) -> MessageDelivery {
    match command.get("streamingBehavior").and_then(Value::as_str) {
        Some("followUp") | Some("follow_up") => MessageDelivery::FollowUp,
        Some("nextTurn") | Some("next_turn") => MessageDelivery::NextTurn,
        _ => MessageDelivery::Steer,
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
    /// `pendingUiSessionIds` 恒为空：toiler 没有「等待用户输入的 UI 请求」
    /// 这条链路（阶段 3 的说明里已记录）。
    pub fn running_snapshot(&self) -> Value {
        json!({
            "runningSessionIds": self.running(),
            // `ask_user` 正卡在提问上的会话：前端靠它弹「等待回答」提示。
            "pendingUiSessionIds": self.map
                .lock()
                .expect("live")
                .values()
                .filter(|agent| agent.has_pending_ui_request())
                .map(|agent| agent.id.clone())
                .collect::<Vec<_>>(),
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
