//! `/api/files/[...path]` —— 文件读写、目录列举、预览与变更推送。
//!
//! 契约事实来源是 pi 的 `app/api/files/[...path]/route.ts`（29KB，从编译产物
//! 与源码逐条对齐）。GET 的 `type` 有六种，全部实现：
//!
//! | `type` | 行为 |
//! |---|---|
//! | `list`（默认） | 目录列举，`{entries, path}`；支持 `hideDotDirs=true` |
//! | `read` | 文本 → `{content, language, size}`；图片/音频/文档 → 流原字节 |
//! | `download` | 一律流原字节，`Content-Disposition: attachment` |
//! | `meta` | `{size, language, mime, previewKind}`（前端轮询用） |
//! | `preview` | DOCX → HTML 页面；其它类型 400 |
//! | `watch` | SSE，命名事件 `connected` / `change` |
//!
//! 写侧（阶段 5 补）：
//!
//! | 方法 | 行为 |
//! |---|---|
//! | `PUT` | 写文件，`{content, allowEmpty}`；空内容覆盖需要显式允许 |
//! | `PATCH` | 重命名，`{name}` |
//! | `POST` | `?type=upload-check` 查冲突；`?type=upload` 收 multipart（≤100MB） |
//!
//! 三种形状的路径都认（pi 前端三种都发）：
//!
//! - `/api/files//Users/me/a.ts`（保留前导斜杠的绝对路径）
//! - `/api/files/Users/me/a.ts`（去掉前导斜杠）
//! - `/api/files`（空路径 → 默认 cwd）
//!
//! 授权分两层，与 pi 的 `getAllowedFileRoots` 对齐：先词法包含，再 realpath 包含
//! （后者挡住「允许根里的符号链接逃逸」）。

use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Multipart, Path as UrlPath, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::docx;
use crate::filemeta;
use crate::security::Trusted;
use crate::state::AppState;

/// 文本预览上限（pi 的 `TEXT_PREVIEW_MAX_BYTES`）。
const TEXT_PREVIEW_MAX_BYTES: u64 = filemeta::TEXT_PREVIEW_MAX_BYTES;
/// 图片预览上限（pi 的 `IMAGE_PREVIEW_MAX_BYTES`）。
const IMAGE_PREVIEW_MAX_BYTES: u64 = filemeta::IMAGE_PREVIEW_MAX_BYTES;
/// DOCX 预览上限（pi 的 `DOCX_PREVIEW_MAX_BYTES`）。
const DOCX_PREVIEW_MAX_BYTES: u64 = filemeta::DOCX_PREVIEW_MAX_BYTES;

/// 编辑保存的正文上限（pi 的 `MAX_EDIT_BYTES`，256KB）。
const MAX_EDIT_BYTES: usize = 256 * 1024;
/// 单个上传文件上限（pi 的 `MAX_UPLOAD_FILE_BYTES`，25MB）。
const MAX_UPLOAD_FILE_BYTES: u64 = 25 * 1024 * 1024;
/// 一次上传的总上限（pi 的 `MAX_UPLOAD_TOTAL_BYTES`，100MB）。
const MAX_UPLOAD_TOTAL_BYTES: u64 = 100 * 1024 * 1024;

/// 目录列举里一律隐藏的名字（pi 的 `IGNORED_NAMES`）。
const IGNORED_NAMES: &[&str] = &[
    "node_modules",
    ".git",
    ".next",
    "dist",
    "build",
    "__pycache__",
    ".turbo",
    ".cache",
    "coverage",
    ".pytest_cache",
    ".mypy_cache",
    "target",
    "vendor",
    ".DS_Store",
];
/// 目录列举里一律隐藏的后缀（pi 的 `IGNORED_SUFFIXES`）。
const IGNORED_SUFFIXES: &[&str] = &[".pyc"];

const FILE_REQUEST_TYPES: &[&str] = &["list", "read", "download", "meta", "preview", "watch"];

#[derive(Debug, Deserialize, Default)]
pub struct FileParams {
    /// 六种 `type` 之一，缺省 `list`。
    #[serde(rename = "type")]
    kind: Option<String>,
    /// `list` 时是否隐藏点开头的条目（前端只在项目根传 `true`）。
    #[serde(rename = "hideDotDirs")]
    hide_dot_dirs: Option<String>,
    /// 引用这个文件的会话 id：路径不在允许根内时的第二条授权途径。
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}

