//! 静态前端托管：把 pi 的前端产物在编译期嵌进二进制，由 axum 同源提供。
//!
//! 阶段 4 的要点是「一个进程」：以前是 Tauri 壳 + 打包的 `node` 跑 Next server，
//! 现在前端产物用 [`include_dir!`] 变成二进制里的常量，`/api` 与静态资源同一个
//! 监听端口，WebView 直接开 `http://127.0.0.1:<port>/` —— 同源，SSE 与 fetch 都不
//! 用再处理跨域。
//!
//! 产物目录在编译期由 `WORKMAN_FRONTEND_DIR` 决定（见 `build.rs`），默认是 crate
//! 内置的 `frontend/`（占位页）。pi 的 Next 静态导出产物形状是：
//!
//! ```text
//! index.html  index.txt  __next.__PAGE__.txt  _next/static/...  icons/...
//! ```
//!
//! 其中 `.txt` 是 RSC（React Server Component）载荷：客户端导航时 Next 会请求
//! `/some/route` 并带 `RSC: 1` 头（或 `?_rsc=`），期望拿回对应的 `.txt`。所以下面
//! 的兜底顺序是「精确文件 → 目录 index → RSC 载荷 → index.html（客户端路由接管）
//! → 404」。

use std::path::Path;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use include_dir::{include_dir, Dir};

/// 编译期嵌入的前端产物（`build.rs` 通过 `rustc-env` 注入绝对路径）。
///
/// `include_dir!` 只接受字符串字面量，环境变量用它的 `$NAME` 插值语法。
static FRONTEND: Dir<'static> = include_dir!("$WORKMAN_FRONTEND_DIR");

/// 产物目录的来源，用于日志和 `/api/home` 一侧的自检。
pub const FRONTEND_DIR: &str = env!("WORKMAN_FRONTEND_DIR");

/// 嵌入的产物是否只是一个占位页（没有真实前端）。
pub fn is_placeholder() -> bool {
    FRONTEND.get_file("index.html").is_some() && FRONTEND.get_dir("_next").is_none()
}

/// 探针路径：给「服务已就绪」的判断用。
pub fn has_entry() -> bool {
    FRONTEND.get_file("index.html").is_some()
}

fn content_type(path: &str) -> &'static str {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" => "text/html; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "webmanifest" => "application/manifest+json",
        "wasm" => "application/wasm",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

/// `_next/static` 带内容哈希，永久缓存；HTML 一律不缓存（升级后立刻生效）。
fn cache_control(path: &str) -> &'static str {
    if path.starts_with("_next/static/") || path.starts_with("icons/") {
        "public, max-age=31536000, immutable"
    } else if path.ends_with(".html") || path.is_empty() {
        "no-cache"
    } else {
        "public, max-age=3600"
    }
}

/// 只解 `%XX`，`+` 保持原样（路径语义）。
fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let value = u8::from_str_radix(hex, 16).ok()?;
            out.push(value);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// 把请求路径规范化成产物内的相对路径。目录穿越、绝对路径、空段一律拒绝。
fn normalize(raw: &str) -> Option<String> {
    let decoded = percent_decode(raw)?;
    let mut parts: Vec<&str> = Vec::new();
    for segment in decoded.split('/') {
        match segment {
            "" | "." => continue,
            ".." => return None,
            other => {
                if other.contains('\0') || other.contains('\\') {
                    return None;
                }
                parts.push(other);
            }
        }
    }
    Some(parts.join("/"))
}

fn body_of(path: &str) -> Option<Response> {
    let file = FRONTEND.get_file(path)?;
    let mut response = Response::new(Body::from(file.contents()));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(path)),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control(path)),
    );
    Some(response)
}

fn html_of(path: &str) -> Option<Response> {
    body_of(path)
}

fn not_found(path: &str) -> Response {
    if path.starts_with("api/") || path == "api" {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "Not found" })),
        )
            .into_response();
    }
    // 单独维护的 404 页（pi 的静态导出里有），否则退回正文里的最小页。
    if let Some(response) = html_of("404.html") {
        return response;
    }
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        "<!doctype html><meta charset=\"utf-8\"><title>404</title><h1>404</h1>",
    )
        .into_response()
}

