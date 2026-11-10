//! 项目信任（`.pi` 项目资源 + `trust.json`）。
//!
//! pi 的规则（`trust-manager.js`）：
//!
//! - 只有当项目里有「需要信任才能加载」的资源时，才需要用户点头：
//!   `settings.json` / `mcp.json` / `extensions` / `skills` / `prompts` /
//!   `themes` / `SYSTEM.md` / `APPEND_SYSTEM.md`（在 `<cwd>/.pi/` 下），
//!   或从 `cwd` 往上一路找到的 `.agents/skills`（`$HOME/.agents/skills` 除外）。
//! - 决定存在 `<agentDir>/trust.json`，键是规范化后的绝对路径，值是
//!   `true` / `false` / `null`；查找时从 `cwd` 往上一层层找最近的条目。
//! - 没有任何需要信任的资源时，视作「已信任」。

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::paths;

const CONFIG_DIR: &str = ".pi";
const TRUST_REQUIRING: [&str; 8] = [
    "settings.json",
    "mcp.json",
    "extensions",
    "skills",
    "prompts",
    "themes",
    "SYSTEM.md",
    "APPEND_SYSTEM.md",
];

/// `trust.json` 的句柄。
#[derive(Debug, Clone)]
pub struct TrustStore {
    path: PathBuf,
}

impl Default for TrustStore {
    fn default() -> Self {
        Self::at(paths::agent_file("trust.json"))
    }
}

impl TrustStore {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Map<String, Value> {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Map::new();
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        }
    }

    /// 最近的信任决定；`None` = 没记录过。
    pub fn get(&self, cwd: &str) -> Option<bool> {
        let map = self.read();
        let mut dir = normalize(cwd);
        loop {
            if let Some(value) = map.get(&dir.to_string_lossy().to_string()) {
                if let Some(flag) = value.as_bool() {
                    return Some(flag);
                }
            }
            let parent = dir.parent().map(Path::to_path_buf);
            match parent {
                Some(parent) if parent != dir => dir = parent,
                _ => return None,
            }
        }
    }

    /// 写入一个决定（键排序、值为 `true`/`false`/`null`，与 pi 一致）。
    pub fn set(&self, cwd: &str, decision: Option<bool>) -> std::io::Result<()> {
        let key = normalize(cwd).to_string_lossy().to_string();
        let mut map = self.read();
        map.insert(key.clone(), decision.map(Value::from).unwrap_or(Value::Null));
        // 清掉「同一路径但写法不同」的旧键（大小写、符号链接）
        let canonical = std::fs::canonicalize(cwd).ok();
        if let Some(canonical) = canonical {
            let canonical = canonical.to_string_lossy().to_string();
            let dupes: Vec<String> = map
                .keys()
                .filter(|k| **k != key && *k == &canonical)
                .cloned()
                .collect();
            for dupe in dupes {
                map.remove(&dupe);
            }
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut sorted = Map::new();
        let mut keys: Vec<&String> = map.keys().collect();
        keys.sort();
        for key in keys {
            sorted.insert(key.clone(), map[key].clone());
        }
        let body = serde_json::to_string_pretty(&Value::Object(sorted)).unwrap_or_else(|_| "{}".into());
        std::fs::write(&self.path, format!("{body}\n"))
    }
}

fn normalize(cwd: &str) -> PathBuf {
    std::fs::canonicalize(cwd).unwrap_or_else(|_| paths::absolute(Path::new(cwd)))
}

/// 项目里是否有需要信任才能加载的资源（`hasTrustRequiringProjectResources`）。
pub fn requires_trust(cwd: &str) -> bool {
    let config_dir = paths::absolute(Path::new(cwd)).join(CONFIG_DIR);
    if TRUST_REQUIRING
        .iter()
        .any(|entry| config_dir.join(entry).exists())
    {
        return true;
    }
    let user_skills = paths::absolute(&paths::home_dir().join(".agents/skills"));
    let mut dir = paths::absolute(Path::new(cwd));
    loop {
        let candidate = dir.join(".agents/skills");
        if candidate != user_skills && candidate.exists() {
            return true;
        }
        match dir.parent().map(Path::to_path_buf) {
            Some(parent) if parent != dir => dir = parent,
            _ => return false,
        }
    }
}

/// 一次信任查询的结果（`72498.g`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustStatus {
    pub requires_trust: bool,
    pub trusted: bool,
}

pub fn status(store: &TrustStore, cwd: &str) -> TrustStatus {
    if !requires_trust(cwd) {
        return TrustStatus {
            requires_trust: false,
            trusted: true,
        };
    }
    TrustStatus {
        requires_trust: true,
        trusted: store.get(cwd) == Some(true),
    }
}

/// 记录信任（`72498.h`）。
pub fn mark_trusted(store: &TrustStore, cwd: &str) -> std::io::Result<TrustStatus> {
    let status = status(store, cwd);
    if status.requires_trust {
        store.set(cwd, Some(true))?;
        return Ok(TrustStatus {
            requires_trust: true,
            trusted: true,
        });
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_project_resources() {
        let dir = std::env::temp_dir().join(format!("workman-trust-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".pi")).unwrap();
        assert!(!requires_trust(dir.to_string_lossy().as_ref()));

        std::fs::write(dir.join(".pi/settings.json"), "{}").unwrap();
        assert!(requires_trust(dir.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stores_and_finds_nearest_decision() {
        let dir = std::env::temp_dir().join(format!("workman-trust-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = TrustStore::at(dir.join("trust.json"));
        let project = dir.join("proj/sub");
        std::fs::create_dir_all(&project).unwrap();

        store.set(dir.join("proj").to_string_lossy().as_ref(), Some(true)).unwrap();
        // 子目录继承父目录的决定
        assert_eq!(store.get(project.to_string_lossy().as_ref()), Some(true));
        assert_eq!(store.get(dir.to_string_lossy().as_ref()), None);

        store.set(dir.join("proj").to_string_lossy().as_ref(), Some(false)).unwrap();
        assert_eq!(store.get(project.to_string_lossy().as_ref()), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
