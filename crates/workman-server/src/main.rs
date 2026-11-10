//! workman 服务入口。
//!
//! ```bash
//! workman-server                      # 127.0.0.1:8787，会话根 = ~/.pi/agent/sessions
//! workman-server --port 3000 --host 0.0.0.0
//! ```
//!
//! 环境变量：
//! - `WORKMAN_SESSIONS_ROOT`：会话根（默认 `~/.pi/agent/sessions`，与 pi 共用）
//! - `WORKMAN_MODELS`：`models.json` 路径（默认 `~/.workman/agent/models.json` → `~/.pi/agent/models.json`）
//! - `PI_WEB_HOSTNAME` / `PI_WEB_ALLOWED_HOSTS` / `PI_WEB_DESKTOP_API_ORIGIN`：可信 host / origin

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use workman_server::{catalog::ModelCatalog, routes, state::AppState};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "workman_server=info,tower_http=warn".into()),
        )
        .init();

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
                    "用法: workman-server [--host 127.0.0.1] [--port 8787] \
                     [--sessions-root DIR] [--cwd DIR]"
                );
                return Ok(());
            }
            other => anyhow::bail!("未知参数 {other}（--help 看用法）"),
        }
    }

    let sessions_root = match sessions_root {
        Some(p) => p,
        None => match std::env::var("WORKMAN_SESSIONS_ROOT") {
            Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
            _ => default_sessions_root()?,
        },
    };
    std::fs::create_dir_all(&sessions_root)
        .with_context(|| format!("创建会话根 {} 失败", sessions_root.display()))?;

    let mut catalog = ModelCatalog::load()?;
    // 「上次用的模型」作为默认：最近会话里最后一个 model_change
    catalog.apply_default_hint(last_used_model(&sessions_root));
    let state = AppState::with_cwd(
        &sessions_root,
        catalog,
        default_cwd
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))),
    );

    let app = routes::router(state.clone());
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("解析监听地址 {host}:{port} 失败"))?;

    tracing::info!(
        "workman server 监听 http://{addr}  (会话根 {})",
        sessions_root.display()
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

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// 从最近的会话文件里读最后一个 `model_change`（= 上次用的模型）。
fn last_used_model(sessions_root: &std::path::Path) -> Option<(String, String)> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for dir in std::fs::read_dir(sessions_root).ok()?.flatten() {
        let path = dir.path();
        if !path.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(&path).ok()?.flatten() {
            let p = file.path();
            if p.extension().map_or(true, |e| e != "jsonl") {
                continue;
            }
            let Ok(meta) = file.metadata() else { continue };
            let Ok(modified) = meta.modified() else { continue };
            if newest.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                newest = Some((modified, p));
            }
        }
    }
    let (_, path) = newest?;
    let text = std::fs::read_to_string(path).ok()?;
    let mut found: Option<(String, String)> = None;
    for line in text.lines().rev().take(400) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match value.get("type").and_then(|v| v.as_str()) {
            Some("model_change") => {
                let provider = value.get("provider").and_then(|v| v.as_str())?;
                let model_id = value.get("modelId").and_then(|v| v.as_str())?;
                found = Some((provider.to_string(), model_id.to_string()));
                break;
            }
            Some("message") => {
                // 更实时的线索：assistant 消息自带 provider/model
                let message = value.get("message");
                if message.and_then(|m| m.get("role")).and_then(|r| r.as_str()) == Some("assistant") {
                    let provider = message.and_then(|m| m.get("provider")).and_then(|v| v.as_str());
                    let model_id = message.and_then(|m| m.get("model")).and_then(|v| v.as_str());
                    if let (Some(provider), Some(model_id)) = (provider, model_id) {
                        found = Some((provider.to_string(), model_id.to_string()));
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    found
}

/// 默认会话根：pi 自己的目录（这样 327 个历史会话立刻可见）。
fn default_sessions_root() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME 未设置")?;
    Ok(PathBuf::from(home).join(".pi/agent/sessions"))
}
