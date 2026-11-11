//! `V3SessionStorage` —— 直接读写 pi 的 v3 `.jsonl` 会话文件的 `SessionStorage`。
//!
//! ## 为什么不自己啃整个 trait
//!
//! `rpi_harness::session::SessionStorage` 有 30+ 个方法（entries / records /
//! lanes / progress / 分页视图 / 统计…）。手写一遍既慢又容易和 rpi 的语义跑偏。
//!
//! 这里采用**委托 + 写穿**：
//!
//! - **读**：把 v3 文件逐行回放成 `SessionState`，灌进 rpi 自己的
//!   `InMemorySessionStorage`（`from_state`）。所有读方法直接转发给它，
//!   分支查询、分页、统计全部由 rpi 的原生实现负责。
//! - **写**：`append_entry` 先落内存（拿到 rpi 盖好 `seq`/`parentId` 的
//!   `Entry`），再按 v3 格式 append 一行到文件尾部。
//!
//! **老行永不重写**：只 append，所以 pi 仍然能打开同一个文件，
//! 我们也不会因为字段丢失（rpi 没有 `durationMs` 之类）而破坏历史。
//!
//! ## 回放时补的三处 v3 缺口
//!
//! 1. **没有 `seq`**：按成功落地的顺序递增（`state.next_sequence()`），
//!    失败的行不推进，避免「一条坏行 → 整串非连续」的级联。
//! 2. **可能分支**：v3 是 `parentId` 链，`rewind`/`edit` 会让 `parentId`
//!    指向非叶子。rpi 要求 `parentId == lane 叶子`，所以先把 main 的叶子
//!    退回到 parent（`SessionMutation::Lane`）再 append。
//! 3. **`stopReason: "toolUse"`**：见 [`crate::v3::message`]，读时小写、写时还原。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rpi_harness::error::SessionResult;
use rpi_harness::session::memory::Clock;
use rpi_harness::session::state::SessionState;
use rpi_harness::session::types::IdGenerator;
use rpi_harness::session::types::{
    AssistantFrameRecord, BranchBounds, BranchEntryPage, BranchEntryPageQuery, BranchOverview,
    Entry, EntryAncestry, EntryQuery, LanePointer, LaneRecord, LogItem, LogOptions,
    MessageSummaryPage, MessageSummaryQuery, MessageWindowPage, MessageWindowQuery,
    ProvisionedEntry, RecordPage, RecordPageQuery, RecordQuery, SessionCreateOptions,
    SessionLaneBranch, SessionMetadata, SessionMutation, SessionStats, SessionStorage,
    TranscriptPage, TranscriptPageQuery,
};
use rpi_harness::session::InMemorySessionStorage;
use rpi_harness::session::{DefaultIdGenerator, SystemClock};
use serde_json::Value;

use crate::home::session_file_name;
use crate::v3::{encode_entry, parse_entry_line, V3Error, V3Header};

/// rpi 的默认 lane 名（`Session` 门面也用这个）。
pub const DEFAULT_LANE: &str = "main";

/// 一次加载的结果统计，用于诊断与测试断言。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadReport {
    /// 成功回放的条目数。
    pub loaded_entries: usize,
    /// 被跳过的行（坏 JSON / 无法映射 / 回放违反 rpi 不变量）。
    pub skipped: Vec<String>,
    /// 文件里有几条 v3 里我们写不回去的条目（例如 rpi 的 active_tools_change）。
    pub unrepresentable_writes: usize,
}

/// 追加写入器：只往后写，绝不重写已有字节。
#[derive(Debug)]
pub struct V3Appender {
    path: PathBuf,
    file: Mutex<Option<File>>,
    warnings: Mutex<Vec<String>>,
    unrepresentable: Mutex<usize>,
}

impl V3Appender {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: Mutex::new(None),
            warnings: Mutex::new(Vec::new()),
            unrepresentable: Mutex::new(0),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 写一行（自动补 `\n`）。失败只记录，不打断对话。
    fn append_line(&self, line: &str) {
        let mut guard = self.file.lock().expect("appender not poisoned");
        if guard.is_none() {
            match OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
            {
                Ok(f) => *guard = Some(f),
                Err(e) => {
                    self.warnings
                        .lock()
                        .expect("warnings")
                        .push(format!("打开 {} 失败：{e}", self.path.display()));
                    return;
                }
            }
        }
        if let Some(file) = guard.as_mut() {
            if let Err(e) = file.write_all(line.as_bytes()) {
                self.warnings
                    .lock()
                    .expect("warnings")
                    .push(format!("写 {} 失败：{e}", self.path.display()));
            } else {
                let _ = file.flush();
            }
        }
    }

