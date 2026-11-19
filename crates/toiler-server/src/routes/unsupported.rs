//! 尚未移植的 pi 写侧接口：显式返回 501，不把 404 或空成功响应冒充实现。

use crate::security::Trusted;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

pub async fn checkpoints(_trusted: Trusted) -> Response {
    unsupported("暂不支持工作区检查点：尚未实现文件快照与回滚，发送消息不受影响")
}

pub async fn skill_packages(_trusted: Trusted) -> Response {
    unsupported("暂不支持技能包检查更新、升级或卸载：Toiler 只加载本地技能，请手动管理技能目录")
}

fn unsupported(message: &str) -> Response {
    (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": message}))).into_response()
}