/// 解析请求路径为绝对路径。
///
/// `..` 一律拒绕（pi 同样校验），避免把请求变成任意文件读取。
///
/// 与 pi 的 `filePathFromSegments` 同语义：**只有一种解释**——去掉前导斜杠后重新
/// 从根拼回去。没有「相对 cwd」的分支，因为 pi 前端从不发相对路径；多一条猜测
/// 只会让不存在的路径解析到别处（从而把 404 变成 403）。
#[allow(clippy::result_large_err)] // axum 的 Response 本身就是大结构，包一层只会更绕
pub fn resolve(state: &AppState, raw: &str) -> Result<PathBuf, Response> {
    let decoded = percent_decode(raw);
    if decoded.split(['/', '\\']).any(|seg| seg == "..") {
        return Err(bad_request("Invalid path"));
    }
    let trimmed = decoded.trim_start_matches(['/', '\\']);
    if trimmed.is_empty() {
        return Ok(state.default_cwd().to_path_buf());
    }
    Ok(PathBuf::from(crate::sessions_store::normalize_path(
        &format!("/{trimmed}"),
    )))
}

/// 会话正文里是否出现过这个绝对路径（pi 的 `containsExactPathReference`）。
///
/// 判定要求两边都是边界：前面不是路径字符，后面也不是（后面跟 `:` 时要求再跟数字，
/// 避开 `a.rs:12:5` 这种行列号误判）。这样 `/a/b.rs` 不会命中 `/a/b.rs.bak`。
fn referenced_by_session(state: &AppState, session_id: Option<&str>, target: &Path) -> bool {
    let Some(session_id) = session_id.filter(|id| is_valid_session_id(id)) else {
        return false;
    };
    let Some(session) = state.sessions().session(session_id) else {
        return false;
    };
    let wanted = target.to_string_lossy().replace('\\', "/");
    session
        .entries
        .iter()
        .any(|entry| json_contains_path(entry, &wanted))
}

/// 会话 id 形状（pi 的 `isValidSessionId`）：UUID 的 8-4-4-4-12 十六进制。
fn is_valid_session_id(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    let expected = [8usize, 4, 4, 4, 12];
    groups.len() == expected.len()
        && groups.iter().zip(expected).all(|(group, len)| {
            group.len() == len && group.chars().all(|ch| ch.is_ascii_hexdigit())
        })
}

/// 在任意 JSON 结构里找字符串（与 pi 的 `collectStrings` 同语义）。
fn json_contains_path(value: &serde_json::Value, wanted: &str) -> bool {
    match value {
        serde_json::Value::String(text) => contains_exact_path(text, wanted),
        serde_json::Value::Array(items) => {
            items.iter().any(|item| json_contains_path(item, wanted))
        }
        serde_json::Value::Object(map) => map.values().any(|item| json_contains_path(item, wanted)),
        _ => false,
    }
}

fn contains_exact_path(haystack: &str, wanted: &str) -> bool {
    let haystack = haystack.replace('\\', "/");
    // pi 会同时对比原文与 percent-decode 后的文本；encode 过的路径也要认。
    let mut candidates = vec![haystack.clone()];
    if let Some(decoded) = percent_decode_utf8(&haystack) {
        candidates.push(decoded);
    }

    for text in &candidates {
        for needle in [wanted.to_string(), format!("file://{wanted}")] {
            let mut from = 0usize;
            while from < text.len() {
                let Some(found) = text[from..].find(&needle) else {
                    break;
                };
                let start = from + found;
                let after = start + needle.len();
                let boundary_before =
                    start == 0 || !is_path_char(text[..start].chars().next_back().unwrap_or(' '));
                let boundary_after = match text[after..].chars().next() {
                    None => true,
                    Some(':') => text[after + 1..]
                        .chars()
                        .next()
                        .map(|ch| ch.is_ascii_digit())
                        .unwrap_or(false),
                    Some(ch) => !is_path_char(ch),
                };
                if boundary_before && boundary_after {
                    return true;
                }
                from = start + 1;
            }
        }
    }
    false
}

/// pi 的 `isPathChar`：`[A-Za-z0-9._~+%@/:\\-]`。
fn is_path_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            '.' | '_' | '~' | '+' | '%' | '@' | '/' | '\\' | ':' | '-'
        )
}

fn percent_decode_utf8(input: &str) -> Option<String> {
    if !input.contains('%') {
        return None;
    }
    let decoded = percent_decode(input);
    (decoded != input).then_some(decoded)
}

fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}

fn forbidden(message: &str) -> Response {
    (StatusCode::FORBIDDEN, Json(json!({ "error": message }))).into_response()
}

fn not_found(message: &str) -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": message }))).into_response()
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

/// 两层授权：词法包含 + realpath 包含。
///
/// `existing` 是「用来做 realpath 判定的那个路径」——读文件时它必须是目标本身，
/// 目标不存在时（`?type=watch` 等）退到父目录，这与 pi 的
/// `const existingAuthorizationPath = stat ? filePath : path.dirname(filePath)` 一致。
fn authorize(state: &AppState, target: &Path, existing: &Path) -> bool {
    state.sync_allowed_roots();
    state.roots().allows(target) && state.roots().allows_real(existing)
}

