//! 路径与目录约定：配置目录、应用专属目录、项目库、允许访问的根目录。
//!
//! # 目录布局
//!
//! 分两层：`.agents/` 放**跨 agent 工具通用**的东西，`.agents/toiler/` 放
//! **toiler 特有**的东西，两者不混。
//!
//! ```text
//! ~/.agents/                       ← 通用根（多个 agent 工具共用）
//! ├── skills/                      各工具都能识别
//! ├── mcp/                         同上
//! ├── commands/                    同上
//! ├── tools/                       同上
//! └── toiler/                    ← toiler 应用专属
//!     ├── configs/                 toiler 自己的配置
//!     │   ├── auth.json            API Key（0600）
//!     │   ├── models.json          provider / 模型定义
//!     │   ├── settings.json        默认模型、enabledModels…
//     │   ├── trust.json           项目信任
//!     │   └── desktop-access*.json 手机访问（3 份）
//!     ├── sessions/                v3 会话
//!     ├── plugins/                 插件安装根
//!     │   ├── npm/node_modules/    `npm:` 来源的包（与 pi 的 `<agentDir>/npm` 同构）
//!     │   └── git/<host>/<path>    `git:` 来源的包
//!     └── work.sqlite              项目列表 + 选中项
//! ```
//!
//! 为什么不继续用 `~/.pi/agent`：那是 **pi 的** agent 目录，pi 会按自己的节奏
//! 迁移 schema、清理缓存、重写文件。toiler 把配置放进去就会跟着一起被改，
//! 而且两边谁改了哪个键都说不清。分目录后各管各的。
//!
//! # 首次启动的种子数据
//!
//! `~/.agents/toiler/configs/` 为空时，从 `~/.pi/agent` **复制**一份
//! （`auth.json` / `models.json` / `settings.json` / `trust.json` /
//! `desktop-access*.json`），会话目录也整份复制过去。**旧文件保留不删** ——
//! 用户可能还在用 pi，删掉等于替人做主。见 [`seed_from_pi_if_needed`]。
//!
//! # 环境变量
//!
//! | 变量 | 作用 |
//! |---|---|
//! | `AGENTS_HOME` | 改通用根（默认 `~/.agents`） |
//! | `TOILER_HOME` | 改 toiler 专属目录（默认 `<agentsHome>/toiler`） |
//! | `TOILER_AGENT_DIR` | 改「配置 + 会话」的根（测试用，等价于旧语义） |
//! | `TOILER_SESSIONS_ROOT` | 只改会话根 |
//! | `TOILER_MODELS` | 只改模型目录 |
//!
//! # 允许的根目录
//!
//! pi 的做法（模块 `80547`）：把所有会话的 `cwd` / `projectRoot`、`~` 下的
//! `pi-cwd-YYYYMMDD` 临时目录、以及当前进程里登记过的目录合成一个集合，
//! 然后对每个路径做「前缀包含」+「realpath 包含」两次判定（模块 `82536`）。

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

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

/// 通用根：`$AGENTS_HOME` > `~/.agents`。
///
/// 放跨 agent 工具通用的东西（`skills/`、`mcp/`、`commands/`、`tools/`）。
pub fn agents_home() -> PathBuf {
    if let Some(dir) = std::env::var_os("AGENTS_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    home_dir().join(".agents")
}

/// toiler 专属根：`$TOILER_HOME` > `$TOILER_AGENT_DIR` > `<agentsHome>/toiler`。
///
/// 用一个带 `agent` 名字的变量兼容旧语义：老调用方与测试把
/// `TOILER_AGENT_DIR` 当成「配置 + 会话所在的目录」，现在它指的就是这个根。
pub fn toiler_home() -> PathBuf {
    for key in ["TOILER_HOME", "TOILER_AGENT_DIR"] {
        if let Some(dir) = std::env::var_os(key) {
            if !dir.is_empty() {
                return PathBuf::from(dir);
            }
        }
    }
    agents_home().join("toiler")
}

/// 配置目录：`<toilerHome>/configs`。
pub fn configs_dir() -> PathBuf {
    toiler_home().join("configs")
}

/// 配置目录下的一个文件。
pub fn config_file(name: &str) -> PathBuf {
    configs_dir().join(name)
}

/// 会话根：`$TOILER_SESSIONS_ROOT` > `<toilerHome>/sessions`。
pub fn sessions_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("TOILER_SESSIONS_ROOT") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    toiler_home().join("sessions")
}

/// 插件安装根：`<toilerHome>/plugins`。
///
/// `settings.json` 的 `packages` 里的全局包都装在这里（内部按来源分子目录，见
/// `routes/plugins.rs` 的模块文档）。**不**用 `<toilerHome>/npm`：那是 pi 的布局
/// （`<agentDir>/npm`），toiler 的根目录留给配置 / 会话 / 项目库，插件单开一层。
pub fn plugins_dir() -> PathBuf {
    toiler_home().join("plugins")
}

/// 项目库：`<toilerHome>/work.sqlite`。
pub fn project_db() -> PathBuf {
    toiler_home().join("work.sqlite")
}

