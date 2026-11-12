//! 系统提示词：按 pi 的分节结构组装，并支持界面语言切换。
//!
//! # 为什么单独一个模块
//!
//! 早先 `agent_runtime` 只把一句话直接塞进 `AgentHarnessOptions::system_prompt`，
//! 于是模型不知道有哪些工具、不知道行为准则、也读不到项目的 AGENTS.md ——
//! 前端「系统提示词」面板还因为 `get_state` 不返回 `systemPrompt` 一直显示
//! 「尚未加载」。这里把 pi 的组装逻辑搬过来，作为**唯一**的文案来源，
//! `get_state` 与真正发给模型的文本因此永远一致。
//!
//! # 分节结构（与 pi 逐节对齐）
//!
//! 顺序来自 `@earendil-works/pi-coding-agent` 的 `buildSystemPromptSections()`：
//!
//! ```text
//! preamble → <tools> → <rules> → <project_context> → <cwd>
//! ```
//!
//! `preamble` 是裸文本，其余各节包一层同名标签，节与节之间用空行连接
//! （`pi-ai` 的 `getSystemMessageText()`）。`<project_context>` 段来自 cwd 到
//! 根的每一级 `AGENTS.md`（含全局 agent 目录），由 rpi 的
//! [`rpi_harness::context_files`] 发现。
//!
//! # 与 pi 的差异
//!
//! | 项 | pi | 这里 |
//! |---|---|---|
//! | `<docs>` 段 | 指向 pi 安装包里的 README/docs/examples | 省略：toiler 不带 pi 的文档树，写进去就是模型读不到的路径 |
//! | `<skills>` 段 | `buildSystemPromptSections` 写入 | 交给 rpi harness 在组装时追加（见 `AgentHarness::compose_prompt`）；toiler 目前不把技能装进 `AgentHarnessResources`，所以这一节通常为空 |
//! | `<project_context>` | 默认内联全文，由 pi-desktop 的 `digestProjectContext` 压成路径 | 直接生成「路径 + 读取指引」；没有 `read` 工具时退回全文（一条读不到的指引比多花 token 更糟） |
//! | 语言 | pi-desktop 用 `before_agent_start` 扩展整句替换 + 追加语言指令 | 组装时按 `Locale` 直接选中文/英文文案，并把语言指令追加在末尾 |

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rpi_harness::context_files::{load_project_context_files, ContextFile};
use rpi_tools::{ExecutionEnv, OsExecutionEnv};

/// 界面语言。取值与 pi-desktop 的 `UiLocale` 一致：只认 `zh-CN`，其余都算 `en`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    ZhCn,
    En,
}

impl Locale {
    /// `{"type":"set_ui_locale","locale":…}` 的值 / `GET /api/settings` 里的界面语言。
    pub fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("zh-CN") => Self::ZhCn,
            _ => Self::En,
        }
    }

    fn zh(self) -> bool {
        matches!(self, Self::ZhCn)
    }
}

/// 组装系统提示词所需的全部原料。
///
/// 之所以把原料留下来而不是只留成品字符串：切换界面语言要重算，而
/// `<project_context>` 段的形态（路径列表 / 全文）与语言都取决于原料。
pub struct PromptInputs {
    cwd: String,
    tool_names: Vec<String>,
    context_files: Vec<ContextFile>,
}

impl PromptInputs {
    /// 发现 `AGENTS.md` 一族项目指令文件。
    ///
    /// `agent_dir` 是 toiler 自己的 agent 目录（`~/.agents/toiler`），对应 pi 的
    /// `~/.pi/agent`：那里的 `AGENTS.md` 是用户级常驻指令，排在项目文件之前。
    /// 读不到就当作没有 —— 没有 AGENTS.md 的项目不该因此开不了会话。
    pub async fn load(cwd: &str, tool_names: &[String], agent_dir: &Path) -> Self {
        let env: Arc<dyn ExecutionEnv> = Arc::new(OsExecutionEnv::with_cwd(PathBuf::from(cwd)));
        let context_files = load_project_context_files(&env, Path::new(cwd), agent_dir).await;
        Self {
            cwd: cwd.to_string(),
            tool_names: tool_names.to_vec(),
            context_files,
        }
    }

