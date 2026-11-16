//! toiler 的桌面壳（阶段 4：单进程）。
//!
//! 与 pi-desktop 的差别只有一处，但很关键：**没有子进程**。
//!
//! | | pi-desktop | toiler |
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
mod terminal;
mod tunnel;

use mobile::MobileAccess;
use server::DesktopServer;

/// Tauri 的 WebView 在 macOS/Linux 上把页面 origin 报成 `tauri://localhost`，
/// Windows 上则是 `http://tauri.localhost`。服务侧的 origin 校验必须认这个值，
/// 否则壳里的 `/api` 请求全被判成跨站（pi 用同一对常量）。
#[cfg(windows)]
const DESKTOP_API_ORIGIN: &str = "http://tauri.localhost";
#[cfg(not(windows))]
const DESKTOP_API_ORIGIN: &str = "tauri://localhost";

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
    // toiler 没有原生启动遮罩（不需要等 node 起来），保留这个命令是为了让
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

/// 顶栏高度（逻辑点）。与前端 `CHAT_TOP_BAR_HEIGHT` 是同一个值，
/// 红绿灯的圆心要对到它的中线，所以只能在这里也写一份并注明出处。
#[cfg(target_os = "macos")]
const TITLEBAR_HEIGHT: f64 = 42.0;

/// macOS：把三个红绿灯的**圆心**放到距窗口顶边 `center_from_top` 的位置。
///
/// 为什么不用 `traffic_light_position`：wry 的实现是“拉高 titlebar 容器”，
/// 按钮在容器内仍然走 AppKit 的默认布局，最终圆心落在哪要反推，而且建窗之后
/// 系统还会再布一次局。这里直接把标准按钮的 frame 算出来摆，结果可预期，
/// 并且把摆完的真实坐标打出来，不用再猜。
#[cfg(target_os = "macos")]
fn place_traffic_lights(
    window: &tauri::WebviewWindow,
    center_from_top: f64,
) -> Result<String, String> {
    use objc2_app_kit::{NSWindow, NSWindowButton};

    let raw = window.ns_window().map_err(|error| error.to_string())?;
    // SAFETY: `ns_window` 返回的就是这个窗口的 NSWindow 指针；调用方保证在主线程。
    let ns_window: &NSWindow = unsafe { &*(raw as *mut NSWindow) };
    let window_height = ns_window.frame().size.height;
    let content_view = ns_window
        .contentView()
        .ok_or_else(|| "窗口没有 contentView".to_string())?;

    // 目标圆心距窗口底（contentView 左下原点）的距离。
    let target_from_bottom = window_height - center_from_top;

    let mut report = format!("窗口高 {window_height:.1}，目标圆心距顶 {center_from_top:.1}：");
    for (index, kind) in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ]
    .into_iter()
    .enumerate()
    {
        let Some(button) = ns_window.standardWindowButton(kind) else {
            report.push_str(&format!(" [{index}] 缺失"));
            continue;
        };
        // 关键：frame 是相对**父视图**（titlebar 容器）的，不能拿窗口高度直接赋值。
        // 先把按钮的实际位置换算到窗口坐标，再按差值平移。
        let in_window = button.convertRect_toView(button.bounds(), Some(&content_view));
        let current_center = in_window.origin.y + in_window.size.height / 2.0;
        let mut frame = button.frame();
        frame.origin.y += target_from_bottom - current_center;
        button.setFrameOrigin(frame.origin);

        let after = button.convertRect_toView(button.bounds(), Some(&content_view));
        let after_center = after.origin.y + after.size.height / 2.0;
        report.push_str(&format!(
            " [{index}] 高{:.1} 圆心距顶 {:.1}->{:.1}",
            in_window.size.height,
            window_height - current_center,
            window_height - after_center
        ));
    }
    Ok(report)
}

