//! 新建对话插画只取本地季节信息，不访问外部图片或日历服务。

use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::seasons::Calendar;

static CALENDAR: OnceLock<Mutex<Calendar>> = OnceLock::new();

pub async fn season() -> Response {
    // 首次计算天文历与跨年重建放到阻塞池，避免占用异步请求线程。
    let result = tokio::task::spawn_blocking(|| {
        CALENDAR
            .get_or_init(|| Mutex::new(Calendar::default()))
            .lock()
            .map_err(|_| "节气日历状态不可用".to_string())?
            .snapshot(SystemTime::now())
    })
    .await;

    match result {
        Ok(Ok(snapshot)) => ([(header::CACHE_CONTROL, "no-store")], Json(snapshot)).into_response(),
        error => {
            tracing::warn!("读取页面季节失败：{error:?}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "读取季节失败"})),
            )
                .into_response()
        }
    }
}
