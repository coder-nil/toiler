//! 工具 ↔ HTTP/SSE 的桥：`ask_user` 的问答往返、`task_progress` 的状态/widget 广播。
//!
//! # 为什么需要这一层
//!
//! pi 的扩展工具通过 `ctx.ui.select/confirm/input/editor` 与用户在**同一条链路**上
//! 往返：请求变成 SSE 的 `extension_ui_request` 帧，前端弹窗后把
//! `{type:"extension_ui_response", id, …}` 发回 `POST /api/agent/{id}`，工具那侧
//! 的 Promise 才 resolve。Rust 工具要等价实现，就需要一个「发事件 + 等响应」的
//! rendezvous，这个类型就是它。
//!
//! 事件与响应的字段逐字对齐 pi（`lib/types.ts` 的 `ExtensionUiRequest` /
//! `ExtensionUiResponse`），因此前端（含手机端）不需要改动：
//!
//! | 方向 | 形状 |
//! |---|---|
//! | 去前端 | `{type:"extension_ui_request", id, method:"select"\|"confirm"\|"input"\|"editor", …}` |
//! | 请求作废 | `{type:"extension_ui_resolved", id}`（回答 / 超时 / 中止都发） |
//! | 回后端 | `{type:"extension_ui_response", id, value}` / `{id, confirmed}` / `{id, cancelled:true}` |

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;

pub struct UiBridge {
    events: broadcast::Sender<Value>,
    pending: Mutex<HashMap<String, Pending>>,
}

struct Pending {
    /// 完整请求帧（`get_state.pendingUiRequests` 要按原样回放给前端）。
    request: Value,
    respond: Option<oneshot::Sender<Value>>,
}

impl UiBridge {
    pub fn new(events: broadcast::Sender<Value>) -> Arc<Self> {
        Arc::new(Self {
            events,
            pending: Mutex::new(HashMap::new()),
        })
    }

    /// 发一帧给 SSE（没有订阅者是正常情况：桌面没开流时事件直接丢弃）。
    pub fn emit(&self, event: Value) {
        let _ = self.events.send(event);
    }

    /// 还在等回答的请求（顺序无关；前端只用 id 去重）。
    pub fn pending_requests(&self) -> Vec<Value> {
        self.pending
            .lock()
            .expect("ui pending")
            .values()
            .map(|pending| pending.request.clone())
            .collect()
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.lock().expect("ui pending").is_empty()
    }

    /// 前端回传的响应。返回 `false` 表示这条请求已经作废（超时 / 已答 / 被中止）。
    pub fn respond(&self, response: &Value) -> bool {
        let Some(id) = response.get("id").and_then(Value::as_str) else {
            return false;
        };
        let pending = self.pending.lock().expect("ui pending").remove(id);
        match pending {
            Some(mut pending) => {
                if let Some(sender) = pending.respond.take() {
                    let _ = sender.send(response.clone());
                }
                true
            }
            None => false,
        }
    }

    /// 发请求并等回答。
    ///
    /// 返回前端原样的响应体；超时、中止、通道断开都收敛成
    /// `{cancelled:true}` —— 与 pi 的 `defaultValue = undefined` 等价
    /// （工具层再把 undefined 解读成 cancelled / expired）。
    ///
    /// `expiresAt` 由这里补：pi 的 `requestExtensionUi` 也是
    /// `timeout ? {timeout, expiresAt: Date.now() + timeout} : {}`。
    pub async fn request(
        &self,
        id: &str,
        request: Value,
        timeout: Option<Duration>,
        cancel: CancellationToken,
    ) -> Value {
        let mut frame = request;
        if let Some(timeout) = timeout {
            let millis = timeout.as_millis() as u64;
            frame["timeout"] = json!(millis);
            frame["expiresAt"] = json!(now_ms() + millis as i64);
        }

        let (sender, receiver) = oneshot::channel();
        self.pending.lock().expect("ui pending").insert(
            id.to_string(),
            Pending {
                request: frame.clone(),
                respond: Some(sender),
            },
        );
        self.emit(frame);

        let answered = match timeout {
            Some(timeout) => {
                tokio::select! {
                    biased;
                    value = receiver => value.ok(),
                    _ = tokio::time::sleep(timeout) => None,
                    _ = cancel.cancelled() => None,
                }
            }
            None => {
                tokio::select! {
                    biased;
                    value = receiver => value.ok(),
                    _ = cancel.cancelled() => None,
                }
            }
        };

        self.pending.lock().expect("ui pending").remove(id);
        // 「这条请求作废了」只有一个出口（回答 / 超时 / 中止），与 pi 的
        // `announceResolved` 同义：前端据此把弹窗收掉。
        self.emit(json!({ "type": "extension_ui_resolved", "id": id }));
        answered.unwrap_or_else(|| json!({ "id": id, "cancelled": true }))
    }

    /// `ctx.ui.setStatus(key, text)`；`text` 为 `None` 表示删除该状态。
    pub fn set_status(&self, key: &str, text: Option<&str>) {
        let mut frame = json!({
            "type": "extension_ui_request",
            "id": new_request_id(),
            "method": "setStatus",
            "statusKey": key,
        });
        if let Some(text) = text {
            frame["statusText"] = json!(text);
        }
        self.emit(frame);
    }

    /// `ctx.ui.setWidget(key, lines, {placement})`；`lines` 为 `None` 表示移除 widget。
    pub fn set_widget(&self, key: &str, lines: Option<&[String]>, placement: &str) {
        let mut frame = json!({
            "type": "extension_ui_request",
            "id": new_request_id(),
            "method": "setWidget",
            "widgetKey": key,
            "widgetPlacement": placement,
        });
        if let Some(lines) = lines {
            frame["widgetLines"] = json!(lines);
        }
        self.emit(frame);
    }
}

/// 一次 UI 请求的 id（pi 用 `randomUUID()`）。
pub fn new_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|delta| delta.as_millis() as i64)
        .unwrap_or(0)
}