/// 建窗之后再摆一次：AppKit 会在窗口上屏时重排标题栏，建窗那一下设的会被盖掉。
/// 两次都写日志，方便在 `/tmp/toiler-desktop.log` 里对票。
#[cfg(target_os = "macos")]
fn schedule_traffic_lights(app: &tauri::AppHandle) {
    let handle = app.clone();
    std::thread::spawn(move || {
        for (attempt, delay) in [(1, 0u64), (2, 300)] {
            if delay > 0 {
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
            let inner = handle.clone();
            let _ = handle.run_on_main_thread(move || {
                if let Some(window) = inner.get_webview_window("main") {
                    match place_traffic_lights(&window, TITLEBAR_HEIGHT / 2.0) {
                        Ok(report) => tracing::info!("红绿灯（第 {attempt} 次）：{report}"),
                        Err(error) => tracing::warn!("红绿灯（第 {attempt} 次）失败：{error}"),
                    }
                }
            });
        }
    });
}

/// release 关掉 WebView 自带的右键菜单（「重新载入 / 后退 / 检查元素」那一套）。
///
/// 桌面 App 里这几个条目是纯负担：`重新载入` 会把整页刷掉，正在跑的那一轮对话
/// 直接从界面上消失；`检查元素` 对最终用户没有意义。开发版（debug）保留，调试时
/// 还要靠它刷新和看 DOM。
///
/// 两个例外必须放行：
/// * 输入框 / 文本域 / contenteditable —— 那里的右键是系统级的复制粘贴菜单；
/// * 内置终端（`.xterm`）—— xterm 自己不接管右键，去掉原生菜单就只剩快捷键盘能
///   粘贴，终端里右键粘贴是习惯动作；
/// * 会话列表行 —— 它自己派发自定义右键菜单（`lib/session-row-context-menu.ts`）。
///   监听挂的是捕获阶段的 `preventDefault`，只会取消原生默认动作，不会
///   `stopPropagation`，所以 React 的 `onContextMenu` 照常触发，自定义菜单不受影响。
///
/// 注入脚本一律写成多行常量，不用字符串行尾续行拼接：行尾 `\` 会连同换行一起
/// 吃掉，脚本一旦被压成一行，里面的 `//` 注释就会吃掉后续所有代码（导出页踩过
/// 这个坑，注释一律用 `/* */`）。
const SUPPRESS_CONTEXT_MENU_SCRIPT: &str = r#"
(function () {
  function isEditable(target) {
    if (!target || target.nodeType !== 1) return false;
    var tag = target.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
    if (target.isContentEditable) return true;
    if (target.closest(".xterm")) return true;
    return !!target.closest("input, textarea, select, [contenteditable='true'], [contenteditable='']");
  }
  window.addEventListener("contextmenu", function (event) {
    if (isEditable(event.target)) return;
    event.preventDefault();
  }, true);
})();
"#;

/// 只有 release 注入：开发版留着原生右键（刷新 / 检查元素）。
fn context_menu_guard_script() -> Option<&'static str> {
    if cfg!(debug_assertions) {
        None
    } else {
        Some(SUPPRESS_CONTEXT_MENU_SCRIPT)
    }
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
    //
    // `__TOILER_OVERLAY_TITLEBAR__` 告诉前端：这条进程的系统标题栏是覆盖式的，
    // 红绿灯浮在网页内容上，所以页内那条顶栏的左边要让出约 86px，
    // 否则侧栏标题会压在红绿灯底下。非 macOS 平台菜单里没有这个值（false）。
    let overlay_titlebar = cfg!(target_os = "macos");
    let mut script = format!(
        "window.__PI_WEB_API_ORIGIN__ = {origin};\
         window.__PI_WEB_DESKTOP__ = true;\
         window.__TOILER_DESKTOP__ = true;\
         window.__TOILER_OVERLAY_TITLEBAR__ = {overlay_titlebar};\
         window.addEventListener('pi-app-ready', function () {{\
           if (window.__TAURI_INTERNALS__) {{\
             window.__TAURI_INTERNALS__.invoke('hide_startup_overlay').catch(function () {{}});\
           }}\
         }}, {{ once: true }});"
    );
    if let Some(guard) = context_menu_guard_script() {
        script.push_str(guard);
    }

    let mut builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
        .title("Toiler")
        .inner_size(1280.0, 820.0)
        .min_inner_size(720.0, 520.0)
        .initialization_script(script);

    // macOS：把系统标题栏换成覆盖式，红绿灯浮在内容上。
    // 不这么做窗口里会有两层标题——系统那层写 "Toiler"，页内顶栏又写一遍，
    // 而“里面套着一个网页”正是最明显的网页感来源。
    #[cfg(target_os = "macos")]
    {
        // 红绿灯位置在 place_traffic_lights 里算（不走 wry 的 inset）。
        builder = builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true);
    }

    builder
        .build()
        .map(|_| ())
        .map_err(|error| format!("创建窗口失败：{error}"))
}

