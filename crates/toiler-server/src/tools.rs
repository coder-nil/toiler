//! 内置工具集：pi 默认启用的编码工具 + Pi Desktop 的三个桌面工具。
//!
//! 编码工具来自 `rpi-tools`，执行环境是进程内的 `OsExecutionEnv`（cwd = 会话的 cwd）；
//! `system_time` / `task_progress` / `ask_user` 在 [`crate::desktop_tools`] 里，
//! 对应 pi-desktop 的三份 TS 内联扩展。
//!
//! 顺序就是 `get_tools` 返回的顺序，也是提示词里 `<tools>` 段的顺序（未启用会被过滤）。

use std::path::PathBuf;
use std::sync::Arc;

use rpi_ai::Model;
use rpi_harness::types::HarnessTool;
use rpi_tools::{
    create_bash_tool, create_edit_tool, create_find_tool, create_grep_tool, create_ls_tool,
    create_read_tool, create_write_tool, ExecutionToolContext, OsExecutionEnv,
};

use crate::desktop_tools::{
    create_ask_user_tool, create_system_time_tool, create_task_progress_tool, TaskProgressStore,
    UiBridge,
};

/// 不受工具预设影响的工具：pi 的 `set_tools` 会把扩展工具加回激活集合
/// （`withExtensionTools`），所以「只读」「默认」等预设不会把这三个关掉。
pub const ALWAYS_ON_TOOLS: [&str; 3] = ["system_time", "task_progress", "ask_user"];

/// 返回 `(工具名, HarnessTool)`。
pub fn build_tools(
    cwd: &str,
    _model: &Model,
    ui: Arc<UiBridge>,
    tasks: Arc<TaskProgressStore>,
) -> Vec<(String, HarnessTool)> {
    let env = Arc::new(OsExecutionEnv::with_cwd(PathBuf::from(cwd)));
    let context = ExecutionToolContext::new(env.clone(), Some(env));

    let tools: Vec<Arc<dyn rpi_agent::AgentTool>> = vec![
        create_read_tool(&context, None),
        create_bash_tool(&context, None),
        create_edit_tool(&context),
        create_write_tool(&context),
        create_grep_tool(&context, None),
        create_find_tool(&context, None),
        create_ls_tool(&context, None),
        // 桌面自带工具排在最后：pi 的面板里它们也是跟在编码工具后面的。
        create_system_time_tool(),
        create_task_progress_tool(tasks),
        create_ask_user_tool(ui),
    ];

    tools
        .into_iter()
        .map(|tool| (tool.schema().name.clone(), HarnessTool::new(tool)))
        .collect()
}
