//! `V3SessionRepo` —— 把一个 sessions 根目录当成 pi 的会话仓库。
//!
//! 目录结构与 pi 逐字一致（见 [`crate::home`]），所以：
//!
//! - 指向 `~/.workman/agent/sessions` → workman 自己的会话
//! - 指向 `~/.pi/agent/sessions` → 直接读 pi 的历史会话（只读用）
//!
//! 列表只读每个文件的 **header**（不解析条目），这样扫 500+ 会话是毫秒级；
//! 需要名字/条目数时用 [`V3SessionRepo::inspect`]。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rpi_harness::error::{SessionError, SessionResult};
use rpi_harness::session::memory::Clock;
use rpi_harness::session::types::IdGenerator;
use rpi_harness::session::{DefaultIdGenerator, SystemClock};
use rpi_harness::session::types::{
    ForkOptions, SessionCreateOptions, SessionMetadata, SessionRepo, SessionStorage,
};

use crate::home::{session_directory_name, session_id_from_file_name};
use crate::storage::{metadata_for, SessionFileInfo, V3SessionStorage};
use crate::v3::V3Header;

/// 会话仓库：一个 sessions 根目录 + 一个 id→路径 索引。
pub struct V3SessionRepo {
    sessions_root: PathBuf,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    index: Mutex<Option<HashMap<String, PathBuf>>>,
}

impl V3SessionRepo {
    pub fn new(sessions_root: impl Into<PathBuf>) -> Self {
        Self {
            sessions_root: sessions_root.into(),
            clock: Arc::new(SystemClock),
            ids: Arc::new(DefaultIdGenerator::new()),
            index: Mutex::new(None),
        }
    }

    pub fn with_clock_and_ids(
        sessions_root: impl Into<PathBuf>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            sessions_root: sessions_root.into(),
            clock,
            ids,
            index: Mutex::new(None),
        }
    }

    pub fn sessions_root(&self) -> &Path {
        &self.sessions_root
    }

    pub fn clock(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock)
    }

    pub fn ids(&self) -> Arc<dyn IdGenerator> {
        Arc::clone(&self.ids)
    }

    /// 扫描所有 `--cwd--` 目录下的 `.jsonl`，只解析 header。返回按创建时间倒序。
    pub fn scan(&self) -> Vec<SessionFileInfo> {
        let mut out = Vec::new();
        let Ok(dirs) = std::fs::read_dir(&self.sessions_root) else {
            return out;
        };
        for dir in dirs.flatten() {
            let dir_path = dir.path();
            if !dir_path.is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&dir_path) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().map_or(true, |e| e != "jsonl") {
                    continue;
                }
                if let Ok(info) = header_only(&path) {
                    out.push(info);
                }
            }
        }
        out.sort_by_key(|i| std::cmp::Reverse(i.created_at));
        out
    }

    /// 读一个会话的完整摘要（含名字与条目数）。比 [`Self::scan`] 慢。
    pub fn inspect(&self, id: &str) -> Option<SessionFileInfo> {
        let path = self.path_of(id)?;
        V3SessionStorage::inspect(path).ok()
    }

    fn path_of(&self, id: &str) -> Option<PathBuf> {
        {
            let guard = self.index.lock().expect("index");
            if let Some(map) = guard.as_ref() {
                if let Some(p) = map.get(id) {
                    return Some(p.clone());
                }
            }
        }
        let scanned = self.scan();
        let map: HashMap<String, PathBuf> = scanned
            .into_iter()
            .map(|i| (i.id.clone(), i.path.clone()))
            .collect();
        let found = map.get(id).cloned();
        *self.index.lock().expect("index") = Some(map);
        found
    }

    fn refresh_index(&self) {
        let map: HashMap<String, PathBuf> = self
            .scan()
            .into_iter()
            .map(|i| (i.id.clone(), i.path))
            .collect();
        *self.index.lock().expect("index") = Some(map);
    }

    /// 在某个 cwd 下建新会话文件。返回已就绪的 storage。
    pub fn create_at(
        &self,
        cwd: &str,
        options: &SessionCreateOptions,
    ) -> SessionResult<V3SessionStorage> {
        let id = options
            .id
            .clone()
            .unwrap_or_else(|| self.ids.next());
        let created_at = self.clock.now_ms();
        let header = V3Header::new(id, created_at, cwd);
        let dir = self.sessions_root.join(session_directory_name(cwd));
        let file = dir.join(crate::home::session_file_name(
            header.created_at,
            &header.id,
        ));
        let storage = V3SessionStorage::create_at(
            file,
            &header,
            Arc::clone(&self.clock),
            Arc::clone(&self.ids),
        )?;
        self.refresh_index();
        Ok(storage)
    }

    /// 打开已有会话（按 id 找文件）。
    pub fn open_by_id(&self, id: &str) -> SessionResult<V3SessionStorage> {
        let path = self.path_of(id).ok_or_else(|| {
            SessionError::new(
                rpi_harness::error::SessionErrorCode::NotFound,
                format!("找不到会话 {id}"),
            )
        })?;
        V3SessionStorage::load_with(path, Arc::clone(&self.clock), Arc::clone(&self.ids))
    }

    /// 删除会话文件。
    pub fn delete_by_id(&self, id: &str) -> SessionResult<()> {
        let path = self.path_of(id).ok_or_else(|| {
            SessionError::new(
                rpi_harness::error::SessionErrorCode::NotFound,
                format!("找不到会话 {id}"),
            )
        })?;
        std::fs::remove_file(&path).map_err(|e| {
            SessionError::storage(format!("删除 {} 失败：{e}", path.display()))
        })?;
        self.refresh_index();
        Ok(())
    }
}