fn shutdown(app: &tauri::AppHandle) {
    // 终端先收：每个 PTY 后面是一个真实 shell（可能还挂着 `npm run dev`），
    // 服务关掉之后它们就不该继续跑。
    terminal::stop_terminals(app);
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
    toiler_server::boot::init_tracing();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .manage(ServerState(Mutex::new(None)))
        .manage(MobileAccess::default())
        .manage(terminal::TerminalState::default())
        .invoke_handler(tauri::generate_handler![
            hide_startup_overlay,
            open_release_url,
            open_external_url,
            terminal::terminal_start,
            terminal::terminal_write,
            terminal::terminal_resize,
            terminal::terminal_close,
        ])
        .setup(|app| {
            // 把自己标记成「桌面壳」，并把 pid 交给服务层。两个变量都在 pi 里由
            // spawn node 时的 env 注入；toiler 同进程，直接写进进程环境即可：
            //
            // * `PI_WEB_DESKTOP_API_ORIGIN` —— `security` 的 origin 校验用它，
            //   `/api/mobile/*` 靠它区分「桌面壳」与「命令行直启」；
            // * `PI_WEB_SHELL_PID` —— `read_lan_port` 用它校验
            //   `desktop-access.runtime.json` 里的 `ownerPid`，不设的话配对地址
            //   永远停在 `starting`（阶段 5 手机回归实测）。
            std::env::set_var("PI_WEB_DESKTOP_API_ORIGIN", DESKTOP_API_ORIGIN);
            std::env::set_var("PI_WEB_SHELL_PID", std::process::id().to_string());

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
            #[cfg(target_os = "macos")]
            schedule_traffic_lights(&handle);
            tracing::info!("toiler 桌面窗口已创建（端口 {port}）");
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
    let _ = WebviewWindowBuilder::new(
        app,
        "startup-error",
        WebviewUrl::App("startup-error.html".into()),
    )
    .title("Toiler 启动失败")
    .inner_size(620.0, 380.0)
    .initialization_script(format!("window.__TOILER_STARTUP_ERROR__ = {escaped};"))
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
pub(crate) fn hidden_command(program: &str) -> Command {
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
    fn context_menu_guard_only_in_release() {
        // 开发版必须保留原生右键：刷新页面与检查元素都靠它。
        assert_eq!(
            context_menu_guard_script().is_some(),
            !cfg!(debug_assertions)
        );
    }

    #[test]
    fn context_menu_guard_keeps_editable_targets() {
        let script = SUPPRESS_CONTEXT_MENU_SCRIPT;
        // 监听必须在捕获阶段挂到 window 上：页面里的 React 处理器注册得比这更晚。
        assert!(script.contains("addEventListener(\"contextmenu\""));
        assert!(script.contains("}, true);"), "监听必须走捕获阶段：{script}");
        // 输入框 / contenteditable 要放行，否则终端与编辑器没法右键粘贴。
        for allowed in ["INPUT", "TEXTAREA", "isContentEditable", ".xterm"] {
            assert!(script.contains(allowed), "缺少例外 {allowed}：{script}");
        }
        // 只预置默认动作，不能 stopPropagation —— 会话列表行的自定义右键靠冒泡。
        assert!(
            !script.contains("stopPropagation"),
            "不能吞掉事件：{script}"
        );
        // 脚本会被原样注入，`//` 注释在压平后是灾难（与导出页同一条铁律）。
        assert!(!script.contains("//"), "注入脚本里不能有 // 注释：{script}");
    }

    #[test]
    fn server_state_starts_empty() {
        let state = ServerState(Mutex::new(None));
        assert!(state.0.lock().unwrap().is_none());
    }

    #[test]
    fn agent_dir_is_absolute() {
        // mobile::start 用的是 toiler-server 的 agent 目录解析，
        // 这里只确认它是个绝对路径（不会写到进程当前目录下）。
        let dir = toiler_server::paths::configs_dir();
        assert!(dir.is_absolute(), "{} 应当是绝对路径", dir.display());
    }
}
