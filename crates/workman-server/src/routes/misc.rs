//! 其余核心 route：models / mobile / home / default-cwd / cwd。
//!
//! - `GET  /api/models`：模型列表（来自 `models.json`）
//! - `GET  /api/mobile/state`：手机端会话状态
//! - `GET  /api/home`：`{home}`
//! - `POST /api/default-cwd`：建目录并返回 `{cwd}`

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::mobile_view;
use crate::security::Trusted;
use crate::session_view;
use crate::settings::{self, ModelPattern, ModelRef};
use crate::state::AppState;
use rpi_ai::ThinkingLevel;

/// `GET /api/models`
///
/// 返回形状以 pi 的 `Mx()`（`tools/pi-contract/raw/models.js`）为准，字段名逐字对齐：
///
/// ```jsonc
/// {
///   "models": { "provider:id": "显示名" },      // 字典，不是数组！
///   "modelList": [{ provider, id, name }],
///   "defaultModel": { provider, modelId } | null,
///   "thinkingLevels": { "provider:id": ["off", …] },
///   "thinkingLevelMaps": { "provider:id": {…}},
///   "thinkingLevelPins": { "provider/id": "level" },
///   "source": "/path/to/models.json" | null
/// }
///
/// # 为什么不能把 `models` 发成数组
///
/// pi 前端把它当 `Record<string, string>` 用（`useAgentSession.ts` 的
/// `ModelsResponse`）。若发数组，`Object.entries()` 会得到 `["0", {…}]`，
/// `modelId` 变成 `"0"`、`name` 变成整个对象；随后 `ChatInput` 里
/// `Intl.Collator.compare(a.name || a.modelId, …)` 拿到对象而不是字符串，
/// 选模型时抛 TypeError，整个 React 树被卸载，页面变成 Next 的
/// 「This page couldn't load」兜底页。这正是选模型就白屏的根因。
///
/// # 两种分隔符不能混
///
/// `models` 与 `thinkingLevels*` 的键是 `provider:id`（**冒号**），
/// 而 `thinkingLevelPins` 的键是 `provider/id`（**斜杠**）—— pi 产品里就是
/// 这么分开的，统一成一种会让前端查不到，下拉框整片空白。
pub async fn models(State(state): State<AppState>, _trusted: Trusted) -> Response {
    let catalog = state.catalog();

    // 可见范围：`settings.json` 的 `enabledModels` 白名单（`None` = 全部可见）。
    // 不过滤的话，前端会显示所有 provider 的模型，而后端根本连不上其中一部分。
    let patterns = state.settings().enabled_models();
    let all: Vec<ModelRef> = catalog
        .entries
        .iter()
        .map(|e| ModelRef {
            provider: e.provider.clone(),
            id: e.id.clone(),
        })
        .collect();
    let visible = settings::visible_models(patterns.as_deref(), &all);

    let mut models = serde_json::Map::new();
    let mut model_list: Vec<Value> = Vec::new();
    let mut thinking_levels = serde_json::Map::new();
    let mut thinking_level_maps = serde_json::Map::new();

    for entry in &catalog.entries {
        if !visible
            .iter()
            .any(|m| m.provider == entry.provider && m.id == entry.id)
        {
            continue;
        }
        let key = entry.catalog_key();
        models.insert(key.clone(), Value::String(entry.name.clone()));
        model_list.push(entry.to_list_json());
        if let Some(model) = catalog.model_for(&entry.provider, &entry.id) {
            thinking_levels.insert(
                key.clone(),
                json!(crate::catalog::supported_thinking_levels(model)),
            );
            if let Some(map) = &model.thinking_level_map {
                // 原样回传模型自己的映射表：前端用它把等级换成厂商值
                // （如 `max -> "maximum"`）并标注「(xhigh)」这种差异。
                let mut out = serde_json::Map::new();
                for (level, mapped) in map {
                    let name = match level {
                        ThinkingLevel::Off => "off",
                        ThinkingLevel::Minimal => "minimal",
                        ThinkingLevel::Low => "low",
                        ThinkingLevel::Medium => "medium",
                        ThinkingLevel::High => "high",
                        ThinkingLevel::Xhigh => "xhigh",
                        ThinkingLevel::Max => "max",
                    };
                    out.insert(
                        name.to_string(),
                        mapped.clone().map(Value::String).unwrap_or(Value::Null),
                    );
                }
                thinking_level_maps.insert(key, Value::Object(out));
            }
        }
    }

    // `enabledModels` 里写成 `provider/id:level` 的模式会把新会话的思考等级
    // 直接钉死（见 pi 的 `resolveModelScope`）。只收非 auto 的等级。
    let mut thinking_level_pins = serde_json::Map::new();
    for raw in patterns.as_deref().unwrap_or_default() {
        let Some(pattern) = ModelPattern::parse(raw) else {
            continue;
        };
        let Some(level) = pattern.thinking.as_deref() else {
            continue;
        };
        if level == "auto" {
            continue;
        }
        for model in &visible {
            if settings::pattern_matches(&pattern, model) {
                thinking_level_pins.insert(
                    format!("{}/{}", model.provider, model.id),
                    Value::String(level.to_string()),
                );
            }
        }
    }

    let default_model = catalog.default_entry().map(|m| {
        json!({
            "provider": m.provider,
            "modelId": m.id,
        })
    });

    Json(json!({
        "models": Value::Object(models),
        "modelList": model_list,
        "defaultModel": default_model,
        "thinkingLevels": Value::Object(thinking_levels),
        "thinkingLevelMaps": Value::Object(thinking_level_maps),
        "thinkingLevelPins": Value::Object(thinking_level_pins),
        "source": catalog.source.as_ref().map(|p| p.to_string_lossy().to_string()),
    }))
    .into_response()
}