    /// 当前启用的工具名（与 `<tools>` 段、harness 的 active 集合一致）。
    pub fn tool_names(&self) -> &[String] {
        &self.tool_names
    }

    /// 工具预设改变后替换启用集合（顺序即 `<tools>` 段的顺序）。
    pub fn set_tool_names(&mut self, names: Vec<String>) {
        self.tool_names = names;
    }

    /// 组装最终发给模型的文本（含末尾的语言指令）。
    pub fn compose(&self, locale: Locale) -> String {
        let mut sections: Vec<(&str, String)> = Vec::new();

        if let Some(tools) = self.tools_section(locale) {
            sections.push(("tools", tools));
        }
        if let Some(rules) = self.rules_section(locale) {
            sections.push(("rules", rules));
        }
        if let Some(context) = self.project_context_section(locale) {
            sections.push(("project_context", context));
        }
        sections.push(("cwd", self.cwd.replace('\\', "/")));

        let mut out = preamble(locale).to_string();
        for (name, body) in sections {
            out.push_str("\n\n");
            out.push('<');
            out.push_str(name);
            out.push_str(">\n");
            out.push_str(&body);
            out.push_str("\n</");
            out.push_str(name);
            out.push('>');
        }
        out.push_str("\n\n");
        out.push_str(language_instruction(locale));
        out
    }

    /// `<tools>`：每行 `- name: snippet`，末尾一句「还可能有别的工具」。
    ///
    /// 只列**当前会话真的启用**的工具 —— 与 pi 的 `declaredTools` 同义，
    /// 否则模型会去调一个 schema 里没有的工具。
    fn tools_section(&self, locale: Locale) -> Option<String> {
        let lines: Vec<String> = self
            .tool_names
            .iter()
            .filter_map(|name| TOOL_PROMPTS.iter().find(|tool| tool.name == name))
            .map(|tool| format!("- {}: {}", tool.name, tool.snippet(locale)))
            .collect();
        if lines.is_empty() {
            return None;
        }
        let tail = if locale.zh() {
            "除上述工具外，你还可能根据项目获得其他自定义工具。"
        } else {
            "In addition to the tools above, you may have access to other custom tools depending on the project."
        };
        Some(format!("{}\n\n{}", lines.join("\n"), tail))
    }

    /// `<rules>`：各工具自带的行为准则 + 两条通用准则。
    ///
    /// 去重顺序与 pi 的 `buildRules` 一致：条件规则 → 工具准则（按工具顺序）
    /// → 通用准则。`bash` 的「用 bash 做 ls/rg/find」只在没有 grep/find/ls
    /// 时才加，否则与专门的工具冲突。
    fn rules_section(&self, locale: Locale) -> Option<String> {
        let mut rules: Vec<&str> = Vec::new();
        let mut push = |rule: &'static str| {
            if !rules.contains(&rule) {
                rules.push(rule);
            }
        };

        let has = |name: &str| self.tool_names.iter().any(|tool| tool == name);
        if has("bash") && !has("grep") && !has("find") && !has("ls") {
            push(if locale.zh() {
                "使用 bash 执行 ls、rg、find 等文件操作"
            } else {
                "Use bash for file operations like ls, rg, find"
            });
        }
        for name in &self.tool_names {
            if let Some(tool) = TOOL_PROMPTS.iter().find(|tool| tool.name == name) {
                for (en, zh) in tool.guidelines {
                    push(if locale.zh() { zh } else { en });
                }
            }
        }
        push(if locale.zh() {
            "回答应简洁明了"
        } else {
            "Be concise in your responses"
        });
        push(if locale.zh() {
            "处理文件时清晰展示文件路径"
        } else {
            "Show file paths clearly when working with files"
        });

