//! 路径与目录约定：agent 目录、配置文件位置、允许访问的根目录。
//!
//! # agent 目录
//!
//! pi 的规则是 `$PI_CODING_AGENT_DIR` > `~/.pi/agent`（`config.js:510`）。
//! workman 用同一套文件名（`auth.json` / `models.json` / `settings.json` /
//! `trust.json` / `pi.sqlite`），所以**默认直接复用 `~/.pi/agent`**——这样
//! 已有的 API Key、模型目录、项目列表立刻可用，两边也不会各存一份。
//!
//! 判定顺序（[`agent_dir`]）：
//!
//! 1. `$WORKMAN_AGENT_DIR`
//! 2. `~/.workman/agent`（若已存在任一配置文件 → 用）
//! 3. `~/.pi/agent`（若已存在任一配置文件 → 用）
//! 4. `~/.workman/agent`（全新安装的默认落点）
//!
//! # 允许的根目录
//!
//! pi 的做法（模块 `80547`）：把所有会话的 `cwd` / `projectRoot`、`~` 下的
//! `pi-cwd-YYYYMMDD` 临时目录、以及当前进程里登记过的目录合成一个集合，
//! 然后对每个路径做「前缀包含」+「realpath 包含」两次判定（模块 `82536`）。

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 六个配置文件：命中任意一个就认为「这个目录是 agent 目录」。
const PROBE_FILES: [&str; 5] = [
    "auth.json",
    "models.json",
    "settings.json",
    "trust.json",
    "pi.sqlite",
];

/// `$HOME`；没有就退化成 `/`。
pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `~` 展开（只认 `~` 与 `~/...`）。
pub fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(raw)
}

fn looks_like_agent_dir(dir: &Path) -> bool {
    PROBE_FILES.iter().any(|name| dir.join(name).exists())
}

/// agent 目录（见模块文档）。
pub fn agent_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("WORKMAN_AGENT_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let workman = home_dir().join(".workman/agent");
    let pi = home_dir().join(".pi/agent");
    if looks_like_agent_dir(&workman) {
        return workman;
    }
    if looks_like_agent_dir(&pi) {
        return pi;
    }
    workman
}

/// agent 目录下的一个文件。
pub fn agent_file(name: &str) -> PathBuf {
    agent_dir().join(name)
}

/// 词法规整：折叠 `.`/`..` 与重复分隔符，不解析符号链接。
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(comp);
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// 绝对化 + 词法规整（不 touch 磁盘）。
pub fn absolute(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    lexical_normalize(&joined)
}

/// 去掉结尾分隔符（`/a/b/` → `/a/b`），根目录保持原样。
pub fn trim_trailing_sep(path: &Path) -> PathBuf {
    let s = path.to_string_lossy().to_string();
    if s.len() <= 1 {
        return path.to_path_buf();
    }
    PathBuf::from(s.trim_end_matches('/').to_string())
}

/// 复刻 `74244.z()`：给 projectKey 用的规范形式（去尾分隔符、按平台大小写）。
pub fn project_key(path: &str) -> String {
    let p = absolute(&expand_tilde(path));
    trim_trailing_sep(&p).to_string_lossy().to_string()
}

/// 复刻 `82536.o()`：`path` 是否落在 `roots` 里某个根之内（纯词法）。
pub fn within_roots(path: &Path, roots: &BTreeSet<PathBuf>) -> bool {
    let target = absolute(path);
    for root in roots {
        let root = absolute(root);
        if target == root || target.starts_with(&root) {
            return true;
        }
    }
    false
}

/// 复刻 `82536.x()`：先 realpath 再判定，符号链接逃逸过不去。
pub fn within_roots_real(path: &Path, roots: &BTreeSet<PathBuf>) -> bool {
    let Ok(real) = std::fs::canonicalize(path) else {
        return false;
    };
    let real_roots: BTreeSet<PathBuf> = roots
        .iter()
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .collect();
    within_roots(&real, &real_roots)
}

/// 允许访问的根目录集合（可并发追加）。
///
/// 对应 pi 的 `globalThis.__piAllowedRootsCache` + `__piAdditionalAllowedRoots`：
/// 会在每次 [`AllowedRoots::snapshot`] 时重新从会话列表里收集，额外登记的根
/// 一直保留。
#[derive(Debug, Clone, Default)]
pub struct AllowedRoots {
    extra: Arc<Mutex<BTreeSet<PathBuf>>>,
    /// 会话里出现的 cwd（由路由在 snapshot 时喂进来）
    seeded: Arc<Mutex<BTreeSet<PathBuf>>>,
}

