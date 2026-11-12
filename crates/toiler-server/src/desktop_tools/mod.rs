//! 桌面自带工具（Pi Desktop 的 TS 内联扩展在 Rust 侧的实现）。
//!
//! pi-desktop 把三个工具写成 `InlineExtension`（`lib/system-time-tool.ts`、
//! `lib/task-progress-tool.ts`、`lib/ask-user-tool.ts`），通过 pi-coding-agent 的
//! 扩展 API 注册。toiler 用 rpi（Rust）且不加载 TS 扩展，所以这里是它们的等价实现：
//!
//! | 工具 | pi 的实现 | toiler 的依赖 |
//! |---|---|---|
//! | `system_time` | 纯本地计算 | 无 |
//! | `task_progress` | 状态机 + `ctx.ui.setStatus/setWidget` | [`UiBridge`]（事件） |
//! | `ask_user` | `ctx.ui.select/confirm/input/editor` | [`UiBridge`]（请求/响应往返） |
//!
//! 与 pi 的差异只在**实现语言与宿主通道**，工具名、参数 schema、返回文本与
//! `details` 形状都逐字对齐，前端（含手机端）不需要改任何东西。

pub mod ask_user;
pub mod system_time;
pub mod task_progress;
pub mod ui;

pub use ask_user::create_ask_user_tool;
pub use system_time::create_system_time_tool;
pub use task_progress::{create_task_progress_tool, TaskProgressStore};
pub use ui::UiBridge;