/// `GET /api/files/{path}`
pub async fn read(
    State(state): State<AppState>,
    _trusted: Trusted,
    UrlPath(path): UrlPath<String>,
    Query(params): Query<FileParams>,
    headers: HeaderMap,
) -> Response {
    serve(&state, &path, params, &headers).await
}

/// `GET /api/files` 与 `/api/files/`：路径为空时落到默认 cwd。
///
/// 前端文件面板首次打开、还没拿到具体路径时会请求根路径；没有这条路由时
/// 会被静态兜底成 404 JSON，面板上直接显示 "Not found"。
pub async fn read_root(
    State(state): State<AppState>,
    _trusted: Trusted,
    Query(params): Query<FileParams>,
    headers: HeaderMap,
) -> Response {
    let cwd = state.default_cwd().to_string_lossy().to_string();
    serve(&state, &cwd, params, &headers).await
}

async fn serve(state: &AppState, path: &str, params: FileParams, headers: &HeaderMap) -> Response {
    let raw_type = params.kind.as_deref().unwrap_or("list");
    if !FILE_REQUEST_TYPES.contains(&raw_type) {
        return bad_request("Invalid file request type");
    }

    let target = match resolve(state, path) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let meta = std::fs::metadata(&target).ok();
    // 目标不存在时用父目录做 realpath 判定（`?type=watch` 允许盯一个还没建的文件）。
    let existing_for_auth = if meta.is_some() {
        target.clone()
    } else {
        target
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| target.clone())
    };

    // 第二条授权途径（pi 的 `isFilePathReferencedBySession`）：路径不在允许根内，
    // 但如果这个会话的正文里确实提到过它的绝对路径（agent 读过 / 写过），也放行。
    // `list` 不适用：目录不属于「某个文件被引用」。
    let in_roots = state.roots().allows(&target) && state.roots().allows_real(&existing_for_auth);
    let allowed_by_reference = raw_type != "list"
        && !in_roots
        && referenced_by_session(state, params.session_id.as_deref(), &target);

    if !allowed_by_reference && !authorize(state, &target, &existing_for_auth) {
        return forbidden("Access denied");
    }

    match raw_type {
        "watch" => {
            if matches!(&meta, Some(m) if !m.is_file()) {
                return bad_request("Not a file");
            }
            watch(target).await
        }
        "list" => {
            if !matches!(&meta, Some(m) if m.is_dir()) {
                return match meta {
                    Some(_) => bad_request("Not a directory"),
                    None => not_found("Not found"),
                };
            }
            let hide_dot_dirs = params.hide_dot_dirs.as_deref() == Some("true");
            list_dir(&target, hide_dot_dirs)
        }
        _ => {
            let Some(meta) = meta else {
                return not_found("Not found");
            };
            if !meta.is_file() {
                return bad_request("Not a file");
            }
            match raw_type {
                "read" => read_target(&target, &meta, range_header(headers)),
                "download" => {
                    let mime = filemeta::download_mime(&target.to_string_lossy());
                    stream_file(&target, &meta, &mime, range_header(headers), true)
                }
                "meta" => meta_json(&target, &meta),
                "preview" => preview(&target, &meta),
                _ => unreachable!("上面的 FILE_REQUEST_TYPES 已经挡掉未知 type"),
            }
        }
    }
}

/// `Range: bytes=start-end` / `bytes=start-` / `bytes=-suffix`。
fn range_header(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// `?type=read`：图片/音频/文档流原字节，其余按文本返回 JSON。
fn read_target(target: &Path, meta: &std::fs::Metadata, range: Option<String>) -> Response {
    let path_str = target.to_string_lossy().to_string();
    if let Some(mime) = filemeta::image_mime(&path_str) {
        if meta.len() > IMAGE_PREVIEW_MAX_BYTES {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({ "error": "Image too large (>10MB)" })),
            )
                .into_response();
        }
        return stream_file(target, meta, &mime, range, false);
    }
    if let Some(mime) = filemeta::audio_mime(&path_str) {
        return stream_file(target, meta, &mime, range, false);
    }
    if let Some(mime) = filemeta::document_mime(&path_str) {
        return stream_file(target, meta, &mime, range, false);
    }
    if meta.len() > TEXT_PREVIEW_MAX_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "error": "File too large for preview (>256KB)" })),
        )
            .into_response();
    }
    let Ok(content) = std::fs::read_to_string(target) else {
        // 声明是文本但实际不是 UTF-8：给一个明确的错误，别让前端拿到乱码。
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "File is not valid UTF-8" })),
        )
            .into_response();
    };
    Json(json!({
        "content": content,
        "language": filemeta::language(&path_str),
        "size": meta.len(),
    }))
    .into_response()
}

