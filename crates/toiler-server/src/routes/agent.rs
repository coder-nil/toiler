//! `/api/agent/*` —— 命令与 SSE 事件流。
//!
//! - `POST /api/agent/{id}`：`{type:…}` → `{success:true,data}`
//! - `GET  /api/agent/{id}`：`{running}` / `{running,state}`
//! - `GET  /api/agent/{id}/events`：SSE（`connected` → agent 事件 → 关闭）
//! - `POST /api/agent/new`：建新会话
//! - `GET  /api/agent/running`、`GET /api/agent/running/events`

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;

use crate::agent_runtime::LiveAgent;
use crate::security::Trusted;
use crate::state::AppState;

/// `POST /api/agent/{id}`
pub async fn command(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let command_type = body.get("type").and_then(Value::as_str).unwrap_or("");
    let is_prompt = command_type == "prompt";

    let agent = match ensure_agent(&state, &id).await {
        Ok(agent) => agent,
        Err(AgentError::NotFound) => {
            let mut payload = serde_json::Map::new();
            payload.insert("error".into(), json!("Session not found"));
            if is_prompt {
                payload.insert("code".into(), json!("prompt_rejected"));
                payload.insert("accepted".into(), json!(false));
            }
            return (StatusCode::NOT_FOUND, Json(Value::Object(payload))).into_response();
        }
        Err(AgentError::Failed(message)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": message })),
            )
                .into_response()
        }
    };

    match agent.send(&body).await {
        Ok(data) => Json(json!({ "success": true, "data": data })).into_response(),
        Err(e) => {
            let mut payload = serde_json::Map::new();
            payload.insert("error".into(), json!(e.to_string()));
            if is_prompt {
                payload.insert("code".into(), json!("prompt_rejected"));
                payload.insert("accepted".into(), json!(false));
            }
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Value::Object(payload)),
            )
                .into_response()
        }
    }
}

/// `GET /api/agent/{id}`
pub async fn state_of(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(id): Path<String>,
) -> Response {
    match state.live().get(&id) {
        Some(agent) if agent.is_streaming() => match agent.state_json().await {
            Ok(state_json) => Json(json!({ "running": true, "state": state_json })).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response(),
        },
        _ => Json(json!({ "running": false })).into_response(),
    }
}

/// `GET /api/agent/{id}/events` —— SSE。
pub async fn events(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(id): Path<String>,
) -> Response {
    let agent = match ensure_agent(&state, &id).await {
        Ok(agent) => agent,
        Err(AgentError::NotFound) => {
            return (StatusCode::NOT_FOUND, "Session not found").into_response()
        }
        Err(AgentError::Failed(message)) => {
            // pi：先发 startup_error，再关流
            let payload = json!({
                "type": "startup_error",
                "errorMessage": format!("Failed to start agent: {message}"),
            });
            let stream = futures::stream::iter(vec![Ok::<Event, Infallible>(
                Event::default().data(payload.to_string()),
            )]);
            return sse_response(stream);
        }
    };

    let receiver = agent.subscribe();
    let is_streaming = agent.is_streaming();
    let connected = json!({
        "type": "connected",
        "sessionId": id,
        "isStreaming": is_streaming,
    });

    let stream = async_stream::stream! {
        yield Ok::<Event, Infallible>(Event::default().data(connected.to_string()));
        let mut receiver = receiver;
        loop {
            match receiver.recv().await {
                Ok(payload) => yield Ok(Event::default().data(payload.to_string())),
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    };

    sse_response(stream)
}

fn sse_response<S>(stream: S) -> Response
where
    S: Stream<Item = Result<Event, Infallible>> + Send + 'static,
{
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)).text(""))
        .into_response();
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache, no-transform"),
    );
    headers.insert(
        axum::http::header::CONNECTION,
        axum::http::HeaderValue::from_static("keep-alive"),
    );
    headers.insert(
        axum::http::HeaderName::from_bytes(b"x-accel-buffering").unwrap(),
        axum::http::HeaderValue::from_static("no"),
    );
    response
}

