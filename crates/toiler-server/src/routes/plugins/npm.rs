//! 插件操作才查找本机 Node.js；不运行登录 shell 或 nvm 脚本，也不修改 Toiler 的 PATH。

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

#[cfg(unix)]
const NODE: &str = "node";
#[cfg(windows)]
const NODE: &str = "node.exe";
#[cfg(unix)]
const NPM_NAMES: &[&str] = &["npm"];
#[cfg(windows)]
const NPM_NAMES: &[&str] = &["npm.cmd", "npm.exe"];

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) struct NpmRuntime {
    pub(super) npm: PathBuf,
    node: PathBuf,
    cli: Option<PathBuf>,
    path: OsString,
}

impl NpmRuntime {
    pub(super) async fn resolve() -> Result<Self, String> {
        let search = Search::from_env();
        tokio::time::timeout(SEARCH_TIMEOUT, search.resolve())
            .await
            .map_err(|_| {
                "查找本机 Node.js / npm 超时；请检查安装或用 TOILER_NPM 指定 npm".to_string()
            })?
    }

    pub(super) fn command(&self) -> tokio::process::Command {
        let mut command = match &self.cli {
            // Windows 的 npm.cmd 只是启动 JS 的包装层。直接调用 node.exe，避免
            // cmd.exe 重新解释带空格的路径或 npm 参数，并沿用隐藏控制台策略。
            Some(cli) => {
                let mut command = crate::process::tokio_command(&self.node);
                command.arg(cli);
                command
            }
            None => crate::process::tokio_command(&self.npm),
        };
        command
            .env("PATH", &self.path)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }
}

struct Search {
    override_npm: Option<PathBuf>,
    path: Vec<PathBuf>,
    active_nvm: Option<PathBuf>,
    nvm_root: Option<PathBuf>,
    system: Vec<PathBuf>,
}