/// 旧位置：pi 的 agent 目录（`$PI_CODING_AGENT_DIR` > `~/.pi/agent`）。
///
/// **只读**：播种数据时从这里复制，之后 toiler 不再写它。
pub fn legacy_pi_agent_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("PI_CODING_AGENT_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    home_dir().join(".pi/agent")
}

/// 旧的项目库位置（迁移用）：`<piAgentDir>/pi.sqlite`。
pub fn legacy_project_db() -> PathBuf {
    legacy_pi_agent_dir().join("pi.sqlite")
}

// ============================================================================
// 首次启动的种子数据
// ============================================================================

/// 需要从 pi 的 agent 目录搬过来的配置文件。
///
/// 只搬 toiler 真正用到的；`mcp.json` / `mcp-cache.json` / `models-store.json` /
/// `web-push.json` 等属于 pi 自己的缓存与状态，不搬（搬了反而变成陈旧副本）。
///
/// `models-store.json` 另有一层原因：toiler 自己会写这份文件（`apisets` 的远端
/// 模型目录，见 [`crate::apisets`]），从 pi 搬一份两个进程会互相覆盖。
pub const SEEDED_CONFIG_FILES: [&str; 4] =
    ["auth.json", "models.json", "settings.json", "trust.json"];

/// 手机访问的三份状态文件也要跟着走，否则配对地址与开关全丢。
const SEEDED_DESKTOP_ACCESS: [&str; 3] = [
    "desktop-access.json",
    "desktop-access.runtime.json",
    "desktop-access.tunnel.json",
];

/// 播种结果，供启动日志与测试使用。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SeedReport {
    /// 复制了哪些配置文件。
    pub configs: Vec<String>,
    /// 复制了多少个会话文件。
    pub sessions: usize,
    /// 是否真的做了任何事（已播种过则为 false）。
    pub acted: bool,
}

/// 首次使用时，从 pi 的 agent 目录**复制**一份配置与会话。
///
/// 判定条件：
///
/// * 目标配置目录里**还没有任何** [`SEEDED_CONFIG_FILES`]—— 一旦播过就不再动，
///   否则用户后来删掉的凭据会在下次启动时“复活”；
/// * 源目录里确实有东西可搬。
///
/// **只读源目录**：不移动、不删除。用户可能还在用 pi，替人做主删掉是不可接受的。
/// 复制后用 `0600` 落盘（里面可能有 API Key）。
///
/// 会话逐个文件复制，已存在的不覆盖。返回复制了多少个。
pub fn seed_from_pi_if_needed() -> std::io::Result<SeedReport> {
    seed_into(&legacy_pi_agent_dir(), &toiler_home())
}

/// [`seed_from_pi_if_needed`] 的可注入版本（测试用）。
///
/// `source` 是 pi 的 agent 目录（只读），`target` 是 toiler 专属根。
fn seed_into(source: &Path, target_root: &Path) -> std::io::Result<SeedReport> {
    let mut report = SeedReport::default();
    if !source.is_dir() {
        return Ok(report);
    }

    let target = target_root.join("configs");
    let already_seeded = SEEDED_CONFIG_FILES
        .iter()
        .any(|name| target.join(name).exists());

    if !already_seeded {
        std::fs::create_dir_all(&target)?;
        let mut names: Vec<&str> = SEEDED_CONFIG_FILES.to_vec();
        names.extend(SEEDED_DESKTOP_ACCESS);
        for name in names {
            let from = source.join(name);
            let to = target.join(name);
            if !from.is_file() || to.exists() {
                continue;
            }
            std::fs::copy(&from, &to)?;
            // 配置里可能有 API Key，统一收紧到 0600。
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o600));
            }
            report.configs.push(name.to_string());
            report.acted = true;
        }
    }

    // 会话单独判：即使配置已经播过，会话也可能还没搬（用户后来才生成新的）。
    report.sessions = seed_sessions(&source.join("sessions"), &target_root.join("sessions"))?;
    if report.sessions > 0 {
        report.acted = true;
    }

    Ok(report)
}