#[derive(Debug, Deserialize, Default)]
pub struct MobileParams {
    session: Option<String>,
    cwd: Option<String>,
    limit: Option<String>,
}

/// `GET /api/mobile/state`
pub async fn mobile_state(
    State(state): State<AppState>,
    _trusted: Trusted,
    Query(params): Query<MobileParams>,
) -> Response {
    let running = state.live().running();
    let sessions = state.sessions().list(false);

    let chosen: Option<Value> = if let Some(id) = params.session.as_deref() {
        sessions
            .iter()
            .find(|s| s.get("id").and_then(Value::as_str) == Some(id))
            .cloned()
    } else {
        let filtered: Vec<&Value> = match params.cwd.as_deref() {
            Some(cwd) => sessions
                .iter()
                .filter(|s| s.get("cwd").and_then(Value::as_str) == Some(cwd))
                .collect(),
            None => sessions.iter().collect(),
        };
        let pool = if filtered.is_empty() {
            sessions.iter().collect::<Vec<_>>()
        } else {
            filtered
        };
        // 优先正在运行的
        pool.iter()
            .find(|s| {
                s.get("id")
                    .and_then(Value::as_str)
                    .map(|id| running.iter().any(|r| r == id))
                    .unwrap_or(false)
            })
            .map(|s| (*s).clone())
            .or_else(|| pool.first().map(|s| (*s).clone()))
    };

    let Some(chosen) = chosen else {
        return Json(json!({
            "sessionId": null,
            "name": null,
            "cwd": params.cwd,
            "status": "idle",
            "updatedAt": null,
            "queue": { "steering": 0, "followUp": 0 },
            "pendingUiRequest": null,
            "messages": [],
            "earlierCount": 0,
        }))
        .into_response();
    };

    let id = chosen
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    // 活 agent 的排队信息 + 流式状态
    let (status, queue) = match state.live().get(&id) {
        Some(agent) => {
            let state_json = agent.state_json().await.unwrap_or(Value::Null);
            let status = if agent.is_streaming() {
                "running"
            } else {
                "idle"
            };
            (status, state_json.get("queuedMessages").cloned())
        }
        None => ("idle", None),
    };

    // 消息：走手机投影（pi 的 `buildMobileWindow`）。
    //
    // 这里**不能**直接下发原始 `AgentMessage`：toolResult 里的 base64 图片一张就
    // 几百 KB，阶段 5 走查实测 120 条窗口能到 474KB，手机弱网直接打不开。投影后
    // 图片变成「[N images]」占位，思考/工具入参也都截断，并且只送最新一窗。
    let limit = params
        .limit
        .as_deref()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(mobile_view::DEFAULT_LIMIT);
    let window = match state.sessions().session(&id) {
        Some(session) => {
            let view = session_view::context_view(&session.entries, None);
            mobile_view::build_window(&view.messages, &view.entry_ids, limit, true)
        }
        None => mobile_view::MobileWindow {
            messages: Vec::new(),
            earlier_count: 0,
        },
    };

    let mut body = json!({
        "sessionId": id,
        "name": chosen.get("name").cloned().unwrap_or(Value::Null),
        "cwd": chosen.get("cwd").cloned().unwrap_or(Value::Null),
        "status": status,
        "updatedAt": chosen.get("modified").cloned().unwrap_or(Value::Null),
        "queue": queue.unwrap_or_else(|| json!({ "steering": 0, "followUp": 0 })),
        "pendingUiRequest": Value::Null,
        "messages": window.messages,
        "earlierCount": window.earlier_count,
    });
    // pi 只在有值时才带这两个字段（前端靠「有没有」区分「没记录」与「已关闭」）。
    if let Some(session) = state.sessions().session(&id) {
        let view = session_view::context_view(&session.entries, None);
        if !view.thinking_level.is_null() {
            body["thinkingLevel"] = view.thinking_level;
        }
        if let Some(model) = view
            .model
            .get("modelId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            body["model"] = json!(model);
        }
    }
    Json(body).into_response()
}

/// `GET /api/home`
pub async fn home(_trusted: Trusted) -> Response {
    let home = std::env::var("HOME").unwrap_or_default();
    Json(json!({ "home": home })).into_response()
}

/// `POST /api/default-cwd` —— 建（如需要）、登记允许根、返回 `{cwd}`。
pub async fn default_cwd(State(state): State<AppState>, _trusted: Trusted) -> Response {
    let cwd = state.default_cwd().to_path_buf();
    if let Err(e) = crate::routes::projects::ensure_default_dir(&state, &cwd) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response();
    }
    Json(json!({ "cwd": cwd.to_string_lossy() })).into_response()
}

/// `GET /api/tools/settings` —— Shell 工具开关。
///
/// pi 用它决定常规页「Shell 工具」那一组画不画：`isWindows` 为 `false` 时前端
/// 整组不渲染（`SettingsPanel` 的条件是 `isWindows === true || 读取失败`）。
/// 少了这条路由，请求会落到静态兜底的 404 `{"error":"Not found"}`，前端把 error
/// 原文画在开关下面——这就是设置页里那条莫名其妙的红字。
///
/// workman 的 shell 工具是 bash（`rpi`），没有 PowerShell 通道，所以恒为关闭。
pub async fn tools_settings(_trusted: Trusted) -> Response {
    Json(json!({
        "isWindows": cfg!(windows),
        "powerShellEnabled": false,
    }))
    .into_response()
}

/// `PUT /api/tools/settings` —— pi 只在 Windows 上接受这个开关。
pub async fn tools_settings_put() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "error": "workman 的 shell 工具是 bash，没有 PowerShell 通道可切换"
        })),
    )
        .into_response()
}