impl Search {
    fn from_env() -> Self {
        let env_path = |key| {
            std::env::var_os(key)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        let path = std::env::var_os("PATH")
            .map(|value| std::env::split_paths(&value).collect())
            .unwrap_or_default();
        let override_npm = std::env::var_os("TOILER_NPM").map(PathBuf::from);

        #[cfg(unix)]
        let (active_nvm, nvm_root, system) = {
            let nvm_root = env_path("NVM_DIR").or_else(|| {
                env_path("XDG_CONFIG_HOME")
                    .map(|dir| dir.join("nvm"))
                    .or_else(|| env_path("HOME").map(|dir| dir.join(".nvm")))
            });
            let mut system = Vec::new();
            #[cfg(target_os = "macos")]
            system.push(PathBuf::from("/opt/homebrew/bin"));
            system.extend(["/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
            (env_path("NVM_BIN"), nvm_root, system)
        };

        #[cfg(windows)]
        let (active_nvm, nvm_root, system) = {
            let system = ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
                .into_iter()
                .filter_map(env_path)
                .map(|dir| dir.join("nodejs"))
                .chain(env_path("APPDATA").map(|dir| dir.join("npm")))
                .collect();
            (env_path("NVM_SYMLINK"), env_path("NVM_HOME"), system)
        };

        Self {
            override_npm,
            path,
            active_nvm,
            nvm_root,
            system,
        }
    }

    fn bins(&self) -> Vec<&Path> {
        self.path
            .iter()
            .chain(&self.active_nvm)
            .map(PathBuf::as_path)
            .filter(|bin| bin.is_absolute())
            .collect()
    }

    async fn resolve(&self) -> Result<NpmRuntime, String> {
        let (prefer_system, versions) = self.nvm_root.as_deref().map(nvm_bins).unwrap_or_default();
        let (first, last) = if prefer_system {
            (&self.system, &versions)
        } else {
            (&versions, &self.system)
        };
        let mut bins = self.bins();
        bins.extend(first.iter().chain(last).map(PathBuf::as_path));
        if let Some(npm) = &self.override_npm {
            if npm.as_os_str().is_empty() || !npm.is_file() {
                return Err(format!("TOILER_NPM 指向的文件不存在：{}", npm.display()));
            }
            let npm = if npm.is_absolute() {
                npm.clone()
            } else {
                std::env::current_dir()
                    .map_err(|error| format!("无法解析 TOILER_NPM：{error}"))?
                    .join(npm)
            };
            return self.try_npm(&npm, &bins).await.ok_or_else(|| {
                format!("TOILER_NPM 无法与本机 Node.js 一起运行：{}", npm.display())
            });
        }

        let mut seen = HashSet::new();
        for bin in &bins {
            if !seen.insert(*bin) {
                continue;
            }
            for name in NPM_NAMES {
                let npm = bin.join(name);
                if npm.is_file() {
                    if let Some(runtime) = self.try_npm(&npm, &bins).await {
                        return Ok(runtime);
                    }
                }
            }
        }
        Err(
            "未找到可用的 Node.js 与 npm。请安装 Node.js（自带 npm），或检查 PATH / TOILER_NPM"
                .into(),
        )
    }

    async fn try_npm(&self, npm: &Path, bins: &[&Path]) -> Option<NpmRuntime> {
        let npm_bin = npm.parent()?;
        let cli = npm_cli(npm)?;
        let adjacent = npm_bin.join(NODE);
        // Linux 发行版与用户级 npm 可能将二者分开安装；有同目录的 node 时不混用别的版本。
        let nodes: Vec<_> = if adjacent.is_file() {
            vec![adjacent]
        } else {
            bins.iter()
                .map(|bin| bin.join(NODE))
                .filter(|node| node.is_file())
                .collect()
        };
        let mut seen = HashSet::new();
        for node in nodes.into_iter().filter(|node| seen.insert(node.clone())) {
            let node_bin = node.parent()?;
            let dirs = std::iter::once(node_bin)
                .chain(std::iter::once(npm_bin).filter(|bin| *bin != node_bin))
                .chain(
                    self.path
                        .iter()
                        .map(PathBuf::as_path)
                        .filter(|bin| *bin != node_bin && *bin != npm_bin),
                );
            let Ok(path) = std::env::join_paths(dirs) else {
                continue;
            };
            let runtime = NpmRuntime {
                npm: npm.to_path_buf(),
                node,
                cli: cli.clone(),
                path,
            };
            let mut node = crate::process::tokio_command(&runtime.node);
            node.env("PATH", &runtime.path);
            if let Some(node_version) = probe(node).await {
                if let Some(npm_version) = probe(runtime.command()).await {
                    tracing::info!(
                        npm = %runtime.npm.display(), node = %runtime.node.display(),
                        %node_version, %npm_version, source = self.source(&runtime.node),
                        "已选择插件用的 Node.js 和 npm"
                    );
                    return Some(runtime);
                }
            }
        }
        tracing::debug!(npm = %npm.display(), "跳过无法配对或运行的 npm");
        None
    }

    fn source(&self, node: &Path) -> &'static str {
        let managed = self
            .nvm_root
            .as_ref()
            .and_then(|root| root.canonicalize().ok())
            .zip(node.canonicalize().ok())
            .is_some_and(|(root, node)| node.starts_with(root));
        match (managed, cfg!(windows)) {
            (true, true) => "nvm-windows",
            (true, false) => "nvm",
            _ => "系统路径或其他安装方式",
        }
    }
}

#[cfg(unix)]
fn npm_cli(_: &Path) -> Option<Option<PathBuf>> {
    Some(None)
}

#[cfg(windows)]
fn npm_cli(npm: &Path) -> Option<Option<PathBuf>> {
    let name = npm.file_name()?;
    if name.eq_ignore_ascii_case("npm-cli.js") {
        Some(Some(npm.to_path_buf()))
    } else if npm.extension()?.eq_ignore_ascii_case("cmd") {
        let cli = npm.parent()?.join("node_modules/npm/bin/npm-cli.js");
        cli.is_file().then_some(Some(cli))
    } else {
        npm.extension()?.eq_ignore_ascii_case("exe").then_some(None)
    }
}

async fn probe(mut command: tokio::process::Command) -> Option<String> {
    let output = tokio::time::timeout(
        PROBE_TIMEOUT,
        command
            .arg("--version")
            .env("npm_config_update_notifier", "false")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let version = std::str::from_utf8(&output.stdout).ok()?.trim();
    (output.status.success()
        && version_numbers(version.split_once('-').map_or(version, |(v, _)| v))?.len() == 3)
        .then(|| version.to_string())
}

fn version_numbers(raw: &str) -> Option<Vec<u32>> {
    raw.strip_prefix('v')
        .unwrap_or(raw)
        .split('.')
        .map(|part| {
            (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| part.parse().ok())
                .flatten()
        })
        .collect()
}

fn nvm_bins(root: &Path) -> (bool, Vec<PathBuf>) {
    #[cfg(unix)]
    let (versions_root, default) = (root.join("versions/node"), nvm_default(root));
    #[cfg(windows)]
    let (versions_root, default) = (root.to_path_buf(), None::<String>);
    let mut versions: Vec<_> = std::fs::read_dir(versions_root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?.strip_prefix('v')?;
            let version = version_numbers(name)?;
            let bin = if cfg!(windows) {
                entry.path()
            } else {
                entry.path().join("bin")
            };
            (version.len() == 3 && bin.is_dir()).then_some((version, bin))
        })
        .collect();
    let prefix = default
        .as_deref()
        .and_then(|value| version_numbers(value.trim_end_matches(".*").trim_end_matches(".x")));
    // 指定了默认版本时优先使用匹配项；剩余候选仍按数字排序，v22.10 > v22.9。
    let preferred = |version: &[u32]| prefix.as_ref().is_some_and(|p| version.starts_with(p));
    versions.sort_unstable_by(|(a, _), (b, _)| (preferred(b), b).cmp(&(preferred(a), a)));
    (
        default.as_deref() == Some("system"),
        versions.into_iter().map(|(_, bin)| bin).collect(),
    )
}

#[cfg(unix)]
fn nvm_default(root: &Path) -> Option<String> {
    let mut alias = "default".to_string();
    // 支持普通别名链与 nvm 缓存的 LTS 别名，但不执行 shell，也不跟随无限循环。
    for _ in 0..8 {
        let key = alias.strip_prefix("lts/").unwrap_or(&alias);
        if key.is_empty()
            || !key
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_*.".contains(&c))
            || key == "."
            || key == ".."
        {
            return None;
        }
        if alias == "system"
            || alias == "node"
            || alias == "stable"
            || version_numbers(alias.trim_end_matches(".*").trim_end_matches(".x")).is_some()
        {
            return Some(alias);
        }
        alias = std::fs::read_to_string(root.join("alias").join(&alias))
            .ok()?
            .trim()
            .to_string();
    }
    None
}

#[cfg(test)]
mod tests;
