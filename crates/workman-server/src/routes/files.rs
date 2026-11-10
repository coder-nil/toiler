//! `/api/files/[...path]` —— 文件读 + `?type=watch` 的 SSE 变更推送。
//!
//! pi 的实现是 Next.js 里的一个 36KB bundle（含读写、目录列举、git 忽略、
//! watch）。阶段 2 先覆盖前端首屏必需的三件事：
//!
//! - `GET  /api/files/{path}`：读文件（文本 / 二进制 base64 / 目录列举）
//! - `HEAD /api/files/{path}`：存在性 + 大小 + mtime（文件树轮询用）
//! - `GET  /api/files/{path}?type=watch`：SSE，每 2s 比对 mtime，变了就推一条
//!
//! 前端首屏之外的高级选项（`?type=diff`、git 忽略规则、图片缩略）留到阶段 3。

use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::security::Trusted;
use crate::state::AppState;

/// 超过这个大小的文件不内联返回（pi 也会截断）。
const MAX_INLINE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Deserialize, Default)]
pub struct FileParams {
    /// `watch` = SSE 变更推送。
    #[serde(rename = "type")]
    kind: Option<String>,
}

/// 解析请求路径为绝对路径。
///
/// 两种形状都要认：
///
/// * `/api/files//Users/me/proj/a.ts` → 绝对路径
/// * `/api/files/Users/me/proj?type=list` → pi 前端把绝对路径**去掉前导斜杠**后拼进 URL，
///   所以非绝对形式的候选依次试「默认 cwd 相对」与「从根开始」。
///
/// `..` 一律拒绕（pi 同样校验），避免把请求变成任意文件读取。
#[allow(clippy::result_large_err)] // axum 的 Response 本身就是大结构，包一层只会更绕
pub fn resolve(state: &AppState, raw: &str) -> Result<PathBuf, Response> {
    let decoded = percent_decode(raw);
    if decoded.split('/').any(|seg| seg == "..") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid path" })),
        )
            .into_response());
    }

    let candidate = PathBuf::from(&decoded);
    let mut candidates: Vec<PathBuf> = Vec::new();
    if candidate.is_absolute() {
        candidates.push(candidate);
    } else {
        candidates.push(state.default_cwd().join(&candidate));
        candidates.push(Path::new("/").join(&candidate));
    }

    let mut first_normalized: Option<PathBuf> = None;
    for candidate in candidates {
        let normalized = PathBuf::from(crate::sessions_store::normalize_path(
            &candidate.to_string_lossy(),
        ));
        if normalized.exists() {
            return Ok(normalized);
        }
        first_normalized.get_or_insert(normalized);
    }
    // 都不存在：把第一个候选交回去，由调用方给出 404（而不是 400）。
    Ok(first_normalized.unwrap_or_else(|| state.default_cwd().to_path_buf()))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// `GET /api/files/{path}`
pub async fn read(
    State(state): State<AppState>,
    _trusted: Trusted,
    UrlPath(path): UrlPath<String>,
    Query(params): Query<FileParams>,
) -> Response {
    serve(&state, &path, params).await
}

/// `GET /api/files` 与 `/api/files/`：路径为空时落到默认 cwd。
///
/// 前端文件面板首次打开、还没拿到具体路径时会请求根路径；没有这条路由时
/// 会被静态兜底成 404 JSON，面板上直接显示 "Not found"。
pub async fn read_root(
    State(state): State<AppState>,
    _trusted: Trusted,
    Query(params): Query<FileParams>,
) -> Response {
    let cwd = state.default_cwd().to_string_lossy().to_string();
    serve(&state, &cwd, params).await
}

async fn serve(state: &AppState, path: &str, params: FileParams) -> Response {
    let target = match resolve(state, path) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    if params.kind.as_deref() == Some("watch") {
        return watch(target).await;
    }

    match std::fs::metadata(&target) {
        Ok(meta) if meta.is_dir() => list_dir(&target),
        Ok(meta) => read_file(&target, &meta),
        Err(_) => (StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" }))).into_response(),
    }
}

fn read_file(target: &Path, meta: &std::fs::Metadata) -> Response {
    if meta.len() > MAX_INLINE_BYTES {
        return Json(json!({
            "path": target.to_string_lossy(),
            "size": meta.len(),
            "truncated": true,
            "content": "",
        }))
        .into_response();
    }
    let Ok(bytes) = std::fs::read(target) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "read failed" })))
            .into_response();
    };
    let mtime = mtime_ms(meta);
    match String::from_utf8(bytes.clone()) {
        Ok(text) => Json(json!({
            "path": target.to_string_lossy(),
            "size": meta.len(),
            "mtime": mtime,
            "encoding": "utf8",
            "content": text,
        }))
        .into_response(),
        Err(_) => {
            use base64::Engine as _;
            Json(json!({
                "path": target.to_string_lossy(),
                "size": meta.len(),
                "mtime": mtime,
                "encoding": "base64",
                "content": base64::engine::general_purpose::STANDARD.encode(bytes),
            }))
            .into_response()
        }
    }
}

fn list_dir(target: &Path) -> Response {
    let Ok(entries) = std::fs::read_dir(target) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "readdir failed" })))
            .into_response();
    };
    let mut items: Vec<serde_json::Value> = entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            let meta = entry.metadata().ok();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            json!({
                "name": entry.file_name().to_string_lossy(),
                "path": path.to_string_lossy(),
                // pi 的前端读 `isDir`；`isDirectory` 是阶段 2 的旧字段名，���着保留
                "isDir": is_dir,
                "isDirectory": is_dir,
                "size": meta.as_ref().map(|m| m.len()).unwrap_or(0),
                "mtime": meta.as_ref().map(mtime_ms).unwrap_or(0),
            })
        })
        .collect();
    items.sort_by(|a, b| {
        let ad = a["isDirectory"].as_bool().unwrap_or(false);
        let bd = b["isDirectory"].as_bool().unwrap_or(false);
        bd.cmp(&ad).then_with(|| {
            a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or(""))
        })
    });
    Json(json!({
        "path": target.to_string_lossy(),
        "isDirectory": true,
        "entries": items,
    }))
    .into_response()
}

fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `?type=watch`：每 2s 看一次 mtime/size，变了就推一条 `change`。
async fn watch(target: PathBuf) -> Response {
    let stream = async_stream::stream! {
        let mut last = signature(&target);
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let current = signature(&target);
            if current != last {
                last = current.clone();
                let payload = json!({
                    "type": "change",
                    "path": target.to_string_lossy(),
                    "signature": current,
                });
                yield Ok::<Event, Infallible>(Event::default().data(payload.to_string()));
            }
        }
    };
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)).text(""))
        .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache, no-transform"),
    );
    response
}

fn signature(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(meta) => format!("{}:{}", meta.len(), mtime_ms(&meta)),
        Err(_) => "missing".to_string(),
    }
}
