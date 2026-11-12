//! toiler 的会话层。
//!
//! 目标：**直接读写 pi 自己的 v3 `.jsonl` 会话文件**，不迁移、不改格式。
//! 这样 pi 和 toiler 可以并行打开同一份会话历史。
//!
//! ```text
//! ~/.toiler/agent/sessions/--<cwd 编码>--/<ISO>_<id>.jsonl      ← 与 pi 同构
//! ```
//!
//! # 四个模块
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`time`] | ISO-8601 ↔ unix 毫秒（pi 写字符串、rpi 要整数） |
//! | [`v3`] | v3 文件的编解码：header / 条目行 / 消息 |
//! | [`storage`] | `SessionStorage` 实现：回放 v3 → rpi 内存后端，写时 append 回文件 |
//! | [`repo`] | 仓库：列目录、按 id 打开、新建、删除、fork |
//!
//! # 为什么绕开 rpi 的 jsonl 层
//!
//! `rpi_harness::session::jsonl` 只认 v4（`{"kind":"header","version":4}` +
//! 条目带 `seq`），对 `version != 4` 直接报错。我们实现的是 `SessionStorage`
//! 这个**公开纯 async trait**，把 v3 的语义喂给 rpi 自己的 reducer，
//! 从而保持「rpi 是未修改的 crates.io 依赖」这个前提。
//!
//! # 用法
//!
//! ```no_run
//! use std::sync::Arc;
//! use toiler_session::{ToilerHome, V3SessionRepo};
//! use rpi_harness::session::Session;
//!
//! # async fn demo() -> anyhow::Result<()> {
//! let home = ToilerHome::discover()?;
//! home.ensure()?;
//! let repo = V3SessionRepo::new(home.sessions_root());
//!
//! // 列出所有会话（按创建时间倒序）
//! for info in repo.scan() {
//!     println!("{} {} {}", info.id, info.cwd, info.entries);
//! }
//!
//! // 打开一个已有会话，接到 rpi 的 Session 门面上
//! let storage = repo.open_by_id("<id>")?;
//! let session = Session::new(Arc::new(storage), None);
//! # Ok(()) }
//! ```

pub mod home;
pub mod repo;
pub mod storage;
pub mod time;
pub mod v3;

pub use home::{session_directory_name, session_file_name, ToilerHome};
pub use repo::V3SessionRepo;
pub use storage::{LoadReport, SessionFileInfo, V3Appender, V3SessionStorage, DEFAULT_LANE};
pub use time::{iso_to_ms, ms_to_iso};
pub use v3::{V3Error, V3Header};
