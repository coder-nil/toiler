//! 服务引导：把「会话根 → 模型目录 → `AppState` → 监听」这套启动流程从 `main.rs`
//! 抽出来，供两种宿主共用：
//!
//! * `workman-server` CLI（`main.rs`）
//! * `workman-desktop` 的 Tauri 单进程壳（阶段 4，跑在 Tauri 自己的 tokio runtime 上）
//!
//! 这里只做与协议无关的事：路径、默认值、state 组装、端口绑定、优雅退出。
//! 路由（含静态资源）由宿主自己组合，见 [`crate::routes::router`]。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::catalog::ModelCatalog;
use crate::state::AppState;

/// 默认会话根：pi 自己的目录（这样历史会话立刻可见）。
pub fn default_sessions_root() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME 未设置")?;
    Ok(PathBuf::from(home).join(".pi/agent/sessions"))
}

/// 会用到的环境变量（CLI 与桌面壳共享，保证两边行为一致）。
pub fn sessions_root_from_env() -> Result<PathBuf> {
    match std::env::var("WORKMAN_SESSIONS_ROOT") {
        Ok(p) if !p.trim().is_empty() => Ok(PathBuf::from(p)),
        _ => default_sessions_root(),
    }
}

/// 从最近的会话文件里读最后一个 `model_change`（= 上次用的模型）。
pub fn last_used_model(sessions_root: &Path) -> Option<(String, String)> {
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

/// 启动参数。`default_cwd` 为空时退回进程当前目录。
#[derive(Debug, Clone)]
pub struct BootOptions {
    pub sessions_root: PathBuf,
    pub default_cwd: Option<PathBuf>,
}

impl BootOptions {
    /// 从环境变量推导（`WORKMAN_SESSIONS_ROOT`，其余同 CLI 默认）。
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            sessions_root: sessions_root_from_env()?,
            default_cwd: None,
        })
    }
}

/// 建目录 → 载模型目录 → 补「上次用的模型」→ 组装 `AppState`。
///
/// 这是唯一一处创建会话根的地方：桌面壳与 CLI 都走它，避免两边各建一次。
pub fn prepare_state(options: &BootOptions) -> Result<AppState> {
    std::fs::create_dir_all(&options.sessions_root)
        .with_context(|| format!("创建会话根 {} 失败", options.sessions_root.display()))?;

    let mut catalog = ModelCatalog::load()?;
    catalog.apply_default_hint(last_used_model(&options.sessions_root));
    let default_cwd = options
        .default_cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
    Ok(AppState::with_cwd(
        &options.sessions_root,
        catalog,
        default_cwd,
    ))
}

/// 绑定监听地址。`port = 0` 表示让内核分配（桌面壳用，避免抢用户的 8787）。
pub async fn bind(host: &str, port: u16) -> Result<(TcpListener, SocketAddr)> {
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("解析监听地址 {host}:{port} 失败"))?;
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("绑定 {addr} 失败"))?;
    let local = listener.local_addr().context("读取本地监听地址失败")?;
    Ok((listener, local))
}

/// 一直服务到 `shutdown` 收到信号（或调用方 drop 掉 receiver）。
///
/// 不走 `tokio::spawn`：桌面壳自己决定把 future 交给哪个 runtime。
pub async fn serve(
    listener: TcpListener,
    app: axum::Router,
    shutdown: oneshot::Receiver<()>,
) -> Result<()> {
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.await;
        })
        .await
        .context("axum 服务异常退出")
}

/// 打包好的日志（CLI 与桌面壳共用同一套 tracing 过滤规则）。
pub fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "workman_server=info,tower_http=warn".into());
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_sessions_root_matches_pi_layout() {
        // 只测纯函数分支，避免改进程环境导致并发测试互相影响。
        let root = default_sessions_root().unwrap();
        assert_eq!(root.file_name().unwrap(), "sessions");
        assert!(root.ends_with("agent/sessions"), "{} 应以 agent/sessions 结尾", root.display());
    }

    #[test]
    fn prepare_state_creates_sessions_root() {
        let dir = std::env::temp_dir().join(format!("workman-boot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let options = BootOptions {
            sessions_root: dir.clone(),
            default_cwd: Some("/tmp".into()),
        };
        let state = prepare_state(&options).expect("准备 state");
        assert!(dir.is_dir(), "会话根应被创建");
        assert_eq!(state.default_cwd(), Path::new("/tmp"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
