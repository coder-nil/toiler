//! 内嵌服务：axum 直接跑在 Tauri 自己的 tokio runtime 上。
//!
//! 阶段 4 的核心差别就在这个文件：以前 Tauri 壳用 `Command::new(node)` 拉起
//! Next server，再轮询端口等它起来；现在 [`start`] 只是 `bind` → `spawn(serve)`，
//! 端口由内核分配（`port = 0`），没有子进程、没有 45 秒探测、没有 `desktop-dist`。

use std::net::SocketAddr;
use std::time::Instant;

use toiler_server::{assets, boot, paths, projects::ProjectStore, routes};
use tokio::sync::oneshot;

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
    // 双击启动（Finder / 启动台 / Dock）时 LaunchServices 给的 cwd 是 `/`。
    // 以前这里退回 `$HOME`，后果不只是「文件面板显示」：默认 cwd 会被
    // `sync_allowed_roots` 当成允许根，于是文件面板列出主目录、agent 的 cwd
    // 也是主目录，`find` / `grep` 的缺省路径 `.` 一递归就撞进 `Documents` /
    // `Desktop` / `Pictures` / `Music`，macOS 的 TCC 逐个弹隐私授权。
    if options.default_cwd.is_none() {
        options.default_cwd = Some(default_workspace_dir());
    }
    let mut state =
        boot::prepare_state(&options).map_err(|error| format!("初始化状态失败：{error:#}"))?;
    // 上一段只在 cwd 不可用时生效，但 `prepare_state` 还会把 cwd 当缺省值另算
    // 一遍；这里统一以 `options.default_cwd` 为准，保证 `create_dir_all` 建出
    // 的就是真正要用的那个目录。
    if let Some(dir) = options.default_cwd.clone() {
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("创建默认项目目录 {} 失败：{error}", dir.display()))?;
        state = state.with_default_cwd(dir);
    }
    start_with_state(state, &options)
}

/// 进程 cwd 可用就用它；否则按「上次选中的项目 → 专用兜底目录」的顺序挑。
///
/// 判断「可用」的标准是 `components().count() > 1`：`/` 只有一个根分量，
/// 是 LaunchServices 双击启动的标记，不是用户选的工作区。
fn default_workspace_dir() -> std::path::PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();
    if cwd.components().count() > 1 {
        return cwd;
    }
    if let Some(root) = remembered_project_root() {
        return root;
    }
    paths::workspace_dir()
}

/// `work.sqlite` 里上次选中的项目根（只读，不建库、不迁移）。
///
/// 必须只读：`ProjectStore::init_default()` 一建库，`migrate_from_pi_if_needed`
/// 的「当前库还不存在」这个前提就永久失效，用户从 pi 搬过来的项目列表就再也
/// 导不进来了。这里只在一个只读连接里读一行，库不存在时直接放弃。
fn remembered_project_root() -> Option<std::path::PathBuf> {
    let (_, root) = ProjectStore::default().selected_read_only()?;
    let path = std::path::PathBuf::from(root);
    // 记着的项目可能已被删掉 / 移走 / 在断开的移动硬盘上：只有真在才用。
    if !path.is_dir() {
        return None;
    }
    // 老版本曾经把 `$HOME` 当成默认项目写过库。若真把主目录记住在那里，
    // 也会把受 TCC 保护的子目录拉进允许根，所以绕开它走专用目录。
    if path == paths::home_dir() {
        return None;
    }
    Some(path)
}

/// 同 [`start`]，但允许调用方传入现成的 state（测试与 `--sessions-root` 覆盖用）。
pub fn start_with_state(
    state: toiler_server::state::AppState,
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
    // `qoder-cn` 的模型 baseUrl 指向本进程端口，所以注入必须发生在 bind 之后；
    // `apisets`（Coding）的目录来自远端缓存，缓存过期时顺手在后台拉一份。
    state.attach_builtin_providers();
    state.warm_apisets_catalog();

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
    state: &toiler_server::state::AppState,
    addr: SocketAddr,
    options: &boot::BootOptions,
) {
    tracing::info!(
        "toiler 内嵌服务 http://{addr}（会话根 {}）",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 双击启动（cwd 是 `/`）时**绝不能**退回 `$HOME`。
    ///
    /// 退回主目录会让默认 cwd 进入允许根，于是文件面板列出主目录、agent 的
    /// cwd 也是主目录，`find` / `grep` 一递归就撞进 `Documents` / `Desktop` /
    /// `Pictures` / `Music`，macOS 的 TCC 逐个弹隐私授权。`workspace_dir()` 是
    /// 自己的空目录，永不命中那些受保护路径。
    #[test]
    fn fallback_is_never_the_home_directory() {
        let fallback = paths::workspace_dir();
        assert_eq!(fallback.file_name().unwrap(), "workspace");
        assert_eq!(fallback.parent().unwrap(), paths::toiler_home());
        assert_ne!(fallback, paths::home_dir());
        for protected in ["Documents", "Desktop", "Pictures", "Music", "Downloads"] {
            let dir = paths::home_dir().join(protected);
            assert!(!fallback.starts_with(&dir), "兜底目录不能落在 {dir:?} 里");
        }
    }

    /// `work.sqlite` 不存在时只读查询返回 `None`，且**不能**顺手把空库建出来。
    ///
    /// 建出来就会让 `migrate_from_pi_if_needed` 的「当前库还不存在」前提失效，
    /// 从 `pi.sqlite` 搬项目的那条路径就永远不跑了。
    #[test]
    fn read_only_lookup_does_not_create_the_store() {
        let dir = std::env::temp_dir().join(format!("toiler-readonly-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = dir.join("work.sqlite");
        let store = ProjectStore::at(&db);
        assert!(store.selected_read_only().is_none());
        assert!(!db.exists(), "只读查询不能建库");
        assert!(!dir.exists(), "只读查询不能建目录");

        // 建库并写入后能读回，且仍是只读（不因读而改内容）。
        store.set_selected("/tmp/a", "/tmp/a").unwrap();
        assert_eq!(
            store.selected_read_only(),
            Some(("/tmp/a".to_string(), "/tmp/a".to_string()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
