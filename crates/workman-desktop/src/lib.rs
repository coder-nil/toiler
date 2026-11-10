//! workman 的桌面壳（阶段 4：单进程）。
//!
//! 与 pi-desktop 的差别只有一处，但很关键：**没有子进程**。
//!
//! | | pi-desktop | workman |
//! |---|---|---|
//! | 后端 | `Command::new(node)` 拉起 Next server | 同一进程里的 axum（Tauri 的 tokio runtime） |
//! | 前端 | 打包的 `.next` + `node_modules` | `include_dir!` 嵌进二进制的静态产物 |
//! | 启动 | 轮询 45 秒等端口就绪 | `bind` 后立刻可用（端口由内核分配） |
//! | 安装包 | 带 `runtime/node`（100MB+） | 只有可执行文件 |
//!
//! 手机访问（局域网反向代理 + Cloudflare 隧道）原样保留，见 [`mobile`]。

use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

mod lan_proxy;
mod mobile;
pub mod server;
mod tunnel;

use mobile::MobileAccess;
use server::DesktopServer;

/// 内嵌服务的句柄（退出时优雅关闭）。
struct ServerState(Mutex<Option<DesktopServer>>);

/// 打开外部链接只允许 http/https，别的一律拒绝（前端也能传 `file://`）。
fn safe_external_url(raw: &str) -> Result<String, String> {
    let url = raw.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("只允许打开 http/https 链接：{url}"));
    }
    Ok(url.to_string())
}

#[tauri::command]
fn hide_startup_overlay() -> Result<(), String> {
    // workman 没有原生启动遮罩（不需要等 node 起来），保留这个命令是为了让
    // pi 前端的 `pi-app-ready` 回调不至于报错。
    Ok(())
}

#[tauri::command]
fn open_external_url(url: String) -> Result<(), String> {
    open_url(&safe_external_url(&url)?)
}

#[tauri::command]
fn open_release_url(url: String) -> Result<(), String> {
    open_url(&safe_external_url(&url)?)
}

fn open_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        // `start` 是 cmd 内建命令，必须经由 cmd /C 调用。
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("打开链接失败：{error}"))
}

// 终端面板（PTY）是 pi 桌面独有的功能，阶段 4 没有移植：命令存在但明确报错，
// 前端的 TerminalPanel 会把原因显示出来，而不是静默失效。
#[tauri::command]
fn terminal_start() -> Result<(), String> {
    Err("workman 尚未提供内置终端（阶段 5 再评估）".to_string())
}

#[tauri::command]
fn terminal_write() -> Result<(), String> {
    Err("workman 尚未提供内置终端（阶段 5 再评估）".to_string())
}

#[tauri::command]
fn terminal_resize() -> Result<(), String> {
    Err("workman 尚未提供内置终端（阶段 5 再评估）".to_string())
}

#[tauri::command]
fn terminal_close() -> Result<(), String> {
    // 关闭是幂等的：没开过终端时也当成功。
    Ok(())
}

/// 创建主窗口：直接加载内嵌服务的地址（同源，`/api` 与前端一个端口）。
fn create_main_window(app: &tauri::AppHandle, server: &DesktopServer) -> Result<(), String> {
    let base = server.base_url();
    let url = base
        .parse()
        .map_err(|error| format!("应用地址不合法 {base}：{error}"))?;
    let origin = serde_json::to_string(&base).map_err(|error| error.to_string())?;

    // pi 前端靠 `__PI_WEB_API_ORIGIN__` 把 `/api/...` 改写成绝对地址；
    // 同源部署下它就是自己的 origin，语义与 pi 桌面态一致。
    let script = format!(
        "window.__PI_WEB_API_ORIGIN__ = {origin};\
         window.__PI_WEB_DESKTOP__ = true;\
         window.__WORKMAN_DESKTOP__ = true;\
         window.addEventListener('pi-app-ready', function () {{\
           if (window.__TAURI_INTERNALS__) {{\
             window.__TAURI_INTERNALS__.invoke('hide_startup_overlay').catch(function () {{}});\
           }}\
         }}, {{ once: true }});"
    );

    WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
        .title("workman")
        .inner_size(1280.0, 820.0)
        .min_inner_size(720.0, 520.0)
        .initialization_script(script)
        .build()
        .map(|_| ())
        .map_err(|error| format!("创建窗口失败：{error}"))
}

fn shutdown(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<ServerState>() {
        if let Ok(mut guard) = state.0.lock() {
            if let Some(mut server) = guard.take() {
                server.stop();
            }
        }
    }
    if let Some(state) = app.try_state::<MobileAccess>() {
        state.stop();
    }
}