/// `GET /api/agent/running` —— `{runningSessionIds, pendingUiSessionIds}`。
pub async fn running(State(state): State<AppState>, _trusted: Trusted) -> Response {
    let mut response = Json(state.live().running_snapshot()).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftSystemPrompt {
    cwd: String,
    tool_names: Option<Vec<String>>,
    ui_locale: Option<String>,
    #[serde(default)]
    chat_mode: bool,
}

/// 草稿预览不建会话、不更新偏好；提示词使用与运行时相同的工具注册表和组装规则。
pub async fn draft_system_prompt(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(draft): Json<DraftSystemPrompt>,
) -> Response {
    use crate::routes::sessions::error_response;
    use crate::system_prompt::{Locale, PromptInputs};

    let path = std::path::Path::new(&draft.cwd);
    if !path.is_absolute() {
        return error_response(StatusCode::BAD_REQUEST, "cwd must be an absolute directory");
    }
    state.sync_allowed_roots();
    if !state.roots().allows(path) || !state.roots().allows_real(path) {
        return error_response(StatusCode::FORBIDDEN, "Access denied");
    }
    if !path.is_dir() {
        return error_response(StatusCode::BAD_REQUEST, "cwd must be a directory");
    }
    if draft.chat_mode {
        return Json(json!({"systemPrompt": ""})).into_response();
    }

    let cwd = crate::paths::simplified(&draft.cwd);
    let trusted = crate::trust::status(state.project_trust(), &cwd).trusted;
    let (events, _receiver) = tokio::sync::broadcast::channel(1);
    let ui = crate::desktop_tools::UiBridge::new(events);
    let tasks = crate::desktop_tools::TaskProgressStore::new(ui.clone());
    let tools = crate::tools::build_tools(&cwd, ui, tasks);
    let names = crate::tools::active_tool_names(
        tools.into_iter().map(|(name, _)| name),
        draft.tool_names.as_deref(),
    );
    let inputs = PromptInputs::load(&cwd, &names, &crate::paths::toiler_home(), trusted).await;
    Json(json!({"systemPrompt": inputs.compose(Locale::parse(draft.ui_locale.as_deref()))}))
        .into_response()
}

/// `POST /api/agent/new` —— 建会话、登记项目、顺带执行 body 里的命令。
///
/// 形状对齐 pi：`{success, sessionId, data, model:{provider, modelId}, thinkingLevel}`，
/// body 里除了 `cwd` 之外的字段会原样交给 agent 当命令（`prompt` / `get_state` …）。
///
/// # body 里的 `provider` / `modelId`
///
/// 新会话在输入框里选过模型时，前端 `ensureNewSession()` 会把那一对放在这里。
/// pi 把它作为 `initialModel` 交给会话构造（`tools/pi-contract/raw/agent_new.js`）：
///
/// ```js
/// let { provider: m, modelId: o, … } = l;
/// if (m && !o || !m && o) throw Error("provider and modelId must be provided together");
/// …{ ...(m && o ? { initialModel: { provider: m, modelId: o } } : {}) }
/// ```
///
/// 早先这里**根本不看这两个字段**，一律用 `AppState::default_model_ref()` 建会话，
/// 于是用户在草稿页选中的模型 100% 被丢掉：界面选择器明明写着 A，首条消息却发给
/// B。更隐蔽的是前端随后并不报错 —— `ensureNewSession()` 直接采用响应里的
/// `model` 作为新会话默认值，所以界面会当场跳回默认模型，看起来像「选什么都
/// 被重置」。
pub async fn new_session(
    State(state): State<AppState>,
    _trusted: Trusted,
    body: Option<Json<Value>>,
) -> Response {
    let payload = body.map(|Json(value)| value).unwrap_or(Value::Null);
    let command_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
    let is_prompt = command_type == "prompt";

    let Some(cwd) = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
    else {
        let mut body = serde_json::Map::new();
        body.insert("error".into(), json!("cwd is required"));
        if is_prompt {
            body.insert("code".into(), json!("prompt_rejected"));
            body.insert("accepted".into(), json!(false));
        }
        return (StatusCode::BAD_REQUEST, Json(Value::Object(body))).into_response();
    };
    if !std::path::Path::new(cwd).exists() {
        let mut body = serde_json::Map::new();
        body.insert(
            "error".into(),
            json!(format!("Directory does not exist: {cwd}")),
        );
        if is_prompt {
            body.insert("code".into(), json!("prompt_rejected"));
            body.insert("accepted".into(), json!(false));
        }
        return (StatusCode::BAD_REQUEST, Json(Value::Object(body))).into_response();
    }
    // 写进 v3 header 的 cwd 先去 Windows 扩展前缀：会话文件是 append-only，
    // 写进去就改不回来了，界面上的「工作目录」也直接读这个值。
    let cwd = crate::paths::simplified(cwd);

    // 前端明确选过的模型优先，缺省才用默认模型（下一步在 `ensure_agent` 里解析）。
    // 成对校验与 pi 一致：只给一个说明调用方写错了，静默容忍会让「选了却没生效」
    // 这类问题更难查。
    let requested_model = match (
        payload.get("provider").and_then(Value::as_str),
        payload.get("modelId").and_then(Value::as_str),
    ) {
        (Some(provider), Some(model_id)) => Some((provider.to_string(), model_id.to_string())),
        (None, None) => None,
        _ => {
            let mut body = serde_json::Map::new();
            body.insert(
                "error".into(),
                json!("provider and modelId must be provided together"),
            );
            if is_prompt {
                body.insert("code".into(), json!("prompt_rejected"));
                body.insert("accepted".into(), json!(false));
            }
            return (StatusCode::BAD_REQUEST, Json(Value::Object(body))).into_response();
        }
    };

    if let Some((provider, model_id)) = &requested_model {
        if state.catalog().model_for(provider, model_id).is_none() {
            let mut body = json!({ "error": format!("Model not found: {provider}/{model_id}") });
            if is_prompt {
                body["code"] = json!("prompt_rejected");
                body["accepted"] = json!(false);
            }
            return (StatusCode::BAD_REQUEST, Json(body)).into_response();
        }
    }

    let repo = toiler_session::V3SessionRepo::new(state.sessions().root());
    let created_at = now_ms();
    let id = uuid::Uuid::now_v7().to_string();
    let header = toiler_session::V3Header::new(&id, created_at, &cwd);
    let dir = state
        .sessions()
        .root()
        .join(toiler_session::session_directory_name(&cwd));
    let file = dir.join(toiler_session::session_file_name(
        header.created_at,
        &header.id,
    ));
    match toiler_session::V3SessionStorage::create_at(&file, &header, repo.clock(), repo.ids()) {
        Ok(storage) => drop(storage),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
    state.sessions().invalidate();

    // 与 pi 一样：新 cwd 立刻成为允许根，并写进项目库
    state.roots().add(&cwd);
    let hint = crate::sessions_store::project_hint(&cwd);
    state.roots().add(&hint.root);
    let _ = state
        .projects()
        .upsert(&crate::paths::project_key(&hint.root), &hint.root, &cwd);

    // 新会话没有 `model_change` 条目，所以「用户在草稿页选的那个模型」只能靠
    // 这里传下去 —— 不传就等于把选择丢掉，会话会静默用默认模型建起来。
    let agent = match ensure_agent_with_model(&state, &id, requested_model.clone()).await {
        Ok(agent) => agent,
        Err(AgentError::NotFound) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Session not found" })),
            )
                .into_response()
        }
        Err(AgentError::Failed(message)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": message })),
            )
                .into_response()
        }
    };

    // 只记住显式选择；打开历史会话或默认建会话不应覆盖用户偏好。
    if let Some((provider, model_id)) = &requested_model {
        if let Err(error) = state.settings().set_default_model(provider, model_id) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("保存最近选择的模型失败：{error}") })),
            )
                .into_response();
        }
    }

    // 显式推理级别必须在首轮 prompt 和状态快照之前真正应用到 lane。
    if let Some(level) = payload.get("thinkingLevel") {
        if let Err(error) = agent
            .send(&json!({"type": "set_thinking_level", "level": level}))
            .await
        {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    }

    // 普通对话模式：前端带 `chatMode: true` 建会话时，第一轮就不该带系统提示词。
    // 与 pi 的 `/api/agent/new` 一致。顺序要紧：先 `set_chat_mode` 再 `set_tools`，
    // 否则 `set_tools` 会把工具重新激活，普通对话模式就不再是空的。
    if payload.get("chatMode").and_then(Value::as_bool) == Some(true) {
        let _ = agent
            .send(&json!({ "type": "set_chat_mode", "on": true }))
            .await;
    } else if let Some(names) = payload.get("toolNames").and_then(Value::as_array) {
        // 工具预设：前端新会话会带上 `toolNames`（`getToolNamesForPreset`，默认
        // `read/bash/edit/write`）。忽略它会让会话始终 7 个工具全开，前端选的预设
        // 也就形同虚设；而 pi 的「用 bash 做 ls/rg/find」那条准则只在没有
        // grep/find/ls 时出现，所以默认预设下的提示词也会与 pi 不同。
        let names: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
        let _ = agent
            .send(&json!({ "type": "set_tools", "toolNames": names }))
            .await;
    }

    // pi 的 `/api/agent/new` 会带上界面语言（`uiLocale`），新会话第一轮就该用对文案。
    // 前端的 `set_ui_locale` 也会随后到达，这里先应用一次不会冲突。
    if let Some(locale) = payload.get("uiLocale").and_then(Value::as_str) {
        let _ = agent
            .send(&json!({ "type": "set_ui_locale", "locale": locale }))
            .await;
    }

    let state_snapshot = agent.state_json().await.unwrap_or(Value::Null);
    let model = state_snapshot
        .get("model")
        .filter(|m| !m.is_null())
        .map(|m| {
            json!({
                "provider": m.get("provider").cloned().unwrap_or(Value::Null),
                "modelId": m.get("id").cloned().unwrap_or(Value::Null),
            })
        })
        .unwrap_or(Value::Null);
    let thinking_level = state_snapshot
        .get("thinkingLevel")
        .cloned()
        .unwrap_or(Value::Null);

    if command_type.is_empty() || command_type == "ensure_session" {
        return Json(json!({
            "success": true,
            "sessionId": id,
            "data": Value::Null,
            "model": model,
            "thinkingLevel": thinking_level,
        }))
        .into_response();
    }

    let mut command = payload.clone();
    if let Some(object) = command.as_object_mut() {
        object.remove("cwd");
    }
    match agent.send(&command).await {
        Ok(data) => Json(json!({
            "success": true,
            "sessionId": id,
            "data": data,
            "model": model,
            "thinkingLevel": thinking_level,
        }))
        .into_response(),
        Err(e) => {
            let mut body = serde_json::Map::new();
            body.insert("error".into(), json!(e.to_string()));
            if is_prompt {
                body.insert("code".into(), json!("prompt_rejected"));
                body.insert("accepted".into(), json!(false));
            }
            (StatusCode::INTERNAL_SERVER_ERROR, Json(Value::Object(body))).into_response()
        }
    }
}