    /// 把一条 entry 追加成 v3 行；v3 表达不了的类型只记录并跳过。
    pub fn append_entry(&self, entry: &Entry) {
        match encode_entry(entry) {
            Ok(value) => {
                let mut line = value.to_string();
                line.push('\n');
                self.append_line(&line);
            }
            Err(V3Error::Unrepresentable { entry_type }) => {
                *self.unrepresentable.lock().expect("count") += 1;
                self.warnings
                    .lock()
                    .expect("warnings")
                    .push(format!("条目类型 {entry_type} 无法用 v3 表达，只留在内存"));
            }
            Err(e) => {
                self.warnings
                    .lock()
                    .expect("warnings")
                    .push(format!("编码条目失败：{e}"));
            }
        }
    }

    /// 取走累积的告警（诊断/测试用）。
    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut self.warnings.lock().expect("warnings"))
    }

    pub fn unrepresentable_count(&self) -> usize {
        *self.unrepresentable.lock().expect("count")
    }
}

/// 一个 v3 会话文件对应的 `SessionStorage`。
pub struct V3SessionStorage {
    metadata: SessionMetadata,
    inner: InMemorySessionStorage,
    appender: Arc<V3Appender>,
    report: LoadReport,
}

impl std::fmt::Debug for V3SessionStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("V3SessionStorage")
            .field("id", &self.metadata.id)
            .field("path", &self.appender.path)
            .field("loaded_entries", &self.report.loaded_entries)
            .finish()
    }
}

impl Clone for V3SessionStorage {
    fn clone(&self) -> Self {
        Self {
            metadata: self.metadata.clone(),
            inner: self.inner.clone(),
            appender: Arc::clone(&self.appender),
            report: self.report.clone(),
        }
    }
}

impl V3SessionStorage {
    /// 读文件 → 回放 → 建 storage。文件不存在时报错（新建请走 repo）。
    pub fn load(path: impl AsRef<Path>) -> SessionResult<Self> {
        Self::load_with(path, Arc::new(SystemClock), Arc::new(default_ids()))
    }

    pub fn load_with(
        path: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> SessionResult<Self> {
        let path = path.as_ref().to_path_buf();
        let text = std::fs::read_to_string(&path).map_err(|e| {
            rpi_harness::error::SessionError::storage(format!(
                "读不到会话文件 {}：{e}",
                path.display()
            ))
        })?;
        let (header, state, mut report) = replay_text(&text)?;
        report.unrepresentable_writes = 0;

        let metadata = SessionMetadata {
            id: header.id.clone(),
            created_at: header.created_at,
            parent_session_id: None,
        };
        let inner = InMemorySessionStorage::from_state(
            metadata.clone(),
            state,
            Arc::clone(&clock),
            Arc::clone(&ids),
        );
        Ok(Self {
            metadata,
            inner,
            appender: Arc::new(V3Appender::new(path)),
            report,
        })
    }

    /// 新建一个 v3 文件（写 header）并返回它的 storage。
    pub fn create_at(
        path: impl AsRef<Path>,
        header: &V3Header,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> SessionResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                rpi_harness::error::SessionError::storage(format!(
                    "创建目录 {} 失败：{e}",
                    parent.display()
                ))
            })?;
        }
        std::fs::write(&path, header.to_line()).map_err(|e| {
            rpi_harness::error::SessionError::storage(format!(
                "写会话文件 {} 失败：{e}",
                path.display()
            ))
        })?;
        let metadata = SessionMetadata {
            id: header.id.clone(),
            created_at: header.created_at,
            parent_session_id: None,
        };
        let inner = InMemorySessionStorage::new(metadata.clone(), clock, ids);
        Ok(Self {
            metadata,
            inner,
            appender: Arc::new(V3Appender::new(path)),
            report: LoadReport::default(),
        })
    }

    pub fn path(&self) -> &Path {
        self.appender.path()
    }

    /// 加载统计（跳过了哪些行、有多少条目写不回 v3）。
    pub fn load_report(&self) -> &LoadReport {
        &self.report
    }

    /// 累积的写入告警。
    pub fn take_warnings(&self) -> Vec<String> {
        self.appender.take_warnings()
    }

    /// 只读地看一个文件：header + 条目数 + 会话名。
    pub fn inspect(path: impl AsRef<Path>) -> anyhow::Result<SessionFileInfo> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)?;
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let header = lines
            .next()
            .ok_or_else(|| anyhow::anyhow!("{} 是空文件", path.display()))?;
        let header = V3Header::parse(header)?;
        let mut entries = 0usize;
        let mut name = None;
        for line in lines {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            entries += 1;
            if v.get("type").and_then(Value::as_str) == Some("session_info") {
                if let Some(n) = v.get("name").and_then(Value::as_str) {
                    name = Some(n.to_string());
                }
            }
        }
        Ok(SessionFileInfo {
            id: header.id,
            created_at: header.created_at,
            cwd: header.cwd,
            path: path.to_path_buf(),
            entries,
            name,
        })
    }

    fn note_append(&self, entry: &Entry) {
        self.appender.append_entry(entry);
    }
}