/// `?type=meta`：前端文件监听轮询只读这几个字段。
fn meta_json(target: &Path, meta: &std::fs::Metadata) -> Response {
    let path_str = target.to_string_lossy();
    Json(json!({
        "size": meta.len(),
        "language": filemeta::language(&path_str),
        "mime": filemeta::meta_mime(&path_str),
        "previewKind": filemeta::document_preview_kind(&path_str),
    }))
    .into_response()
}

/// `?type=preview`：只有 DOCX 有预览页（PDF 走 `?type=read` 的 iframe）。
fn preview(target: &Path, meta: &std::fs::Metadata) -> Response {
    let path_str = target.to_string_lossy().to_string();
    if filemeta::extension(&path_str) != "docx" {
        return bad_request("Preview not available for this file type");
    }
    if meta.len() > DOCX_PREVIEW_MAX_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "error": "DOCX too large for preview (>10MB)" })),
        )
            .into_response();
    }
    let Ok(bytes) = std::fs::read(target) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "read failed" })),
        )
            .into_response();
    };
    let Some(body_html) = docx::docx_to_html(&bytes) else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "Could not parse the DOCX document" })),
        )
            .into_response();
    };
    let html = docx::wrap_preview_html(&body_html, filemeta::base_name(&path_str));
    let mut response = Response::new(Body::from(html));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    // 与 pi 同一套 CSP：预览页只能内联样式、不允许脚本。
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; img-src data:; style-src 'unsafe-inline'; \
             base-uri 'none'; form-action 'none'; frame-ancestors 'self'",
        ),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// 流式返回原字节，支持 `Range`（音视频拖动进度条要用）。
fn stream_file(
    target: &Path,
    meta: &std::fs::Metadata,
    mime: &str,
    range: Option<String>,
    as_download: bool,
) -> Response {
    let size = meta.len();
    let target_str = target.to_string_lossy().to_string();
    let file_name = filemeta::base_name(&target_str);

    let (start, end, is_partial) = match range.as_deref().map(|raw| parse_range(raw, size)) {
        None => (0, size.saturating_sub(1), false),
        Some(Some((start, end))) => (start, end, true),
        Some(None) => {
            // 语法认识了但范围不合法：416 + `Content-Range: bytes */<size>`
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{size}"))
                    .unwrap_or_else(|_| HeaderValue::from_static("bytes */0")),
            );
            response
                .headers_mut()
                .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            return response;
        }
    };

    let Ok(bytes) = std::fs::read(target) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "read failed" })),
        )
            .into_response();
    };
    let slice = if size == 0 {
        Vec::new()
    } else {
        let start = start.min(bytes.len() as u64) as usize;
        let end = (end.saturating_add(1)).min(bytes.len() as u64) as usize;
        bytes[start..end.max(start)].to_vec()
    };
    let body_len = slice.len() as u64;

    let mut response = Response::new(Body::from(slice));
    if is_partial {
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
        if let Ok(value) = HeaderValue::from_str(&format!("bytes {start}-{end}/{size}")) {
            response.headers_mut().insert(header::CONTENT_RANGE, value);
        }
    }
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(mime) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Ok(value) = HeaderValue::from_str(&filemeta::content_disposition(file_name, as_download))
    {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(body_len));
    response
}

/// 解析 `Range` 头。`Some(None)` 表示「格式对但范围不可满足」。
fn parse_range(raw: &str, size: u64) -> Option<(u64, u64)> {
    let spec = raw.strip_prefix("bytes=")?;
    // 多段 Range 只认第一段（与 pi 的正则行为一致：多段时不匹配 → 416）。
    if spec.contains(',') {
        return None;
    }
    let (start_raw, end_raw) = spec.split_once('-')?;
    let start_raw = start_raw.trim();
    let end_raw = end_raw.trim();

    if size == 0 {
        return None;
    }

    let (start, end) = if start_raw.is_empty() {
        // `bytes=-500`：末尾 500 字节
        let suffix: u64 = end_raw.parse().ok()?;
        (size.saturating_sub(suffix), size - 1)
    } else {
        let start: u64 = start_raw.parse().ok()?;
        let end = if end_raw.is_empty() {
            size - 1
        } else {
            end_raw.parse::<u64>().ok()?
        };
        (start, end)
    };

    if start >= size || end < start {
        return None;
    }
    Some((start, end.min(size - 1)))
}

