//! `POST /api/files/reveal` —— 在系统文件管理器里显示一个文件或目录。
//!
//! 契约来源是 pi 的 `app/api/files/reveal/route.ts`（编译产物见
//! `tools/pi-contract/raw/files_reveal.js`），判定顺序逐条对齐：
//!
//! 1. 绝对路径、不含 NUL → 否则 400 `Invalid file path`
//! 2. 允许根词法包含，或「会话正文里引用过这个路径」→ 否则 403 `Access denied`
//! 3. 目标必须存在 → 否则 404 `Not found`
//! 4. 非引用授权时还要过 realpath 包含（挡住允许根里的符号链接逃逸）→ 否则 403
//! 5. 交给系统文件管理器：macOS `open -R`、Windows `explorer /select,`、Linux `xdg-open`
//!
//! # 与 pi 的差异
//!
//! | | pi | toiler |
//! |---|---|---|
//! | 手机 / 隧道 | 不区分，远端也能触发本机文件管理器 | 代理注入 `x-toiler-remote-client`，见到即 403 |
//! | 命令超时 | `execFile` 的 `timeout: 10s` | 同样是 10 秒，超时直接 kill |
//! | 失败文案 | `Could not open file manager` | 中文说明（前端直接弹给用户看） |
//!
//! ## 为什么不能靠 `Origin` 判断是不是远端
//!
//! `/api` 与手机、Cloudflare 隧道共用一个端口。`lan_proxy` 会给手机与隧道的
//! 请求把 `Host` / `Origin` 改写成 `127.0.0.1:<port>`（见 `lan_proxy.rs` 的
//! `rewrite_request_head`），与桌面窗口发出的值逐字节相同 —— 服务端仅凭
//! Host/Origin 分不出「窗口」和「别人手机」。所以判据由代理自己写入的标记头
//! 承担：代理转发时**追加**该头（先删掉客户端自带的同名头），客户端无法用它
//! 换到放行。

use std::path::Path;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::security::Trusted;
use crate::state::AppState;

/// `lan_proxy` 注入的「这个请求来自手机或公网隧道」标记。
pub const REMOTE_CLIENT_HEADER: &str = "x-toiler-remote-client";

/// 打开文件管理器的超时（pi 的 `execFile` 选项里写的是 10 秒）。
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// 测试与非常规安装可以指定替代的系统文件管理器命令。
///
/// 存在的唯一理由是测试：真正执行 `open -R` 会在开发机上弹出 Finder 窗口，
/// 让 `cargo test` 变成一次骚扰。产品路径不读这个变量。
const FILE_MANAGER_ENV: &str = "TOILER_FILE_MANAGER";

#[derive(Debug, Deserialize, Default)]
pub struct RevealRequest {
    #[serde(rename = "filePath")]
    file_path: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}

pub async fn reveal(
    State(state): State<AppState>,
    _trusted: Trusted,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // 闸门一：远端（手机 / 隧道）一律拒绝。见文件头「为什么不能靠 Origin 判断」。
    if headers.contains_key(REMOTE_CLIENT_HEADER) {
        return forbidden("不支持从手机或公网隧道打开本机文件管理器");
    }

    let Ok(payload) = serde_json::from_slice::<RevealRequest>(&body) else {
        return bad_request("Invalid JSON");
    };
    let Some(raw) = payload.file_path.as_deref() else {
        return bad_request("Invalid file path");
    };
    if !crate::paths::is_absolute_path(raw) || raw.contains('\0') {
        return bad_request("Invalid file path");
    }
    let target = crate::paths::lexical_normalize(Path::new(raw));

    // 授权与 `/api/files` 的 GET 一致：允许根（词法）或会话引用。
    state.sync_allowed_roots();
    let within_roots = state.roots().allows(&target);
    let by_reference = !within_roots
        && crate::routes::files::referenced_by_session(
            &state,
            payload.session_id.as_deref(),
            &target,
        );
    if !within_roots && !by_reference {
        return forbidden("Access denied");
    }

    let Ok(meta) = std::fs::metadata(&target) else {
        return not_found("Not found");
    };
    // 符号链接不能把「允许根内的路径」变成根外的路径；会话引用授权除外（pi 同语义）。
    if !by_reference && !state.roots().allows_real(&target) {
        return forbidden("Access denied");
    }

    let Some((program, args)) = opener_command(&target, meta.is_dir()) else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({ "error": "当前系统不支持打开文件管理器" })),
        )
            .into_response();
    };
    let program = std::env::var(FILE_MANAGER_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or(program);

    match run_opener(&program, &args).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("无法打开系统文件管理器：{error}") })),
        )
            .into_response(),
    }
}

