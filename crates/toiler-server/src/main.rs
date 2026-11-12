//! toiler 服务入口（CLI 宿主）。
//!
//! ```bash
//! toiler-server                      # 127.0.0.1:8787，会话根 = ~/.agents/toiler/sessions
//! toiler-server --port 3000 --host 0.0.0.0
//! toiler-server --port 0             # 让内核分配端口（先打印再服务）
//! ```
//!
//! 启动流程全在 [`toiler_server::boot`] 里：桌面壳（`toiler-desktop`）复用同一套
//! 逻辑，只是把 `serve` 交给 Tauri 的 tokio runtime，不再 spawn 子进程。
//!
//! 环境变量（完整列表见 `paths.rs` 与 `docs/contracts/README.md`）：
//! - `TOILER_HOME`：toiler 专属目录（默认 `~/.agents/toiler`）
//! - `AGENTS_HOME`：通用根（默认 `~/.agents`）
//! - `TOILER_SESSIONS_ROOT` / `TOILER_MODELS`：单独覆盖会话根 / 模型目录
//! - `PI_WEB_HOSTNAME` / `PI_WEB_ALLOWED_HOSTS` / `PI_WEB_DESKTOP_API_ORIGIN`：可信 host / origin

use std::path::PathBuf;

use anyhow::{Context, Result};
use toiler_server::{assets, boot, routes};
use tokio::sync::oneshot;

#[tokio::main]
async fn main() -> Result<()> {
    boot::init_tracing();

    let mut host = "127.0.0.1".to_string();
    let mut port: u16 = 8787;
    let mut sessions_root: Option<PathBuf> = None;
    let mut default_cwd: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" | "-p" => {
                port = args
                    .next()
                    .context("--port 需要一个数字")?
                    .parse()
                    .context("--port 不是合法端口")?;
            }
            "--host" | "-H" => {
                host = args.next().context("--host 需要一个地址")?;
            }
            "--sessions-root" => {
                sessions_root = args.next().map(PathBuf::from);
            }
            "--cwd" => {
                default_cwd = args.next().map(PathBuf::from);
            }
            "--help" | "-h" => {
                println!(
                    "用法: toiler-server [--host 127.0.0.1] [--port 8787] \
                     [--sessions-root DIR] [--cwd DIR]"
                );
                return Ok(());
            }
            other => anyhow::bail!("未知参数 {other}（--help 看用法）"),
        }
    }

    let options = boot::BootOptions {
        sessions_root: match sessions_root {
            Some(path) => path,
            None => boot::sessions_root_from_env()?,
        },
        default_cwd,
    };
    let state = boot::prepare_state(&options)?;

    let (listener, addr) = boot::bind(&host, port).await?;
    log_startup(&state, addr, &options.sessions_root);

    // Ctrl-C 也要走优雅退出：正在跑的 agent 任务会收到停止信号。
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = shutdown_tx.send(());
    });

    let state = state.with_binding(&host, addr.port());
    // 内置目录只能在 bind 之后注入：`qoder-cn` 的 baseUrl 要指向真实监听端口。
    // `apisets`（Coding）用的是远端缓存，顺手在后台若过期就拉一份。
    state.attach_builtin_providers();
    state.warm_apisets_catalog();
    boot::serve(listener, routes::router(state), shutdown_rx).await
}

fn log_startup(
    state: &toiler_server::state::AppState,
    addr: std::net::SocketAddr,
    root: &std::path::Path,
) {
    tracing::info!(
        "toiler server 监听 http://{addr}  (会话根 {})",
        root.display()
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
    if let Some(source) = state.catalog().source.as_ref() {
        tracing::info!(
            "模型目录 {}（{} 个模型 / {} 个 provider）",
            source.display(),
            state.catalog().entries.len(),
            state.catalog().providers.len()
        );
    } else {
        tracing::warn!("没有找到 models.json，agent 无法启动（/api/models 会返回空）");
    }
    tracing::info!(
        "web root: {}",
        state
            .catalog()
            .raw
            .get("web")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into())
    );
}
