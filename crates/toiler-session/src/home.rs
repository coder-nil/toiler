//! `~/.toiler/agent` 的目录约定。
//!
//! 与 pi 的 `~/.pi/agent` **同构**，这样两边的会话文件可以互相复制：
//!
//! ```text
//! ~/.toiler/agent/
//!   sessions/
//!     --Users-me-Development-project--/        ← cwd 编码（/ \ : → -）
//!       2026-11-10T06-41-44-431Z_<id>.jsonl    ← ISO（: . → -）_ id
//! ```
//!
//! 目录名与文件名规则来自 pi 自己的实现（`jsonl_session_directory_name` /
//! `session_file_name`），刻意逐字复刻，保证「把 pi 的会话复制进来就能用」。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::time::ms_to_iso;

/// toiler 的 home 根（默认 `~/.toiler`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToilerHome {
    root: PathBuf,
}

impl ToilerHome {
    /// `$TOILER_HOME` 优先，其次 `~/.toiler`。
    pub fn discover() -> Result<Self> {
        if let Some(dir) = std::env::var_os("TOILER_HOME") {
            return Ok(Self::at(PathBuf::from(dir)));
        }
        let home = std::env::var_os("HOME").context("HOME 未设置，无法定位 ~/.toiler")?;
        Ok(Self::at(PathBuf::from(home).join(".toiler")))
    }

    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `~/.toiler/agent`
    pub fn agent_dir(&self) -> PathBuf {
        self.root.join("agent")
    }

    /// `~/.toiler/agent/sessions`
    pub fn sessions_root(&self) -> PathBuf {
        self.agent_dir().join("sessions")
    }

    /// `~/.toiler/agent/models.json`（阶段 3 用，这里只给路径）
    pub fn models_path(&self) -> PathBuf {
        self.agent_dir().join("models.json")
    }

    /// 首次启动初始化目录；已存在则不动。只创建目录，不碰任何会话文件。
    pub fn ensure(&self) -> Result<()> {
        let sessions = self.sessions_root();
        std::fs::create_dir_all(&sessions)
            .with_context(|| format!("创建 {} 失败", sessions.display()))?;
        Ok(())
    }

    /// 某个 cwd 的会话目录。
    pub fn session_dir_for(&self, cwd: &str) -> PathBuf {
        self.sessions_root().join(session_directory_name(cwd))
    }
}

/// pi 的 cwd 目录名：去掉开头的分隔符，`/ \ :` 换成 `-`，两头加 `--`。
pub fn session_directory_name(cwd: &str) -> String {
    let stripped = cwd
        .strip_prefix('/')
        .or_else(|| cwd.strip_prefix('\\'))
        .unwrap_or(cwd);
    let replaced: String = stripped
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' => '-',
            other => other,
        })
        .collect();
    format!("--{replaced}--")
}

/// pi 的会话文件名：`<ISO 时间戳>: . 换 -` + `_` + id + `.jsonl`。
pub fn session_file_name(created_at: i64, id: &str) -> String {
    let iso = ms_to_iso(created_at);
    let safe = iso.replace([':', '.'], "-");
    format!("{safe}_{id}.jsonl")
}

/// 从文件名里把会话 id 抠出来（`…Z_<id>.jsonl`）；不符合规则返回 `None`。
pub fn session_id_from_file_name(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".jsonl")?;
    let (_, id) = stem.split_once('_')?;
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_name_matches_pi() {
        // 真实 pi 目录：~/.pi/agent/sessions/--Volumes-Chenxiaoni·SSD-…-toiler--
        assert_eq!(
            session_directory_name("/Volumes/Chenxiaoni·SSD/Development/RustProjects/toiler"),
            "--Volumes-Chenxiaoni·SSD-Development-RustProjects-toiler--"
        );
        assert_eq!(session_directory_name("/a/b"), "--a-b--");
        assert_eq!(session_directory_name("C:\\x"), "--C--x--");
    }

    #[test]
    fn file_name_matches_pi() {
        let ms = crate::time::iso_to_ms("2026-11-10T06:41:44.431Z").unwrap();
        assert_eq!(
            session_file_name(ms, "01a1c430-f1ef-7516-b8ed-33ac88cc72cc"),
            "2026-11-10T06-41-44-431Z_01a1c430-f1ef-7516-b8ed-33ac88cc72cc.jsonl"
        );
    }

    #[test]
    fn parses_id_back_out() {
        assert_eq!(
            session_id_from_file_name("2026-11-10T06-41-44-431Z_01a1c430-f1ef.jsonl"),
            Some("01a1c430-f1ef")
        );
        assert_eq!(session_id_from_file_name("broken.jsonl"), None);
    }

    #[test]
    fn ensure_creates_sessions_root() {
        let tmp = std::env::temp_dir().join(format!("toiler-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = ToilerHome::at(&tmp);
        home.ensure().unwrap();
        assert!(home.sessions_root().is_dir());
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