        Some(
            rules
                .into_iter()
                .map(|rule| format!("- {rule}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    /// `<project_context>`：有 `read` 时压成路径列表，否则内联全文。
    ///
    /// 压缩的收益是「模型没读的那几轮」省下整份 AGENTS.md；代价是压缩
    /// （compaction）丢掉更早的历史后，摘要会消失而内联版永远在 —— 所以摘要
    /// 正文明确要求模型「上下文被压缩过就重新读取」。
    fn project_context_section(&self, locale: Locale) -> Option<String> {
        if self.context_files.is_empty() {
            return None;
        }
        let can_read = self.tool_names.iter().any(|tool| tool == "read");
        if can_read {
            Some(self.digested_context(locale))
        } else {
            Some(self.inlined_context(locale))
        }
    }

    fn digested_context(&self, locale: Locale) -> String {
        let list = self
            .context_files
            .iter()
            .map(|file| format!("- {}", file.path.display()))
            .collect::<Vec<_>>()
            .join("\n");
        let cwd = &self.cwd;
        if locale.zh() {
            format!(
                "当前项目路径：{cwd}\n\
                 本项目的代码、命令与文件都在这个目录内，不要到其它目录里去找本项目的代码。\n\n\
                 本项目专属的说明与行为准则没有内联在这里，它们存放在以下文件：\n\n\
                 {list}\n\n\
                 这些文件里是本项目的强制约定（验证命令、禁止操作、架构约束），不是可选的背景资料。\n\
                 在开始任何代码修改或命令执行之前，先用 read 工具完整读取；如果上下文被压缩过，\n\
                 或者你发现自己的做法与约定可能冲突，重新读取对应文件再继续。"
            )
        } else {
            format!(
                "Current project path: {cwd}\n\
                 This project's code, commands and files live in that directory; do not go looking for this project's code elsewhere.\n\n\
                 This project's specific instructions and guidelines are not inlined here. They live in:\n\n\
                 {list}\n\n\
                 Those files carry binding conventions for this project (verification commands, forbidden\n\
                 operations, architecture constraints), not optional background. Read them in full with the\n\
                 read tool before changing code or running commands; re-read the relevant file if the\n\
                 conversation was compacted or your approach may conflict with the conventions."
            )
        }
    }

    fn inlined_context(&self, locale: Locale) -> String {
        let lead = if locale.zh() {
            "项目专属说明和行为准则："
        } else {
            "Project-specific instructions and guidelines:"
        };
        let mut out = String::from(lead);
        for file in &self.context_files {
            out.push_str("\n\n<project_instructions path=\"");
            out.push_str(&escape_attribute(&file.path.to_string_lossy()));
            out.push_str("\">\n");
            out.push_str(&file.content);
            out.push_str("\n</project_instructions>");
        }
        out
    }
}

/// 一个工具的提示词文案（英文 / 中文各一份，文案取自 pi 与 pi-desktop 的翻译表）。
struct ToolPrompt {
    name: &'static str,
    snippet_en: &'static str,
    snippet_zh: &'static str,
    /// `(英文, 中文)`
    guidelines: &'static [(&'static str, &'static str)],
}

impl ToolPrompt {
    fn snippet(&self, locale: Locale) -> &'static str {
        if locale.zh() {
            self.snippet_zh
        } else {
            self.snippet_en
        }
    }
}

/// 与 `crate::tools::build_tools` 启用的工具一一对应。
const TOOL_PROMPTS: &[ToolPrompt] = &[
    ToolPrompt {
        name: "read",
        snippet_en: "Read file contents",
        snippet_zh: "读取文件内容",
        guidelines: &[(
            "Use read to examine files instead of cat or sed.",
            "使用 read 查看文件，不要使用 cat 或 sed。",
        )],
    },
    ToolPrompt {
        name: "bash",
        snippet_en: "Execute bash commands (ls, grep, find, etc.)",
        snippet_zh: "执行 bash 命令（ls、grep、find 等）",
        // pi 这里还有一条「可以读 PI_* 环境变量看模型/会话详情」：toiler 只注入
        // `PI_WEB_DESKTOP_API_ORIGIN` / `PI_WEB_SHELL_PID` 两个桌面壳内部变量
        // （前端产物写死了 `__PI_WEB_*` 名字，不能改），没有模型/会话信息，
        // 写进提示词只会把模型引向不存在的变量。
        guidelines: &[],
    },
    ToolPrompt {
        name: "edit",
        snippet_en: "Make precise file edits with exact text replacement, including multiple disjoint edits in one call",
        snippet_zh: "通过精确替换文本编辑文件，并可在一次调用中完成多个不连续的修改",
        guidelines: &[
            (
                "Use edit for precise changes (edits[].oldText must match exactly)",
                "使用 edit 进行精确修改（edits[].oldText 必须完全匹配）",
            ),
            (
                "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls",
                "修改同一文件中的多个独立位置时，请在一次 edit 调用的 edits[] 中提供多个条目，而不是多次调用 edit",
            ),
            (
                "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.",
                "每个 edits[].oldText 都会针对原始文件匹配，而不是针对前面修改后的文件。不要提交相互重叠或嵌套的修改；相邻修改应合并为一次 edit。",
            ),
            (
                "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.",
                "在保证文件中唯一匹配的前提下，让 edits[].oldText 尽可能简短。不要加入大段未修改的内容。",
            ),
        ],
    },
    ToolPrompt {
        name: "write",
        snippet_en: "Create or overwrite files",
        snippet_zh: "创建或覆盖文件",
        guidelines: &[(
            "Use write only for new files or complete rewrites.",
            "仅在创建新文件或完整重写文件时使用 write。",
        )],
    },
    ToolPrompt {
        name: "grep",
        snippet_en: "Search file contents for patterns (respects .gitignore)",
        snippet_zh: "在文件内容中搜索模式（遵循 .gitignore）",
        guidelines: &[],
    },
    ToolPrompt {
        name: "find",
        snippet_en: "Find files by glob pattern (respects .gitignore)",
        snippet_zh: "按 glob 模式查找文件（遵循 .gitignore）",
        guidelines: &[],
    },
    ToolPrompt {
        name: "ls",
        snippet_en: "List directory contents",
        snippet_zh: "列出目录内容",
        guidelines: &[],
    },
    // ---- toiler 的三个桌面工具（实现见 `desktop_tools`） ----
    ToolPrompt {
        name: "system_time",
        snippet_en: "read the current host date, time, weekday, and time zone",
        snippet_zh: "读取当前主机的日期、时间、星期与时区",
        guidelines: &[(
            "Call system_time before resolving references such as today, tomorrow, yesterday, this week, or a relative deadline. Do not infer the current date from model knowledge.",
            "在解释今天、明天、昨天、本周或相对截止时间等日期前，先调用 system_time。不要根据模型自身知识推断当前日期。",
        )],
    },
    ToolPrompt {
        name: "task_progress",
        snippet_en: "create and update the Desktop task list with pending, in-progress, completed, failed, blocked, or skipped states",
        snippet_zh: "创建并更新 Desktop 任务列表，状态可为 pending、in_progress、completed、failed、blocked 或 skipped",
        guidelines: &[
            (
                "Use task_progress for substantial work with multiple distinct steps; do not create a task list for a simple single-step request.",
                "用于包含多个不同步骤的较大工作量；简单的一步请求不要建任务列表。",
            ),
            (
                "Before starting multi-step work, call task_progress with action=replace and a concise ordered list. Keep stable ids and include the complete list when replacing it.",
                "开始多步骤工作前，用 action=replace 传入一个简洁有序的列表。id 保持稳定，替换时给出完整列表。",
            ),
            (
                "Immediately before working on a task, update it to in_progress. Keep at most one task in_progress at a time.",
                "开始处理某个任务前，先把它更新为 in_progress。同一时刻最多只能有一个任务处于 in_progress。",
            ),
            (
                "Mark a task completed only after its required work and verification are finished. Mark failures as failed with the concrete reason, external dependencies as blocked, and intentionally omitted work as skipped.",
                "只有在所需工作和验证都完成后才标记 completed。失败标记为 failed 并写明具体原因，受外部依赖阻塞标记为 blocked，有意省略的工作标记为 skipped。",
            ),
            (
                "Update task_progress whenever execution state changes, not only in the final answer. Before finishing, ensure no task is incorrectly left in_progress. The Desktop automatically hides the task list after every task is completed or skipped.",
                "执行状态变化时就更新 task_progress，不要只在最终答复里才更新。结束前确认没有任务被错误地留在 in_progress。所有任务完成或跳过之后，Desktop 会自动隐藏任务列表。",
            ),
        ],
    },
    ToolPrompt {
        name: "ask_user",
        snippet_en: "ask the user for non-sensitive clarification, preferences, or confirmation",
        snippet_zh: "向用户询问非敏感的澄清、偏好或确认",
        guidelines: &[
            (
                "Use ask_user only when missing information would materially affect the result and cannot be discovered safely from the project.",
                "仅当缺少信息会实质影响结果、且无法从项目中安全查明时才使用 ask_user。",
            ),
            (
                "When you need multiple related answers, make one ask_user call and put every question in its questions array; the UI will present them one by one. Do not stop after the first question or ask the user to format tool arguments.",
                "需要多个相关答案时，只做一次 ask_user 调用，把所有问题放进 questions 数组；界面会逐个呈现。不要问完第一个问题就停下，也不要要求用户自行组织工具参数。",
            ),
            (
                "Each question must have a unique id, a clear prompt, and the correct kind. Use single_select with 2-8 concrete options for choices. For confirm, text, and editor, set options to an empty array. Use confirm for yes/no decisions, text for short values, and editor for multi-line input.",
                "每个问题都要有唯一的 id、清晰的 prompt 和正确的 kind。选择题用 single_select 并给出 2-8 个具体选项；confirm、text、editor 的 options 传空数组。是/否决定用 confirm，短值用 text，多行输入用 editor。",
            ),
            (
                "ask_user answers are visible to the model and may be saved in the session; never request passwords, tokens, private keys, passphrases, OTP codes, or other secrets. When Git or SSH needs credentials, run the normal command and let toiler show its secure credential prompt instead.",
                "ask_user 的答案对模型可见，且可能保存在会话中；绝不要索取密码、令牌、私钥、口令、验证码或其他机密。Git 或 SSH 需要凭据时，照常执行命令，由 toiler 弹出安全凭据输入框。",
            ),
            (
                "Ask at most three related questions at a time and make option tradeoffs explicit in each option description.",
                "一次最多问三个相关问题，并在每个选项的 description 里写清取舍。",
            ),
            (
                "Every single_select also offers a free-form answer, so an answer may be text you did not list; treat it as the user's choice even when it matches no option value.",
                "每个 single_select 都会额外提供自定义答案，所以返回值可能不在你列出的选项里；即使对不上任何选项值，也要把它当作用户的选择。",
            ),
        ],
    },
];

/// 开场白（文案取自 pi 的默认 preamble，只把框架名换成 toiler）。
fn preamble(locale: Locale) -> &'static str {
    if locale.zh() {
        "你是运行在 toiler 编程代理框架中的专业编程助手。你通过读取文件、执行命令、编辑代码和创建新文件来帮助用户。"
    } else {
        "You are an expert coding assistant operating inside toiler, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files."
    }
}

/// 界面语言指令，追加在整段提示词末尾（pi-desktop 的 `LANGUAGE_INSTRUCTION_*`）。
fn language_instruction(locale: Locale) -> &'static str {
    if locale.zh() {
        "输出语言规则（不可违背）：\n\
         你必须完全使用简体中文回复，这是 toiler 设置中用户选择的语言。所有自然语言的解释、提问、进度更新和最终答复都必须使用简体中文。不得因为用户消息、项目文件、工具输出或系统提示词使用其他语言而切换。只有在用户明确要求切换语言时，才可以使用其他语言。"
    } else {
        "OUTPUT LANGUAGE — NON-NEGOTIABLE:\n\
         You must reply entirely in English, the language selected in toiler settings. This applies to every natural-language explanation, question, status update, and final answer. Do not switch to another language because the user's message, project files, tool output, or system prompt uses it. Switch languages only when the user explicitly asks you to do so."
    }
}

