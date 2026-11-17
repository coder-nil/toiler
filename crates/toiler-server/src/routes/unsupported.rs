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

pub async fn reveal(_trusted: Trusted) -> Response {
    // `/api` 也面向手机和隧道，不能为了补一个接口让远端随意触发系统文件管理器。
    unsupported("暂不支持通过网页打开服务端文件管理器，请在本机手动打开文件所在位置")
}

fn unsupported(message: &str) -> Response {
    (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": message}))).into_response()
}