fn list_dir(target: &Path, hide_dot_dirs: bool) -> Response {
    let Ok(entries) = std::fs::read_dir(target) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "readdir failed" })),
        )
            .into_response();
    };
    let mut items: Vec<serde_json::Value> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            // 忽略名单与点开头过滤顺序与 pi 一致：先按名字过滤，再过 hideDotDirs。
            if IGNORED_NAMES.contains(&name.as_str())
                || IGNORED_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
            {
                return None;
            }
            if hide_dot_dirs && name.starts_with('.') {
                return None;
            }
            let path = entry.path();
            let meta = entry.metadata().ok();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            // 只有目录才付一次「是不是仓库根」的代价（pi 的 `hasGitMetadata`）。
            let git_repository = is_dir && path.join(".git").exists();
            let mut item = json!({
                "name": name,
                // pi 的前端读 `isDir`；`isDirectory` 是阶段 2 的旧字段名，留着兼容
                "isDir": is_dir,
                "isDirectory": is_dir,
                // pi 的 list 不 stat 文件，这两个字段恒为 0 / ""（前端也没读）
                "size": 0,
                "modified": "",
            });
            if git_repository {
                item["gitRepository"] = json!(true);
            }
            Some(item)
        })
        .collect();
    items.sort_by(|a, b| {
        let ad = a["isDir"].as_bool().unwrap_or(false);
        let bd = b["isDir"].as_bool().unwrap_or(false);
        bd.cmp(&ad).then_with(|| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b["name"].as_str().unwrap_or(""))
        })
    });
    Json(json!({
        "path": target.to_string_lossy(),
        "isDirectory": true,
        "entries": items,
    }))
    .into_response()
}

/// 文件签名：`mtime + ctime + inode + size`。
///
/// pi 用同一组字段判断「真的变了」，否则一次读取（会刷新 atime）就会触发下一轮
/// 重读，前端陷入自激刷新。
fn signature(path: &Path) -> Option<(i64, i64, u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.mtime(), meta.ctime(), meta.ino(), meta.len()))
}

/// `?type=watch`：每秒比对签名，变了就推一条命名事件 `change`。
///
/// 先发 `connected`：前端收到它才去快照当前内容，所以必须在监听就绪之后发，
/// 否则「连上到快照之间」的修改会丢。这里用轮询实现，等价位置在建好循环之后。
async fn watch(target: PathBuf) -> Response {
    let stream = async_stream::stream! {
        let mut last = signature(&target);
        let mut last_exists = last.is_some();
        yield Ok::<Event, Infallible>(
            Event::default()
                .event("connected")
                .data(json!({ "filePath": target.to_string_lossy() }).to_string()),
        );
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let current = signature(&target);
            match (&current, last_exists) {
                (Some(sig), _) if current != last => {
                    last = current;
                    last_exists = true;
                    let payload = json!({
                        "mtime": sig.0 * 1000,
                        "size": sig.3,
                    });
                    yield Ok::<Event, Infallible>(
                        Event::default().event("change").data(payload.to_string()),
                    );
                }
                (None, true) => {
                    // 文件被删：推一条 size=0，与 pi 的行为一致
                    last = None;
                    last_exists = false;
                    let payload = json!({
                        "mtime": crate::toiler_now_iso(),
                        "size": 0,
                    });
                    yield Ok::<Event, Infallible>(
                        Event::default().event("change").data(payload.to_string()),
                    );
                }
                _ => {}
            }
        }
    };
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)).text(""))
        .into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    headers.insert(
        header::HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response
}

// ============================================================================
// 写侧
// ============================================================================

/// `PUT /api/files/{path}`：保存编辑器里的内容。
pub async fn write(
    State(state): State<AppState>,
    _trusted: Trusted,
    UrlPath(path): UrlPath<String>,
    Query(params): Query<FileParams>,
    body: axum::body::Bytes,
) -> Response {
    // 前端把 `type=read` 带在 URL 上（同一个 `getFileApiUrl` 生成器），所以这里要看
    // type 时只校验「是 read 或缺省」，不强制。
    if matches!(params.kind.as_deref(), Some(kind) if kind != "read") {
        return bad_request("Invalid file write request type");
    }
    let target = match resolve(&state, &path) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if !authorize(&state, &target, &target) {
        return forbidden("Access denied");
    }
    let Ok(meta) = std::fs::metadata(&target) else {
        return not_found("Not found");
    };
    if !meta.is_file() {
        return bad_request("Not a file");
    }

    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return bad_request("Invalid JSON body");
    };
    let Some(content) = payload.get("content").and_then(|v| v.as_str()) else {
        return bad_request("content must be a string");
    };
    if content.len() > MAX_EDIT_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "error": "File too large to edit (>256KB)" })),
        )
            .into_response();
    }
    let allow_empty = payload
        .get("allowEmpty")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if content.is_empty() && meta.len() > 0 && !allow_empty {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "Empty overwrite blocked; explicit confirmation is required",
                "code": "empty_overwrite_blocked",
            })),
        )
            .into_response();
    }
    if let Err(error) = std::fs::write(&target, content) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response();
    }
    let size = std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
    Json(json!({ "size": size, "modified": crate::toiler_now_iso() })).into_response()
}