/// XML 属性转义：路径里可能出现 `&`、`"`、`<`。
fn escape_attribute(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn standard_tools() -> Vec<String> {
        ["read", "bash", "edit", "write", "grep", "find", "ls"]
            .iter()
            .map(|name| name.to_string())
            .collect()
    }

    fn inputs(tool_names: Vec<String>, context_files: Vec<ContextFile>) -> PromptInputs {
        PromptInputs {
            cwd: "/tmp/demo".to_string(),
            tool_names,
            context_files,
        }
    }

    #[test]
    fn sections_follow_pi_order_and_wrap_non_preamble_parts() {
        let prompt = inputs(standard_tools(), Vec::new()).compose(Locale::En);
        assert!(prompt.starts_with("You are an expert coding assistant"));
        let tools = prompt.find("<tools>").expect("tools 段");
        let rules = prompt.find("<rules>").expect("rules 段");
        let cwd = prompt.find("<cwd>").expect("cwd 段");
        assert!(tools < rules && rules < cwd);
        assert!(prompt.contains("- read: Read file contents"));
        assert!(prompt.contains("- edit: Make precise file edits"));
        assert!(prompt.contains("<cwd>\n/tmp/demo\n</cwd>"));
        // 语言指令永远在最后。
        assert!(
            prompt.ends_with("Switch languages only when the user explicitly asks you to do so.")
        );
    }

    #[test]
    fn rules_drop_bash_file_ops_when_search_tools_exist() {
        let prompt = inputs(standard_tools(), Vec::new()).compose(Locale::En);
        assert!(!prompt.contains("Use bash for file operations"));
        let minimal = inputs(vec!["read".into(), "bash".into()], Vec::new()).compose(Locale::En);
        assert!(minimal.contains("- Use bash for file operations like ls, rg, find"));
    }

    #[test]
    fn context_files_are_digested_only_when_read_is_available() {
        let files = vec![ContextFile {
            path: PathBuf::from("/tmp/demo/AGENTS.md"),
            content: "# 约定\n不要用 cat".to_string(),
        }];
        let with_read = inputs(standard_tools(), files.clone()).compose(Locale::ZhCn);
        assert!(with_read.contains("当前项目路径：/tmp/demo"));
        assert!(with_read.contains("- /tmp/demo/AGENTS.md"));
        assert!(!with_read.contains("不要用 cat"), "有 read 时应压成路径");

        let without_read = inputs(vec!["bash".into()], files).compose(Locale::ZhCn);
        assert!(without_read.contains("<project_instructions path=\"/tmp/demo/AGENTS.md\">"));
        assert!(
            without_read.contains("不要用 cat"),
            "没有 read 时必须内联全文"
        );
    }

    #[test]
    fn no_context_files_means_no_project_context_section() {
        let prompt = inputs(standard_tools(), Vec::new()).compose(Locale::ZhCn);
        assert!(!prompt.contains("<project_context>"));
        assert!(prompt.starts_with("你是运行在 toiler 编程代理框架中的专业编程助手"));
    }

    #[test]
    fn locale_parse_only_accepts_zh_cn() {
        assert_eq!(Locale::parse(Some("zh-CN")), Locale::ZhCn);
        assert_eq!(Locale::parse(Some("zh")), Locale::En);
        assert_eq!(Locale::parse(None), Locale::En);
    }
}