/// 把 `from` 下的会话目录整份复制到 `to`，已存在的不覆盖。
fn seed_sessions(from: &Path, to: &Path) -> std::io::Result<usize> {
    if !from.is_dir() {
        return Ok(0);
    }
    let mut copied = 0usize;
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)?.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name() else {
            continue;
        };
        if !path.is_dir() {
            continue;
        }
        let dir_target = to.join(name);
        std::fs::create_dir_all(&dir_target)?;
        for file in std::fs::read_dir(&path)?.flatten() {
            let file_path = file.path();
            if file_path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(file_name) = file_path.file_name() else {
                continue;
            };
            let dest = dir_target.join(file_name);
            if dest.exists() {
                continue;
            }
            // 只复制能完整读出的文件：半截的复制过去会让会话列表多出坏条目。
            if std::fs::copy(&file_path, &dest).is_ok() {
                copied += 1;
            }
        }
    }
    Ok(copied)
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
        assert_eq!(
            lexical_normalize(Path::new("/a/b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(
            lexical_normalize(Path::new("/a/./b//c")),
            PathBuf::from("/a/b/c")
        );
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

    /// 目录布局：通用根 / toiler 专属根 / 各自的子目录。
    ///
    /// 这些断言不改进程环境（并行测试共享环境变量，改了会互相干扰），
    /// 只验证「在默认环境下的相对关系」。
    #[test]
    fn layout_separates_shared_and_app_specific_state() {
        // 若外层设置了这些变量，跳过相对断言（CI 里可能有）
        if std::env::var_os("AGENTS_HOME").is_some()
            || std::env::var_os("TOILER_HOME").is_some()
            || std::env::var_os("TOILER_AGENT_DIR").is_some()
        {
            return;
        }
        let agents = agents_home();
        let toiler = toiler_home();
        assert_eq!(agents, home_dir().join(".agents"));
        assert_eq!(toiler, agents.join("toiler"), "toiler 专属目录在通用根下");

        // toiler 特有的东西都在自己的目录里
        for (name, path) in [
            ("configs", configs_dir()),
            ("sessions", sessions_dir()),
            ("plugins", plugins_dir()),
            ("work.sqlite", project_db()),
        ] {
            assert!(
                path.starts_with(&toiler),
                "{name} 应在 {} 下，实际 {}",
                toiler.display(),
                path.display()
            );
        }
        assert_eq!(config_file("auth.json"), configs_dir().join("auth.json"));
        assert_eq!(project_db().file_name().unwrap(), "work.sqlite");

        // 通用根下不该出现 toiler 的私有文件
        assert!(!agents.join("work.sqlite").exists());
    }

    /// 首次启动把 pi 的配置与会话复制过来：旧文件必须留着，新副本 0600。
    #[test]
    fn seeding_copies_configs_and_sessions_without_touching_the_source() {
        let dir = std::env::temp_dir().join(format!("toiler-seed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let source = dir.join("pi/agent");
        let target = dir.join("agents/toiler");
        std::fs::create_dir_all(source.join("sessions/--a--")).unwrap();
        std::fs::create_dir_all(&target).unwrap();

        std::fs::write(source.join("auth.json"), r#"{"k":"v"}"#).unwrap();
        std::fs::write(source.join("models.json"), r#"{"providers":{}}"#).unwrap();
        std::fs::write(source.join("settings.json"), "{}").unwrap();
        std::fs::write(source.join("trust.json"), "{}").unwrap();
        std::fs::write(source.join("desktop-access.json"), "{}").unwrap();
        // pi 自己的缓存，不该被搬
        std::fs::write(source.join("mcp-cache.json"), "x").unwrap();
        std::fs::write(source.join("sessions/--a--/s1.jsonl"), "{}\n").unwrap();
        std::fs::write(source.join("sessions/--a--/s2.jsonl"), "{}\n").unwrap();
        // 非 .jsonl 不该被搬
        std::fs::write(source.join("sessions/--a--/notes.txt"), "x").unwrap();

        let report = seed_into(&source, &target).unwrap();
        assert!(report.acted);
        assert_eq!(
            report.configs.len(),
            5,
            "4 个配置 + desktop-access：{report:?}"
        );
        assert_eq!(report.sessions, 2);

        let configs = target.join("configs");
        for name in [
            "auth.json",
            "models.json",
            "settings.json",
            "trust.json",
            "desktop-access.json",
        ] {
            assert!(configs.join(name).is_file(), "{name} 应被复制");
        }
        assert!(!configs.join("mcp-cache.json").exists(), "pi 的缓存不搬");
        assert!(target.join("sessions/--a--/s1.jsonl").is_file());
        assert!(!target.join("sessions/--a--/notes.txt").exists());

        // 源目录一个字节都不能少
        assert!(source.join("auth.json").is_file());
        assert!(source.join("sessions/--a--/s1.jsonl").is_file());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(configs.join("auth.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "凭据副本必须是 0600");
        }

        // 再播一次：什么都不做
        let again = seed_into(&source, &target).unwrap();
        assert!(!again.acted, "已播种过就不该再动");
        assert!(again.configs.is_empty());
        assert_eq!(again.sessions, 0, "已存在的会话不覆盖");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 源目录不存在时安静地什么都不做（全新机器、没装过 pi）。
    #[test]
    fn seeding_without_a_source_is_a_noop() {
        let dir = std::env::temp_dir().join(format!("toiler-seed-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let report = seed_into(&dir.join("missing"), &dir.join("target")).unwrap();
        assert_eq!(report, SeedReport::default());
        assert!(!dir.join("target").exists(), "不该凭空建目录");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// pi 的 agent 目录只作为播种源，且要能被环境变量覆盖。
    #[test]
    fn legacy_pi_dir_is_configurable() {
        let expected = std::env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".pi/agent"));
        assert_eq!(legacy_pi_agent_dir(), expected);
        assert_eq!(legacy_project_db(), expected.join("pi.sqlite"));
    }

    #[test]
    fn absolute_path_detection() {
        assert!(is_absolute_path("/a"));
        assert!(is_absolute_path("C:\\x"));
        assert!(!is_absolute_path("a/b"));
        assert!(!is_absolute_path("~/x"));
    }
}