/// `PATCH /api/files/{path}`：重命名（`{name}` 只能是纯文件名）。
pub async fn rename(
    State(state): State<AppState>,
    _trusted: Trusted,
    UrlPath(path): UrlPath<String>,
    body: axum::body::Bytes,
) -> Response {
    let target = match resolve(&state, &path) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid file name", "code": "invalid_name" })),
        )
            .into_response();
    };
    let Some(name) = payload
        .get("name")
        .and_then(|v| v.as_str())
        .and_then(parse_rename_name)
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid file name", "code": "invalid_name" })),
        )
            .into_response();
    };

    let parent = target
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| target.clone());
    let destination = parent.join(&name);
    // 源与目标都要过两层校验：符号链接目录不能被改名操作带着逃出允许根。
    if !authorize(&state, &target, &target) || !authorize(&state, &destination, &parent) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "Access denied", "code": "access_denied" })),
        )
            .into_response();
    }
    if !target.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Not found", "code": "not_found" })),
        )
            .into_response();
    }
    if target
        .file_name()
        .map(|n| n == name.as_str())
        .unwrap_or(false)
    {
        return Json(json!({ "path": target.to_string_lossy(), "name": name })).into_response();
    }
    if destination.exists() {
        // 同一个 inode 的「改名成自己」不算冲突（大小写不敏感文件系统上会出现）。
        let same = same_entry(&target, &destination);
        if !same {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "A file with that name already exists",
                    "code": "already_exists",
                })),
            )
                .into_response();
        }
    }
    if let Err(error) = std::fs::rename(&target, &destination) {
        // rename 撞名在 unix 上是 EEXIST / ENOTEMPTY（后者 MSRV 1.78 还没有对应的
        // `ErrorKind` 变体），两者含义一致，用它自己的 raw code 判断最稳。
        const EEXIST: i32 = 17;
        const ENOTEMPTY: i32 = 66;
        let kind = error.kind();
        let raw = error.raw_os_error();
        if kind == std::io::ErrorKind::AlreadyExists
            || raw == Some(EEXIST)
            || raw == Some(ENOTEMPTY)
        {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "A file with that name already exists",
                    "code": "already_exists",
                })),
            )
                .into_response();
        }
        if kind == std::io::ErrorKind::NotFound {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "Not found", "code": "not_found" })),
            )
                .into_response();
        }
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string(), "code": "rename_failed" })),
        )
            .into_response();
    }
    Json(json!({
        "path": destination.to_string_lossy(),
        "name": name,
    }))
    .into_response()
}

fn same_entry(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (
        std::fs::symlink_metadata(left),
        std::fs::symlink_metadata(right),
    ) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// 重命名只接受纯文件名（pi 的 `parseFileRenameName`）。
fn parse_rename_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return None;
    }
    Some(name.to_string())
}

/// `POST /api/files/{path}?type=upload-check|upload`
pub async fn upload(
    State(state): State<AppState>,
    _trusted: Trusted,
    UrlPath(path): UrlPath<String>,
    Query(params): Query<FileParams>,
    request: axum::extract::Request,
) -> Response {
    if matches!(params.kind.as_deref(), Some(kind) if kind != "upload" && kind != "upload-check") {
        return bad_request("Invalid upload request type");
    }
    let target = match resolve(&state, &path) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if !authorize(&state, &target, &target) {
        return forbidden("Access denied");
    }
    let Ok(meta) = std::fs::metadata(&target) else {
        return not_found("Upload directory not found");
    };
    if !meta.is_dir() {
        return bad_request("Upload target is not a directory");
    }
    // 上传要写盘，所以必须用 realpath 判定：允许根里的符号链接不能把文件写到外面。
    state.sync_allowed_roots();
    let Ok(real_directory) = std::fs::canonicalize(&target) else {
        return not_found("Upload directory not found");
    };
    if !state.roots().allows_real(&real_directory) {
        return forbidden("Access denied");
    }

    if params.kind.as_deref() == Some("upload-check") {
        return upload_check(&state, &real_directory, request).await;
    }
    upload_receive(&state, &real_directory, request).await
}

/// 冲突检查：`POST ...?type=upload-check`
async fn upload_check(
    _state: &AppState,
    directory: &Path,
    request: axum::extract::Request,
) -> Response {
    let Ok(body) = axum::body::to_bytes(request.into_body(), 64 * 1024).await else {
        return bad_request("Request body too large");
    };
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return bad_request("fileNames must be an array of strings");
    };
    let Some(names) = payload.get("fileNames").and_then(|v| v.as_array()) else {
        return bad_request("fileNames must be an array of strings");
    };
    let mut file_names: Vec<String> = Vec::with_capacity(names.len());
    for name in names {
        let Some(name) = name.as_str() else {
            return bad_request("fileNames must be an array of strings");
        };
        file_names.push(name.to_string());
    }
    if let Some(message) = validate_upload_file_names(&file_names) {
        return bad_request(message);
    }
    Json(inspect_upload_targets(directory, &file_names)).into_response()
}