/// SIGTERM / SIGINT 也要走清理：否则 `cloudflared` 会变成孤儿，继续把公网入口
/// 挂在半死的状态上（tunnel 会在下次启动回收，但立刻回收更干净）。
#[cfg(unix)]
fn spawn_signal_watcher(handle: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let Ok(mut terminate) = signal(SignalKind::terminate()) else {
            return;
        };
        let Ok(mut interrupt) = signal(SignalKind::interrupt()) else {
            return;
        };
        tokio::select! {
            _ = terminate.recv() => tracing::info!("收到 SIGTERM，开始回收"),
            _ = interrupt.recv() => tracing::info!("收到 SIGINT，开始回收"),
        }
        shutdown(&handle);
        handle.exit(0);
    });
}

#[cfg(not(unix))]
fn spawn_signal_watcher(_handle: tauri::AppHandle) {}

/// 应用入口（`main.rs` 调它）。
pub fn run() {
    workman_server::boot::init_tracing();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .manage(ServerState(Mutex::new(None)))
        .manage(MobileAccess::default())
        .invoke_handler(tauri::generate_handler![
            hide_startup_overlay,
            open_release_url,
            open_external_url,
            terminal_start,
            terminal_write,
            terminal_resize,
            terminal_close,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let server = match server::start() {
                Ok(server) => server,
                Err(error) => {
                    show_fatal(&handle, &error);
                    return Err(error.into());
                }
            };
            let port = server.port;
            if let Some(state) = handle.try_state::<ServerState>() {
                if let Ok(mut guard) = state.0.lock() {
                    *guard = Some(server);
                }
            }

            let window_result = {
                let state = handle.state::<ServerState>();
                let guard = state.0.lock().map_err(|_| "服务状态不可用")?;
                let server = guard.as_ref().ok_or("服务未能启动")?;
                create_main_window(&handle, server)
            };
            if let Err(error) = window_result {
                show_fatal(&handle, &error);
                return Err(error.into());
            }

            mobile::start(&handle, port);
            spawn_signal_watcher(handle.clone());
            tracing::info!("workman 桌面窗口已创建（端口 {port}）");
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("构建 Tauri 应用失败");

    app.run(|handle, event| match event {
        RunEvent::ExitRequested { .. } | RunEvent::Exit => shutdown(handle),
        _ => {}
    });
}

/// 启动失败时不要静默退出：弹一个最小窗口说明原因（日志里也有完整信息）。
fn show_fatal(app: &tauri::AppHandle, message: &str) {
    tracing::error!("启动失败：{message}");
    let Ok(escaped) = serde_json::to_string(message) else {
        return;
    };
    let _ = WebviewWindowBuilder::new(app, "startup-error", WebviewUrl::App("startup-error.html".into()))
        .title("workman 启动失败")
        .inner_size(620.0, 380.0)
        .initialization_script(format!("window.__WORKMAN_STARTUP_ERROR__ = {escaped};"))
        .build();
}

// ---------------------------------------------------------------------------
// 子进程工具（tunnel.rs 用）
// ---------------------------------------------------------------------------

#[cfg(unix)]
pub(crate) fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
pub(crate) fn configure_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 先礼后兵：SIGTERM 一组 → 超时 SIGKILL 整组。
pub(crate) fn terminate_process(child: &mut Child, timeout: Duration) {
    request_graceful_shutdown(child);
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break,
        }
    }
    force_kill(child);
    let _ = child.wait();
}

#[cfg(unix)]
fn request_graceful_shutdown(child: &Child) {
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGTERM);
    }
}

#[cfg(windows)]
fn request_graceful_shutdown(child: &Child) {
    let _ = hidden_command("taskkill")
        .args(["/PID", &child.id().to_string(), "/T"])
        .status();
}

#[cfg(unix)]
fn force_kill(child: &mut Child) {
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
}

#[cfg(windows)]
fn force_kill(child: &mut Child) {
    let _ = hidden_command("taskkill")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .status();
    let _ = child.kill();
}

#[cfg(windows)]
fn hidden_command(program: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_urls_must_be_http() {
        assert!(safe_external_url("https://example.com/a").is_ok());
        assert!(safe_external_url("http://127.0.0.1:3000").is_ok());
        assert!(safe_external_url("file:///etc/passwd").is_err());
        assert!(safe_external_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn server_state_starts_empty() {
        let state = ServerState(Mutex::new(None));
        assert!(state.0.lock().unwrap().is_none());
    }

    #[test]
    fn agent_dir_is_absolute() {
        // mobile::start 用的是 workman-server 的 agent 目录解析，
        // 这里只确认它是个绝对路径（不会写到进程当前目录下）。
        let dir = workman_server::paths::agent_dir();
        assert!(dir.is_absolute(), "{} 应当是绝对路径", dir.display());
    }
}