/// 一个 v3 会话文件的只读摘要（列目录用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFileInfo {
    pub id: String,
    pub created_at: i64,
    pub cwd: String,
    pub path: PathBuf,
    pub entries: usize,
    pub name: Option<String>,
}

fn default_ids() -> DefaultIdGenerator {
    DefaultIdGenerator::new()
}

/// 把一个 v3 文件全文回放进 `SessionState`。
fn replay_text(text: &str) -> SessionResult<(V3Header, SessionState, LoadReport)> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header_line = lines
        .next()
        .ok_or_else(|| rpi_harness::error::SessionError::storage("会话文件是空的"))?;
    let header = V3Header::parse(header_line).map_err(storage_err)?;

    let mut state = SessionState::new();
    let mut report = LoadReport::default();
    // pi 把会话名存成 session_info 条目；rpi 的名字是「fact」，回放完补一条
    let mut name: Option<String> = None;

    for (idx, line) in lines.enumerate() {
        let line_no = idx + 2; // header 是第 1 行
        let seq = state.next_sequence();
        let parsed = match parse_entry_line(line, seq, line_no) {
            Ok(p) => p,
            Err(e) => {
                report.skipped.push(e.to_string());
                continue;
            }
        };

        // 分支：parentId 指向非叶子时，先把 main 叶子退回到 parent
        let mut lane = Some(DEFAULT_LANE.to_string());
        let parent = parsed.parent_id.clone();
        let leaf = main_leaf(&state);
        if parent != leaf {
            match parent.as_deref() {
                Some(p) if state.get_entry(p).is_some() => {
                    let rewind = SessionMutation::Lane {
                        seq: state.next_sequence(),
                        lane: DEFAULT_LANE.to_string(),
                        leaf_id: Some(p.to_string()),
                    };
                    if let Err(e) = state.apply_mutation(rewind) {
                        report
                            .skipped
                            .push(format!("第 {line_no} 行：回退 lane 叶子失败：{e}"));
                    }
                }
                // 孤立的根（父已被删）或父缺失：按 fork 路径不校验链
                _ => lane = None,
            }
        }

        if let Some(custom) = as_session_info(&parsed.entry) {
            name = Some(custom);
        }

        let mutation = SessionMutation::Entry {
            seq: state.next_sequence(),
            timestamp: parsed.entry.base().timestamp,
            lane,
            entry: parsed.entry,
        };
        match state.apply_mutation(mutation) {
            Ok(_) => report.loaded_entries += 1,
            Err(e) => report.skipped.push(format!("第 {line_no} 行：{e}")),
        }
    }

    if let Some(name) = name {
        let seq = state.next_sequence();
        let _ = state.apply_mutation(SessionMutation::FactName {
            seq,
            name: Some(name),
        });
    }

    Ok((header, state, report))
}

