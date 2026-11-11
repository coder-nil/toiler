//! 内嵌服务：axum 直接跑在 Tauri 自己的 tokio runtime 上。
//!
//! 阶段 4 的核心差别就在这个文件：以前 Tauri 壳用 `Command::new(node)` 拉起
//! Next server，再轮询端口等它起来；现在 [`start`] 只是 `bind` → `spawn(serve)`，
//! 端口由内核分配（`port = 0`），没有子进程、没有 45 秒探测、没有 `desktop-dist`。

use std::net::SocketAddr;
use std::time::Instant;

use tokio::sync::oneshot;
use workman_server::{assets, boot, routes};

/// 一个正在跑的内嵌服务。
pub struct DesktopServer {
    pub port: u16,
    pub host: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tauri::async_runtime::JoinHandle<()>>,
}

impl DesktopServer {
    /// 服务的根 URL（窗口加载它，`/api` 与前端同源）。
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    /// 优雅退出：通知 axum 停止接受新连接，关掉正在进行的 SSE。
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        // 不 `await` 任务：退出流程本身就在收尾，axum 收到信号后会自己结束。
        self.task.take();
    }
}

/// 绑定 127.0.0.1 的随机端口并在 Tauri 的 tokio runtime 上开服务。
///
/// `bind` 走 `block_on` 是为了在 `setup` 里立刻拿到端口（窗口 URL 需要它）；
/// 这一步只有一次 syscall，不阻塞启动。真正的服务是 `spawn` 出去的。
pub fn start() -> Result<DesktopServer, String> {
    let mut options =
        boot::BootOptions::from_env().map_err(|error| format!("读取启动参数失败：{error:#}"))?;
    // 从 Finder / 启动台拉起时 cwd 是 `/`；直接拿来当默认项目会让文件浏览器和
    // git 面板都指向根目录（界面上看到的是「Not found」），退回用户目录更合理。
    if options.default_cwd.is_none() {
        options.default_cwd = Some(default_workspace_dir());
    }
    let state =
        boot::prepare_state(&options).map_err(|error| format!("初始化状态失败：{error:#}"))?;
    start_with_state(state, &options)
}

fn default_workspace_dir() -> std::path::PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();
    if cwd.components().count() > 1 {
        return cwd;
    }
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(std::path::PathBuf::from)
        .unwrap_or(cwd)
}

/// 同 [`start`]，但允许调用方传入现成的 state（测试与 `--sessions-root` 覆盖用）。
pub fn start_with_state(
    state: workman_server::state::AppState,
    options: &boot::BootOptions,
) -> Result<DesktopServer, String> {
    const HOST: &str = "127.0.0.1";
    let (listener, addr): (tokio::net::TcpListener, SocketAddr) =
        tauri::async_runtime::block_on(boot::bind(HOST, 0))
            .map_err(|error| format!("绑定回环端口失败：{error:#}"))?;

    // 配对地址要用真实监听端口（而不是命令行默认的 8787）。这一步必须在任何
    // `state.clone()` 之前做 —— `with_binding` 靠 `Arc::get_mut`，一旦被克隆过
    // 就只能拒绝（早先放在预热之后，真实启动直接 panic）。
    let state = state.with_binding(HOST, addr.port());

    log_startup(&state, addr, options);

    // 预热会话列表：窗口一加载前端就会拉 `/api/sessions`，别让用户等第一次全量扫描。
    let warm = state.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let started = Instant::now();
        let items = warm.sessions().list(false);
        tracing::info!(
            "会话列表预热完成：{} 个，{:?}",
            items.len(),
            started.elapsed()
        );
    });

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let app = routes::router(state);
    let task = tauri::async_runtime::spawn(async move {
        if let Err(error) = boot::serve(listener, app, shutdown_rx).await {
            tracing::error!("内嵌服务异常退出：{error:#}");
        }
    });

    Ok(DesktopServer {
        port: addr.port(),
        host: HOST.to_string(),
        shutdown: Some(shutdown_tx),
        task: Some(task),
    })
}

fn log_startup(
    state: &workman_server::state::AppState,
    addr: SocketAddr,
    options: &boot::BootOptions,
) {
    tracing::info!(
        "workman 内嵌服务 http://{addr}（会话根 {}）",
        options.sessions_root.display()
    );
    tracing::info!(
        "前端产物 {}（{}）",
        assets::FRONTEND_DIR,
        if assets::is_placeholder() {
            "占位页"
        } else {
            "已嵌入"
        }
    );
    if let Some(default) = state.catalog().default_entry() {
        tracing::info!("默认模型 {}/{}", default.provider, default.id);
    }
}