/// RSC 请求（客户端路由导航）要的是数据而不是 HTML。
fn wants_rsc(headers: &HeaderMap, uri: &Uri) -> bool {
    if headers
        .get("rsc")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("1"))
        .unwrap_or(false)
    {
        return true;
    }
    uri.query()
        .map(|q| q.split('&').any(|part| part.starts_with("_rsc=")))
        .unwrap_or(false)
}

fn is_html_request(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("text/html") || v.contains("*/*"))
        .unwrap_or(false)
}

/// RSC 载荷候选：Next 静态导出把每个路由的 RSC 数据存成同名 `.txt`。
fn rsc_candidates(path: &str) -> [String; 2] {
    [format!("{path}.txt"), format!("{path}/index.txt")]
}

/// 静态资源 handler（挂在 `Router::fallback` 上：`/api/*` 命中具体路由，其余归这里）。
pub async fn serve_static(uri: Uri, headers: HeaderMap) -> Response {
    let raw = uri.path();
    let Some(path) = normalize(raw) else {
        return not_found("");
    };

    // 0) 根路径：RSC 请求要 `index.txt`（Next 客户端预取），其余给 index.html
    if path.is_empty() {
        if wants_rsc(&headers, &uri) {
            if let Some(response) = body_of("index.txt") {
                return response;
            }
        }
        return html_of("index.html").unwrap_or_else(|| not_found(""));
    }

    // `/api` 命名空间已经全归具体路由；落到这里就是真的没有这个接口，
    // 必须 404 JSON，不能被下面的 SPA 兜底成 index.html。
    if path == "api" || path.starts_with("api/") {
        return not_found(&path);
    }

    // 1) 精确文件
    if let Some(response) = body_of(&path) {
        return response;
    }
    // 2) 目录 → index.html
    if let Some(response) = html_of(&format!("{path}/index.html")) {
        return response;
    }

    // 3) RSC 载荷：`/route` → `/route.txt`（Next 静态导出的形状）
    if wants_rsc(&headers, &uri) {
        for candidate in rsc_candidates(&path) {
            if let Some(response) = body_of(&candidate) {
                return response;
            }
        }
    }

    // 4) 客户端路由接管：无扩展名的 GET 一律给 index.html
    if Path::new(&path).extension().is_none() && is_html_request(&headers) {
        if let Some(response) = html_of("index.html") {
            return response;
        }
    }

    not_found(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rejects_traversal() {
        assert_eq!(normalize("/a/b").as_deref(), Some("a/b"));
        assert_eq!(normalize("/a//b/").as_deref(), Some("a/b"));
        assert_eq!(normalize("/../etc/passwd"), None);
        assert_eq!(normalize("/a/../../b"), None);
        assert_eq!(normalize("/a%2Fb").as_deref(), Some("a/b"));
        assert_eq!(normalize("/%2e%2e/x"), None);
        assert_eq!(normalize("/a%5Cb"), None, "反斜杠一律拒");
        assert_eq!(normalize("/a%zz"), None);
    }

    #[test]
    fn content_types_cover_next_outputs() {
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(
            content_type("_next/static/chunks/a.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type("manifest.webmanifest"),
            "application/manifest+json"
        );
        assert_eq!(content_type("_next/static/media/a.woff2"), "font/woff2");
    }

    #[test]
    fn cache_policy_is_immutable_for_hashed_assets() {
        assert!(cache_control("_next/static/chunks/a.js").contains("immutable"));
        assert_eq!(cache_control("index.html"), "no-cache");
    }

    #[test]
    fn rsc_candidates_are_next_shaped() {
        assert_eq!(
            rsc_candidates("settings"),
            ["settings.txt", "settings/index.txt"]
        );
        assert_eq!(rsc_candidates("a/b"), ["a/b.txt", "a/b/index.txt"]);
    }

    #[test]
    fn embedded_frontend_has_entry() {
        assert!(has_entry(), "嵌入目录 {} 缺少 index.html", FRONTEND_DIR);
    }
}