/// 执行一次打开动作，超时即 kill（对应 pi 的 `timeout: 1e4, windowsHide: true`）。
async fn run_opener(program: &str, args: &[String]) -> std::io::Result<()> {
    let mut child = crate::process::tokio_command(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    match tokio::time::timeout(OPEN_TIMEOUT, child.wait()).await {
        Ok(status) => {
            let status = status?;
            if status.success() {
                Ok(())
            } else {
                Err(std::io::Error::other(format!("命令退出码 {status}")))
            }
        }
        Err(_) => {
            let _ = child.start_kill();
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "等待文件管理器超时",
            ))
        }
    }
}

/// 各平台「显示这个文件」的命令与参数。
///
/// 文件与目录分开：`open -R <file>` 是「在 Finder 里选中这个文件」，而目录要
/// 直接打开窗口，否则用户看到的是目录的父级。
#[cfg(target_os = "macos")]
fn opener_command(target: &Path, is_dir: bool) -> Option<(String, Vec<String>)> {
    let path = target.to_string_lossy().to_string();
    let args = if is_dir {
        vec![path]
    } else {
        vec!["-R".to_string(), path]
    };
    Some(("/usr/bin/open".to_string(), args))
}

#[cfg(target_os = "windows")]
fn opener_command(target: &Path, is_dir: bool) -> Option<(String, Vec<String>)> {
    let path = target.to_string_lossy().to_string();
    let args = if is_dir {
        vec![path]
    } else {
        // 注意必须是 `/select,` 带逗号并且单独作为一个参数：explorer 的解析很挑剔。
        vec!["/select,".to_string(), path]
    };
    Some(("explorer.exe".to_string(), args))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn opener_command(target: &Path, is_dir: bool) -> Option<(String, Vec<String>)> {
    let chosen = if is_dir {
        target.to_path_buf()
    } else {
        // xdg-open 没有「选中文件」的概念，退到父目录（与 pi 一致）。
        target
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| target.to_path_buf())
    };
    Some((
        "xdg-open".to_string(),
        vec![chosen.to_string_lossy().to_string()],
    ))
}

#[cfg(not(any(unix, windows)))]
fn opener_command(_target: &Path, _is_dir: bool) -> Option<(String, Vec<String>)> {
    None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_selects_files_and_opens_directories() {
        let file = Path::new("/tmp/notes.md");
        let (program, args) = opener_command(file, false).unwrap();
        assert_eq!(program, "/usr/bin/open");
        assert_eq!(args, vec!["-R".to_string(), "/tmp/notes.md".to_string()]);

        let (_, dir_args) = opener_command(Path::new("/tmp"), true).unwrap();
        assert_eq!(dir_args, vec!["/tmp".to_string()], "目录要直接打开窗口");
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn xdg_open_falls_back_to_the_parent_directory() {
        let (program, args) = opener_command(Path::new("/tmp/a/notes.md"), false).unwrap();
        assert_eq!(program, "xdg-open");
        assert_eq!(args, vec!["/tmp/a".to_string()]);
    }

    /// 超时与失败都必须变成 `Err`，否则前端会以为窗口已经弹出来了。
    #[tokio::test]
    async fn failures_are_reported_not_swallowed() {
        let error = run_opener("/nonexistent/file-manager", &[])
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);

        let failure = run_opener("/bin/sh", &["-c".to_string(), "exit 3".to_string()])
            .await
            .unwrap_err();
        assert!(failure.to_string().contains('3'), "{failure}");
    }
}
