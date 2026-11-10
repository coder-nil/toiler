//! 内置工具集：与 pi 默认启用的那一组对齐（read / bash / edit / write / grep / find / ls）。
//!
//! 工具来自 `rpi-tools`，执行环境是进程内的 `OsExecutionEnv`（cwd = 会话的 cwd）。

use std::path::PathBuf;
use std::sync::Arc;

use rpi_ai::Model;
use rpi_harness::types::HarnessTool;
use rpi_tools::{
    create_bash_tool, create_edit_tool, create_find_tool, create_grep_tool, create_ls_tool,
    create_read_tool, create_write_tool, ExecutionToolContext, OsExecutionEnv,
};

/// 返回 `(工具名, HarnessTool)`，顺序与 pi 的默认工具列表一致。
pub fn build_tools(cwd: &str, _model: &Model) -> Vec<(String, HarnessTool)> {
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
    ];

    tools
        .into_iter()
        .map(|tool| (tool.schema().name.clone(), HarnessTool::new(tool)))
        .collect()
}