fn validate_upload_file_names(names: &[String]) -> Option<&'static str> {
    if names.is_empty() {
        return Some("No files selected");
    }
    let mut seen = std::collections::BTreeSet::new();
    for name in names {
        if name.is_empty() || name == "." || name == ".." || name.contains('\0') {
            return Some("Invalid file name");
        }
        if name.contains('/') || name.contains('\\') {
            return Some("File names must not contain a path");
        }
        if !seen.insert(name.clone()) {
            return Some("Duplicate file name in upload");
        }
    }
    None
}

fn inspect_upload_targets(directory: &Path, names: &[String]) -> serde_json::Value {
    let mut conflicts = Vec::new();
    let mut non_replaceable = Vec::new();
    for name in names {
        let destination = directory.join(name);
        let Ok(meta) = std::fs::symlink_metadata(&destination) else {
            continue;
        };
        conflicts.push(name.clone());
        // 目录与符号链接都不允许被上传覆盖。
        if !meta.is_file() || meta.file_type().is_symlink() {
            non_replaceable.push(name.clone());
        }
    }
    json!({ "conflicts": conflicts, "nonReplaceable": non_replaceable })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConflictStrategy {
    Error,
    Overwrite,
    Skip,
}

fn parse_conflict_strategy(raw: Option<&str>) -> Option<ConflictStrategy> {
    match raw.unwrap_or("error") {
        "error" => Some(ConflictStrategy::Error),
        "overwrite" => Some(ConflictStrategy::Overwrite),
        "skip" => Some(ConflictStrategy::Skip),
        _ => None,
    }
}

/// 收 multipart 上传：`POST ...?type=upload&conflict=error|overwrite|skip`
async fn upload_receive(
    state: &AppState,
    directory: &Path,
    request: axum::extract::Request,
) -> Response {
    let query = request.uri().query().unwrap_or("").to_string();
    let strategy_raw = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == "conflict")
        .map(|(_, value)| value.to_string());
    let Some(strategy) = parse_conflict_strategy(strategy_raw.as_deref()) else {
        return bad_request("Invalid conflict strategy");
    };

    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !content_type.starts_with("multipart/form-data") {
        return bad_request("Expected multipart/form-data");
    }

    let Ok(body) = axum::body::to_bytes(
        request.into_body(),
        // 边界与头部也算字节，给 1MB 余量（pi 的 `MAX_UPLOAD_REQUEST_BYTES`）
        (MAX_UPLOAD_TOTAL_BYTES + 1024 * 1024) as usize,
    )
    .await
    else {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "error": "Uploads must total 100MB or less" })),
        )
            .into_response();
    };

    // 复用 axum 的 multipart 解析：把已读完的 body 重新包成一个请求即可。
    let rebuilt = axum::http::Request::builder()
        .method(axum::http::Method::POST)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .unwrap_or_default();
    use axum::extract::FromRequest as _;
    let mut multipart = match Multipart::from_request(rebuilt, state).await {
        Ok(multipart) => multipart,
        Err(_) => return bad_request("Malformed multipart body"),
    };

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total: u64 = 0;
    loop {
        let next = multipart.next_field().await;
        let Ok(Some(field)) = next else { break };
        if field.name() != Some("files") {
            continue;
        }
        let Some(file_name) = field.file_name().map(str::to_string) else {
            continue;
        };
        let Ok(bytes) = field.bytes().await else {
            return bad_request("Malformed multipart body");
        };
        if bytes.len() as u64 > MAX_UPLOAD_FILE_BYTES {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({ "error": "Each upload must be 25MB or smaller" })),
            )
                .into_response();
        }
        total += bytes.len() as u64;
        if total > MAX_UPLOAD_TOTAL_BYTES {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({ "error": "Uploads must total 100MB or less" })),
            )
                .into_response();
        }
        files.push((file_name, bytes.to_vec()));
    }

    let names: Vec<String> = files.iter().map(|(name, _)| name.clone()).collect();
    if let Some(message) = validate_upload_file_names(&names) {
        return bad_request(message);
    }

    let inspection = inspect_upload_targets(directory, &names);
    let conflicts: Vec<String> = inspection["conflicts"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let non_replaceable: Vec<String> = inspection["nonReplaceable"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    if strategy == ConflictStrategy::Error && !conflicts.is_empty() {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "One or more files already exist",
                "conflicts": conflicts,
                "nonReplaceable": non_replaceable,
            })),
        )
            .into_response();
    }

    let mut uploaded: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut errors: Vec<serde_json::Value> = Vec::new();

    for (name, bytes) in files {
        let destination = directory.join(&name);
        let is_conflict = conflicts.contains(&name);
        if is_conflict && strategy == ConflictStrategy::Skip {
            skipped.push(name);
            continue;
        }
        if is_conflict && non_replaceable.contains(&name) {
            errors.push(json!({
                "name": name,
                "error": "Cannot replace a directory or symbolic link",
            }));
            continue;
        }
        if is_conflict {
            if let Err(error) = std::fs::remove_file(&destination) {
                errors.push(json!({ "name": name, "error": error.to_string() }));
                continue;
            }
        }
        // `wx` = 目标已存在就失败，避免并发上传互相覆盖。
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
        {
            Ok(mut handle) => {
                use std::io::Write;
                match handle.write_all(&bytes) {
                    Ok(()) => uploaded.push(name),
                    Err(error) => errors.push(json!({ "name": name, "error": error.to_string() })),
                }
            }
            Err(error) => errors.push(json!({ "name": name, "error": error.to_string() })),
        }
    }

    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        // 207 Multi-Status：部分成功（pi 的同一行为，前端按 `errors` 逐条提示）
        StatusCode::MULTI_STATUS
    };
    (
        status,
        Json(json!({
            "uploaded": uploaded,
            "skipped": skipped,
            "errors": errors,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parsing_covers_all_three_forms() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=900-", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        // 末尾越界会被夹到 size-1
        assert_eq!(parse_range("bytes=900-9999", 1000), Some((900, 999)));
        // 起点越界 / 反序 / 多段 / 空文件 → 不可满足
        assert_eq!(parse_range("bytes=1000-", 1000), None);
        assert_eq!(parse_range("bytes=10-5", 1000), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 1000), None);
        assert_eq!(parse_range("bytes=0-1", 0), None);
        assert_eq!(parse_range("items=0-1", 1000), None);
    }

    #[test]
    fn rename_name_rejects_paths() {
        assert_eq!(parse_rename_name("a.rs"), Some("a.rs".to_string()));
        assert_eq!(parse_rename_name("  a.rs  "), Some("a.rs".to_string()));
        assert_eq!(parse_rename_name("../a.rs"), None);
        assert_eq!(parse_rename_name("x/a.rs"), None);
        assert_eq!(parse_rename_name(r"x\a.rs"), None);
        assert_eq!(parse_rename_name("."), None);
        assert_eq!(parse_rename_name(""), None);
    }

    #[test]
    fn upload_name_validation_matches_pi() {
        assert_eq!(validate_upload_file_names(&[]), Some("No files selected"));
        assert_eq!(
            validate_upload_file_names(&["a.txt".to_string(), "a.txt".to_string()]),
            Some("Duplicate file name in upload")
        );
        assert_eq!(
            validate_upload_file_names(&["a/b.txt".to_string()]),
            Some("File names must not contain a path")
        );
        assert_eq!(
            validate_upload_file_names(&["a.txt".to_string(), "b.txt".to_string()]),
            None
        );
    }

    #[test]
    fn conflict_strategy_defaults_to_error() {
        assert_eq!(parse_conflict_strategy(None), Some(ConflictStrategy::Error));
        assert_eq!(
            parse_conflict_strategy(Some("skip")),
            Some(ConflictStrategy::Skip)
        );
        assert_eq!(
            parse_conflict_strategy(Some("overwrite")),
            Some(ConflictStrategy::Overwrite)
        );
        assert_eq!(parse_conflict_strategy(Some("bogus")), None);
    }

    #[test]
    fn session_path_reference_needs_boundaries() {
        assert!(contains_exact_path("读 /a/b.rs 了吗", "/a/b.rs"));
        assert!(contains_exact_path("file:///a/b.rs", "/a/b.rs"));
        assert!(contains_exact_path("/a/b.rs:12:5", "/a/b.rs"));
        // 后面还跟着路径字符 → 不是同一个文件
        assert!(!contains_exact_path("/a/b.rs.bak", "/a/b.rs"));
        // 前面还跟着路径字符 → 同前缀的另一个文件
        assert!(!contains_exact_path("/x/a/b.rs", "/a/b.rs"));
    }

    #[test]
    fn session_id_shape_is_validated() {
        assert!(is_valid_session_id("01a1c511-ef4b-7516-b8ed-33bd756700bc"));
        assert!(!is_valid_session_id("nope"));
        assert!(!is_valid_session_id(
            "01a1c511-ef4b-7516-b8ed-33bd756700bczz"
        ));
    }
}