#[async_trait::async_trait]
impl SessionRepo for V3SessionRepo {
    type Storage = V3SessionStorage;

    async fn create(&self, options: &SessionCreateOptions) -> SessionResult<Self::Storage> {
        let cwd = options
            .metadata
            .as_ref()
            .and_then(|m| m.get("cwd"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(current_dir_string);
        self.create_at(&cwd, options)
    }

    async fn open(&self, metadata: &SessionMetadata) -> SessionResult<Self::Storage> {
        self.open_by_id(&metadata.id)
    }

    /// 注意：`SessionMetadata` 只有 id/created_at/parent，没有 cwd 与路径，
    /// 需要 cwd 的调用方请用 [`V3SessionRepo::scan`]。
    async fn list(&self) -> SessionResult<Vec<SessionMetadata>> {
        Ok(self
            .scan()
            .into_iter()
            .map(|i| SessionMetadata {
                id: i.id,
                created_at: i.created_at,
                parent_session_id: None,
            })
            .collect())
    }

    async fn delete(&self, metadata: &SessionMetadata) -> SessionResult<()> {
        self.delete_by_id(&metadata.id)
    }

    /// fork：把源会话按 `ForkOptions` 选定的一段条目复制进新文件。
    async fn fork(
        &self,
        source: &SessionMetadata,
        options: &SessionCreateOptions,
        fork: &ForkOptions,
    ) -> SessionResult<Self::Storage> {
        let source_storage = self.open_by_id(&source.id)?;

        let cwd = options
            .metadata
            .as_ref()
            .and_then(|m| m.get("cwd"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(current_dir_string);
        let target = self.create_at(&cwd, options)?;

        // 取源会话的条目（fork 语义：Tree = 全部；Branch = 到某个条目为止）
        let bound = match fork {
            ForkOptions::Tree => None,
            ForkOptions::Branch { entry_id, .. } => entry_id.clone(),
        };
        let entries = match bound {
            None => source_storage.find_entries(&Default::default()).await?,
            Some(id) => {
                let bounds = rpi_harness::session::types::BranchBounds {
                    start: Some(id),
                    ..Default::default()
                };
                let lane = {
                    let lanes = source_storage.get_lanes().await?;
                    lanes
                        .into_iter()
                        .find(|l| l.lane == crate::storage::DEFAULT_LANE)
                        .and_then(|l| l.leaf_id)
                        .unwrap_or_default()
                };
                let mut path = source_storage
                    .find_entries_on_branch(&Default::default(), &bounds, &lane)
                    .await?;
                path.reverse();
                path
            }
        };

        for entry in entries {
            let provisioned = to_provisioned(&entry);
            target.append_entry(provisioned, crate::storage::DEFAULT_LANE).await?;
        }
        self.refresh_index();
        Ok(target)
    }
}

fn to_provisioned(entry: &rpi_harness::session::types::Entry) -> rpi_harness::session::types::ProvisionedEntry {
    use rpi_harness::session::types::{Entry, ProvisionedEntry, ProvisionedKind};
    let kind = match entry {
        Entry::Message(m) => ProvisionedKind::Message {
            message: m.message.clone(),
            terminate: None,
        },
        Entry::ModelChange(m) => ProvisionedKind::ModelChange {
            provider: m.provider.clone(),
            model_id: m.model_id.clone(),
        },
        Entry::ThinkingLevel(t) => ProvisionedKind::ThinkingLevel {
            thinking_level: t.thinking_level.clone(),
        },
        Entry::ActiveTools(a) => ProvisionedKind::ActiveTools {
            active_tool_names: a.active_tool_names.clone(),
        },
        Entry::Compaction(c) => ProvisionedKind::Compaction {
            summary: c.summary.clone(),
            retained_tail: c.retained_tail.clone(),
            tokens_before: c.tokens_before,
            details: c.details.clone(),
            usage: c.usage.clone(),
        },
        Entry::BranchSummary(b) => ProvisionedKind::BranchSummary {
            from_id: b.from_id.clone(),
            summary: b.summary.clone(),
            details: b.details.clone(),
            usage: b.usage.clone(),
        },
        Entry::Custom(c) => ProvisionedKind::Custom {
            custom_type: c.custom_type.clone(),
            data: c.data.clone(),
        },
    };
    ProvisionedEntry {
        id: entry.id().to_string(),
        kind,
    }
}

/// 只读 header 拿 id/created_at/cwd（不解析条目）。
fn header_only(path: &Path) -> anyhow::Result<SessionFileInfo> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path)?;
    let mut first = String::new();
    BufReader::new(file).read_line(&mut first)?;
    let header = V3Header::parse(first.trim_end())?;
    // 条目数按行数粗算（不解析 JSON），够列表用
    let text = std::fs::read_to_string(path)?;
    let entries = text.lines().filter(|l| !l.trim().is_empty()).count() - 1;
    Ok(SessionFileInfo {
        id: header.id,
        created_at: header.created_at,
        cwd: header.cwd,
        path: path.to_path_buf(),
        entries,
        name: None,
    })
}

/// 当前工作目录（`create` 没带 cwd 时的兜底）。
pub fn current_dir_string() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 从文件名推出会话 id（方便 CLI 传文件名）。
pub fn id_from_path(path: &Path) -> Option<String> {
    session_id_from_file_name(&path.file_name()?.to_string_lossy()).map(str::to_string)
}

/// 便利：`SessionMetadata` + 路径 → 摘要。
pub fn info_of(path: &Path) -> anyhow::Result<SessionFileInfo> {
    V3SessionStorage::inspect(path)
}

/// 便利：给 header 造 metadata（repo 内部与测试用）。
pub fn metadata_of(header: &V3Header) -> SessionMetadata {
    metadata_for(header)
}