enum AgentError {
    NotFound,
    Failed(String),
}

/// 有活 agent 就用它；没有就按 id 找文件、建一个。
async fn ensure_agent(state: &AppState, id: &str) -> Result<Arc<LiveAgent>, AgentError> {
    ensure_agent_with_model(state, id, None).await
}

/// 同 [`ensure_agent`]，但可以指定初始模型。
///
/// `requested` 来自 `POST /api/agent/new` body 里的 `provider`/`modelId`（用户在新会话
/// 里选过的模型），只在**首次建 agent** 时有意义：活会话已经在内存里就直接用它，
/// 中途换模型走 `set_model` 命令（它会往会话文件追加 `model_change`），不能靠重建
/// harness 来实现 —— 重建会把正在跑的轮次、已经连上的 SSE 流一起拆掉。
async fn ensure_agent_with_model(
    state: &AppState,
    id: &str,
    requested: Option<(String, String)>,
) -> Result<Arc<LiveAgent>, AgentError> {
    if let Some(agent) = state.live().get(id) {
        return Ok(agent);
    }
    let Some(path) = state.sessions().path_of(id) else {
        return Err(AgentError::NotFound);
    };
    let session = crate::sessions_store::read_session(&path).ok();
    let cwd = session
        .as_ref()
        .map(|s| crate::paths::simplified(&s.header.cwd))
        .unwrap_or_else(|| state.default_cwd().to_string_lossy().to_string());

    if !state.live().guard_starting(id) {
        // 另一个请求正在建：等它建好
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(agent) = state.live().get(id) {
                return Ok(agent);
            }
        }
    }

    // 显式新会话选择优先；续聊则与输入框一样，先恢复历史模型再看默认偏好。
    let preferred = requested.or_else(|| {
        let historical = session
            .as_ref()
            .and_then(|s| crate::session_view::context_model(&s.entries));
        state.session_model_ref(historical.as_ref())
    });
    let catalog = state.dialogue_catalog();
    // 项目是否已信任：决定项目技能要不要参与系统提示词的 `<skills>` 段与
    // `/skill:` 展开，与 `/api/skills` 的 `projectResourcesLoaded` 同一语义。
    let trusted = crate::trust::status(state.project_trust(), &cwd).trusted;
    let on_first_message = session
        .as_ref()
        .filter(|s| s.message_count() == 0)
        .map(|_| {
            // 活会话属于 AppState，不能反向强持有它。
            let inner = Arc::downgrade(&state.inner);
            move || {
                if let Some(inner) = inner.upgrade() {
                    inner.sessions.invalidate();
                }
            }
        });
    let started = LiveAgent::start(
        id,
        &path,
        &cwd,
        &catalog,
        preferred,
        state.settings(),
        trusted,
        on_first_message,
    )
    .await;
    state.live().clear_starting(id);
    match started {
        Ok(agent) => {
            state.live().insert(Arc::clone(&agent));
            Ok(agent)
        }
        Err(e) => Err(AgentError::Failed(e.to_string())),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
