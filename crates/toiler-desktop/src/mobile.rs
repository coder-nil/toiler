//! 手机访问：局域网反向代理 + 公网入口隧道。
//!
//! 两个模块都是从 pi-desktop 原样移植的（`lan_proxy.rs` / `tunnel.rs`），
//! 阶段 4 的取舍是「保留它们」：它们只认 agent 目录下的两个配置文件，
//! 不关心后端是 Next 还是 axum，所以换掉 server 不影响手机访问。
//!
//! * `desktop-access.json` —— 开关与密码（前端设置页写）
//! * `desktop-access.runtime.json` —— 入口代理绑的随机端口（代理自己写，隧道读）
//! * `desktop-access.tunnel.json` —— 隧道公网地址与状态
//!
//! 未启用时这两个线程什么都不做，也不监听任何端口。

use std::path::{Path, PathBuf};

use tauri::Manager;

use crate::{lan_proxy, tunnel};

/// 代理与隧道的句柄。放在 Tauri 的 managed state 里，退出时按相反顺序回收：
/// 先断公网入口，再关局域网监听 —— 退出应用就等于关掉对外暴露。
#[derive(Default)]
pub struct MobileAccess {
    proxy: std::sync::Mutex<Option<lan_proxy::LanProxyHandle>>,
    tunnel: std::sync::Mutex<Option<tunnel::TunnelHandle>>,
}

impl MobileAccess {
    pub fn stop(&self) {
        if let Ok(mut guard) = self.tunnel.lock() {
            if let Some(mut handle) = guard.take() {
                handle.stop();
            }
        }
        if let Ok(mut guard) = self.proxy.lock() {
            if let Some(mut handle) = guard.take() {
                handle.stop();
            }
        }
    }
}

/// 起两个后台件。`server_port` 是内嵌 axum 监听的端口（代理转发目标）。
pub fn start(app: &tauri::AppHandle, server_port: u16) {
    let directory = toiler_server::paths::configs_dir();
    let _ = std::fs::create_dir_all(&directory);

    let proxy = lan_proxy::start(
        directory.join("desktop-access.json"),
        directory.join("desktop-access.runtime.json"),
        server_port,
    );
    if let Ok(mut guard) = app.state::<MobileAccess>().proxy.lock() {
        *guard = Some(proxy);
    }

    // 隧道指向哪个端口不在这里决定：入口代理绑的是随机端口，隧道管理器自己盯
    // `desktop-access.runtime.json` 里的 `lanPort`，变了就带着隧道一起重起。
    let tunnel_dir = directory.join("tunnel");
    let binary =
        resolve_cloudflared_binary(app).unwrap_or_else(|| tunnel_dir.join("cloudflared-not-found"));
    let handle = tunnel::start(
        directory.join("desktop-access.json"),
        directory.join("desktop-access.tunnel.json"),
        directory.join("desktop-access.runtime.json"),
        binary,
        tunnel_dir,
        home_dir().join(".cloudflared"),
    );
    if let Ok(mut guard) = app.state::<MobileAccess>().tunnel.lock() {
        *guard = Some(handle);
    }

    tracing::info!("手机访问组件已就绪（配置目录 {}）", directory.display());
}

fn home_dir() -> PathBuf {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.map(PathBuf::from).unwrap_or_default()
}

/// 解析 cloudflared 可执行文件的位置。
///
/// 顺序：显式环境变量（开发/排障）→ 随 app 打包的资源目录 → PATH → Homebrew 常见位置。
/// 找不到时返回 None，让隧道管理器把状态报成 `missing-binary`，界面就能说明原因。
fn resolve_cloudflared_binary(app: &tauri::AppHandle) -> Option<PathBuf> {
    if let Some(configured) = std::env::var_os("TOILER_CLOUDFLARED_PATH") {
        let candidate = PathBuf::from(configured);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    if let Ok(resources) = app.path().resource_dir() {
        for candidate in [
            resources.join("binaries").join("cloudflared"),
            resources.join("cloudflared"),
        ] {
            if candidate.is_file() {
                make_executable(&candidate);
                return Some(candidate);
            }
        }
    }

    if let Some(found) = cloudflared_on_path() {
        return Some(found);
    }

    [
        "/opt/homebrew/bin/cloudflared",
        "/usr/local/bin/cloudflared",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|candidate| candidate.is_file())
}

fn cloudflared_on_path() -> Option<PathBuf> {
    let executable = if cfg!(windows) {
        "cloudflared.exe"
    } else {
        "cloudflared"
    };
    std::env::var_os("PATH")
        .map(|value| {
            std::env::split_paths(&value)
                .map(|directory| directory.join(executable))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
        .into_iter()
        .find(|candidate| candidate.is_file())
}

/// 打包进 app 的二进制可能丢掉执行位，启动前补回来。
fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path) {
            let mut permissions = metadata.permissions();
            if permissions.mode() & 0o111 == 0 {
                permissions.set_mode(0o755);
                let _ = std::fs::set_permissions(path, permissions);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}