impl AllowedRoots {
    pub fn new() -> Self {
        Self::default()
    }

    /// 永久登记一个根（对应 `25252.wz()`）。
    pub fn add(&self, path: impl AsRef<Path>) {
        let path = absolute(path.as_ref());
        if let Ok(mut set) = self.extra.lock() {
            set.insert(path);
        }
    }

    /// 用新的会话 cwd 集合替换「seeded」部分（不影响 [`Self::add`] 的登记）。
    pub fn seed(&self, paths: impl IntoIterator<Item = PathBuf>) {
        if let Ok(mut set) = self.seeded.lock() {
            *set = paths.into_iter().map(|p| absolute(&p)).collect();
        }
    }

    /// 当前快照：seeded ∪ extra ∪ `~/pi-cwd-YYYYMMDD`。
    pub fn snapshot(&self) -> BTreeSet<PathBuf> {
        let mut out = BTreeSet::new();
        if let Ok(set) = self.seeded.lock() {
            out.extend(set.iter().cloned());
        }
        if let Ok(set) = self.extra.lock() {
            out.extend(set.iter().cloned());
        }
        if let Ok(entries) = std::fs::read_dir(home_dir()) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if is_pi_cwd_dir_name(&name) {
                    out.insert(absolute(&entry.path()));
                }
            }
        }
        out
    }

    /// 词法包含判定（`80547.A$`）。
    pub fn allows(&self, path: impl AsRef<Path>) -> bool {
        within_roots(path.as_ref(), &self.snapshot())
    }

    /// realpath 包含判定（`80547.B7`）。
    pub fn allows_real(&self, path: impl AsRef<Path>) -> bool {
        within_roots_real(path.as_ref(), &self.snapshot())
    }
}

/// `pi-cwd-20261110` 这类目录名（`80547` 里的 `^pi-cwd-\d{8}$`）。
fn is_pi_cwd_dir_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("pi-cwd-") else {
        return false;
    };
    rest.len() == 8 && rest.chars().all(|c| c.is_ascii_digit())
}

/// 绝对路径判定（`58864.f`：POSIX 以 `/` 开头；也认 Windows 盘符与 UNC）。
pub fn is_absolute_path(raw: &str) -> bool {
    if raw.starts_with('/') {
        return true;
    }
    let bytes = raw.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\'))
        || raw.starts_with("\\\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_lexically() {
        assert_eq!(lexical_normalize(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
        assert_eq!(lexical_normalize(Path::new("/a/./b//c")), PathBuf::from("/a/b/c"));
        assert_eq!(lexical_normalize(Path::new("/")), PathBuf::from("/"));
    }

    #[test]
    fn project_key_trims_trailing_separator() {
        assert_eq!(project_key("/a/b/"), "/a/b");
        assert_eq!(project_key("/"), "/");
    }

    #[test]
    fn containment_is_prefix_based() {
        let roots: BTreeSet<PathBuf> = [PathBuf::from("/a/b")].into_iter().collect();
        assert!(within_roots(Path::new("/a/b"), &roots));
        assert!(within_roots(Path::new("/a/b/c/d"), &roots));
        // /a/bc 不能被 /a/b 命中
        assert!(!within_roots(Path::new("/a/bc/d"), &roots));
    }

    #[test]
    fn lexical_containment_rejects_dotdot_escape() {
        let roots: BTreeSet<PathBuf> = [PathBuf::from("/a/b")].into_iter().collect();
        assert!(!within_roots(Path::new("/a/b/../c"), &roots));
    }

    #[test]
    fn pi_cwd_directory_names() {
        assert!(is_pi_cwd_dir_name("pi-cwd-20261110"));
        assert!(!is_pi_cwd_dir_name("pi-cwd-2026111"));
        assert!(!is_pi_cwd_dir_name("pi-cwd-20261110x"));
        assert!(!is_pi_cwd_dir_name("pi-cwd-abcdefgh"));
    }

    #[test]
    fn absolute_path_detection() {
        assert!(is_absolute_path("/a"));
        assert!(is_absolute_path("C:\\x"));
        assert!(!is_absolute_path("a/b"));
        assert!(!is_absolute_path("~/x"));
    }
}