/// `Custom` 条目里恢复 pi 的 `session_info` 名字。
fn as_session_info(entry: &Entry) -> Option<String> {
    let Entry::Custom(custom) = entry else {
        return None;
    };
    if custom.custom_type != "session_info" {
        return None;
    }
    custom
        .data
        .as_ref()
        .and_then(|d| d.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn main_leaf(state: &SessionState) -> Option<String> {
    state
        .get_lanes()
        .into_iter()
        .find(|l| l.lane == DEFAULT_LANE)
        .and_then(|l| l.leaf_id)
}

fn storage_err(e: V3Error) -> rpi_harness::error::SessionError {
    rpi_harness::error::SessionError::storage(e.to_string())
}

/// 建一个 `SessionMetadata`（供 repo 使用）。
pub fn metadata_for(header: &V3Header) -> SessionMetadata {
    SessionMetadata {
        id: header.id.clone(),
        created_at: header.created_at,
        parent_session_id: None,
    }
}

/// 该 id 的会话文件名（供 repo 使用）。
pub fn file_name_for(header: &V3Header) -> String {
    session_file_name(header.created_at, &header.id)
}

/// 方便 repo 构造一个待写的 provisioned entry（自定义条目）。
pub fn custom_entry(
    id: impl Into<String>,
    custom_type: &str,
    data: Option<Value>,
) -> ProvisionedEntry {
    ProvisionedEntry {
        id: id.into(),
        kind: rpi_harness::session::types::ProvisionedKind::Custom {
            custom_type: custom_type.to_string(),
            data,
        },
    }
}

// ---------------------------------------------------------------------------
// SessionStorage：绝大多数方法直接转发给内存后端，只有 append_entry 系列写穿
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl SessionStorage for V3SessionStorage {
    fn metadata(&self) -> SessionMetadata {
        self.metadata.clone()
    }

    async fn get_lanes(&self) -> SessionResult<Vec<LanePointer>> {
        self.inner.get_lanes().await
    }

    async fn create_lane(&self, lane: &str, at: Option<&str>) -> SessionResult<()> {
        self.inner.create_lane(lane, at).await
    }

    async fn move_lane(&self, lane: &str, to: Option<&str>) -> SessionResult<()> {
        self.inner.move_lane(lane, to).await
    }

    async fn lane_branch(&self, lane: &str) -> SessionResult<SessionLaneBranch> {
        self.inner.lane_branch(lane).await
    }

    async fn append_progress(
        &self,
        records: Vec<AssistantFrameRecord>,
    ) -> SessionResult<Vec<AssistantFrameRecord>> {
        self.inner.append_progress(records).await
    }

    async fn read_progress(&self, run_id: &str) -> SessionResult<Vec<AssistantFrameRecord>> {
        self.inner.read_progress(run_id).await
    }

    async fn prune_completed_progress(&self) -> SessionResult<usize> {
        self.inner.prune_completed_progress().await
    }

    async fn history_revision(&self) -> SessionResult<u64> {
        self.inner.history_revision().await
    }

    async fn append_entry_if_revision(
        &self,
        entry: ProvisionedEntry,
        lane: &str,
        expected: u64,
    ) -> SessionResult<Entry> {
        let full = self
            .inner
            .append_entry_if_revision(entry, lane, expected)
            .await?;
        self.note_append(&full);
        Ok(full)
    }

    async fn append_record_if_revision(
        &self,
        record: LaneRecord,
        expected: u64,
    ) -> SessionResult<LaneRecord> {
        self.inner.append_record_if_revision(record, expected).await
    }

    async fn append_entry(&self, entry: ProvisionedEntry, lane: &str) -> SessionResult<Entry> {
        let full = self.inner.append_entry(entry, lane).await?;
        self.note_append(&full);
        Ok(full)
    }

    async fn append_record(&self, record: LaneRecord) -> SessionResult<LaneRecord> {
        self.inner.append_record(record).await
    }

    async fn append_queue_cancellations(
        &self,
        records: Vec<LaneRecord>,
    ) -> SessionResult<Vec<LaneRecord>> {
        self.inner.append_queue_cancellations(records).await
    }

    async fn append_record_and_entry(
        &self,
        record: LaneRecord,
        entry: ProvisionedEntry,
        lane: &str,
    ) -> SessionResult<(LaneRecord, Entry)> {
        let record = self.inner.append_record(record).await?;
        let entry = self.inner.append_entry(entry, lane).await?;
        self.note_append(&entry);
        Ok((record, entry))
    }

    async fn append_record_entry_and_record(
        &self,
        before: LaneRecord,
        entry: ProvisionedEntry,
        lane: &str,
        after: LaneRecord,
    ) -> SessionResult<(LaneRecord, Entry, LaneRecord)> {
        let before = self.inner.append_record(before).await?;
        let entry = self.inner.append_entry(entry, lane).await?;
        self.note_append(&entry);
        let after = self.inner.append_record(after).await?;
        Ok((before, entry, after))
    }

    async fn append_records(&self, records: Vec<LaneRecord>) -> SessionResult<Vec<LaneRecord>> {
        self.inner.append_records(records).await
    }

    async fn context_usage(
        &self,
        lane: &str,
    ) -> SessionResult<Option<rpi_harness::compaction::ContextUsageEstimate>> {
        self.inner.context_usage(lane).await
    }

    async fn transcript_page(
        &self,
        lane: &str,
        query: &TranscriptPageQuery,
    ) -> SessionResult<Option<TranscriptPage>> {
        self.inner.transcript_page(lane, query).await
    }

    async fn branch_entry_page(
        &self,
        lane: &str,
        query: &BranchEntryPageQuery,
    ) -> SessionResult<BranchEntryPage> {
        self.inner.branch_entry_page(lane, query).await
    }

    async fn branch_overview(&self, lane: &str) -> SessionResult<BranchOverview> {
        self.inner.branch_overview(lane).await
    }

    async fn message_summaries(
        &self,
        lane: &str,
        query: &MessageSummaryQuery,
    ) -> SessionResult<MessageSummaryPage> {
        self.inner.message_summaries(lane, query).await
    }

    async fn message_window(
        &self,
        lane: &str,
        query: &MessageWindowQuery,
    ) -> SessionResult<MessageWindowPage> {
        self.inner.message_window(lane, query).await
    }

    async fn get_entry(&self, id: &str) -> SessionResult<Option<Entry>> {
        self.inner.get_entry(id).await
    }

    async fn find_entries(&self, query: &EntryQuery) -> SessionResult<Vec<Entry>> {
        self.inner.find_entries(query).await
    }

    async fn find_entries_on_branch(
        &self,
        query: &EntryQuery,
        bounds: &BranchBounds,
        start: &str,
    ) -> SessionResult<Vec<Entry>> {
        self.inner
            .find_entries_on_branch(query, bounds, start)
            .await
    }

    async fn branch_ancestry(
        &self,
        bounds: &BranchBounds,
        start: &str,
    ) -> SessionResult<Vec<EntryAncestry>> {
        self.inner.branch_ancestry(bounds, start).await
    }

    async fn context_on_branch(
        &self,
        bounds: &BranchBounds,
        start: &str,
    ) -> SessionResult<rpi_harness::session::context::SessionContext> {
        self.inner.context_on_branch(bounds, start).await
    }

    async fn find_records(&self, query: &RecordQuery) -> SessionResult<Vec<LaneRecord>> {
        self.inner.find_records(query).await
    }

    async fn record_page(&self, query: &RecordPageQuery) -> SessionResult<RecordPage> {
        self.inner.record_page(query).await
    }

    async fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<usize>,
    ) -> SessionResult<Vec<rpi_harness::session::types::OperationStartedRecord>> {
        self.inner.find_open_operations(lane, limit).await
    }

    async fn get_log(&self, options: &LogOptions) -> SessionResult<Vec<LogItem>> {
        self.inner.get_log(options).await
    }

    async fn get_name(&self) -> SessionResult<Option<String>> {
        self.inner.get_name().await
    }

    /// pi 把会话名存成 `session_info` 条目，所以这里也写一条——这样 pi 打开
    /// 我们产生的会话时，标题是对的。
    async fn set_name(&self, name: Option<&str>) -> SessionResult<()> {
        self.inner.set_name(name).await?;
        let Some(name) = name else {
            return Ok(());
        };
        let id = format!("pw{}", uuidish(self.metadata.created_at));
        let entry = custom_entry(
            id,
            "session_info",
            Some(serde_json::json!({ "name": name })),
        );
        // 名字落不进文件不算致命：内存里已经生效
        let _ = self.append_entry(entry, DEFAULT_LANE).await;
        Ok(())
    }

    async fn get_label(&self, id: &str) -> SessionResult<Option<String>> {
        self.inner.get_label(id).await
    }

    async fn set_label(&self, id: &str, label: Option<&str>) -> SessionResult<()> {
        self.inner.set_label(id, label).await
    }

    async fn get_stats(&self) -> SessionResult<SessionStats> {
        self.inner.get_stats().await
    }

    async fn drain(&self) {
        self.inner.drain().await
    }
}

/// 一个够用的短 id（session_info 条目用；不需要和 uuidv7 一样强）。
fn uuidish(salt: i64) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!(
        "{:08x}",
        (now ^ (salt as u64)).wrapping_mul(0x9E37_79B9_7F4A_7C15) as u32
    )
}

/// 供 repo 复用：`SessionCreateOptions` → (id, cwd)
pub fn split_create_options(options: &SessionCreateOptions) -> (Option<String>, Option<String>) {
    let cwd = options
        .metadata
        .as_ref()
        .and_then(|m| m.get("cwd"))
        .and_then(Value::as_str)
        .map(str::to_string);
    (options.id.clone(), cwd)
}
