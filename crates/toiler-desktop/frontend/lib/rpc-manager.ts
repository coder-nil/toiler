import type { StreamFn, ThinkingLevel } from "@earendil-works/pi-agent-core";
import type { FetchFunction } from "@earendil-works/pi-ai";
import { createAgentSessionFromServices, createAgentSessionServices, createLocalPowerShellOperations, getAgentDir, initTheme, SessionManager, SettingsManager, Theme } from "@earendil-works/pi-coding-agent";
import { KeybindingsManager as TuiKeybindingsManager, TUI_KEYBINDINGS } from "@earendil-works/pi-tui";
import { randomUUID } from "crypto";
import { existsSync, realpathSync, writeFileSync } from "fs";
import { resolve } from "path";
import { validateAgentImages } from "./image-attachments";
import { createDesktopModelRuntime, refreshDesktopProviderCatalogs } from "./desktop-providers";
import { invalidateModelsCache } from "./models-cache";
import { resolveVisibleModels, selectInitialModelScope } from "./model-scope";
import {
  createProjectCommandBashExtension,
  createProjectCommandBashOperations,
  preferUserBashExtension,
} from "./project-command-env";
import { createSystemTimeExtension } from "./system-time-tool";
import { createAskUserExtension } from "./ask-user-tool";
import { createTaskProgressExtension } from "./task-progress-tool";
import { cacheSessionPath, invalidateSessionListCache } from "./session-reader";
import { applyShellTool, normalizeActiveShellTool, resolveShellSelection } from "./shell-tool";
import { ensureHostToolPath } from "./host-tool-path";
import type { QueueMode } from "./queue-mode-settings";
import { getProjectTrustStatus, projectTrustReloadOptions } from "./project-trust";
import { persistExplicitStartupPreferences } from "./startup-preferences";
import { findSkillInvocation, recordSkillUsage } from "./skills-store";
import type { InlineExtension, SlashCommandInfo } from "@earendil-works/pi-coding-agent";
import type { AgentSessionLike, ExtensionUiContextLike, ToolInfo } from "./pi-types";
import type {
  ExtensionUiRequest,
  ExtensionUiResponse,
  ExtensionWidgetItem,
  SessionInfo,
  SessionMessageEntry,
} from "./types";
import { createHeadlessCustomUiTui, DEFAULT_CUSTOM_UI_COLUMNS, type HeadlessCustomUiTui } from "./custom-ui-terminal";
import { diagnosticErrorMessage, logAgentDiagnostic } from "./agent-diagnostics";
import { buildChatModeSystemPrompt } from "./chat-mode-prompt";
import { digestProjectContext } from "./project-context-digest";
import { LANGUAGE_INSTRUCTION_EN, LANGUAGE_INSTRUCTION_ZH, languageInstruction, type UiLocale } from "./language-instruction";

// ============================================================================
// Types
// ============================================================================

export interface AgentEvent {
  type: string;
  [key: string]: unknown;
}

type EventListener = (event: AgentEvent) => void;

type PendingUiResponse = {
  resolve: (response: ExtensionUiResponse) => void;
  cancel: () => void;
};

type CustomUiComponent = {
  render: (width: number) => string[];
  handleInput?: (data: string) => void;
  dispose?: () => void;
  invalidate?: () => void;
};

type ExtensionWidgetComponent = {
  render: (width: number) => unknown;
  dispose?: () => void;
};

type ExtensionWidgetFactory = (tui: HeadlessCustomUiTui, theme: Theme) => unknown;

type ActiveExtensionWidget = {
  key: string;
  component: ExtensionWidgetComponent;
  placement: "aboveEditor" | "belowEditor";
  generation: number;
  clearEmitted: boolean;
  rendered: boolean;
};

type ActiveCustomUi = {
  component: CustomUiComponent;
  width: number;
  resolve: (value: unknown) => void;
  settled: boolean;
};

type ExtensionUiRequestBody = Record<string, unknown> & {
  method: ExtensionUiRequest["method"];
  timeout?: number;
  expiresAt?: number;
};

type ExtensionCommandContextActionsLike = {
  waitForIdle: () => Promise<void>;
  newSession: () => Promise<{ cancelled: boolean }>;
  fork: () => Promise<{ cancelled: boolean }>;
  navigateTree: (targetId: string, options?: { summarize?: boolean }) => Promise<{ cancelled: boolean }>;
  switchSession: () => Promise<{ cancelled: boolean }>;
  reload: () => Promise<void>;
};

type ExtensionBindingOptions = {
  forceEmptySystemPrompt?: boolean;
};

type ProviderRetrySettingsManager = {
  applyOverrides: (overrides: {
    retry: { enabled?: boolean; provider: { maxRetries: number } };
  }) => void;
};

type FetchRetryConfigurableAgent = {
  streamFunction: StreamFn;
};

const DESKTOP_PROVIDER_REQUEST_MAX_RETRIES = 5;
// 停止一个「还在准备」的 prompt：轮询式重发 abort 的间隔与上限（约 20 秒）。
const ABORT_PREPARATION_POLL_MS = 100;
const ABORT_PREPARATION_ATTEMPTS = 200;
const configuredProviderRetryAgents = new WeakSet<object>();

function isAbortError(error: unknown): boolean {
  return error instanceof Error && error.name === "AbortError";
}

function waitForProviderRetry(delayMs: number, signal: AbortSignal | null): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(new DOMException("Request aborted", "AbortError"));
      return;
    }
    const timer = setTimeout(resolve, delayMs);
    signal?.addEventListener("abort", () => {
      clearTimeout(timer);
      reject(new DOMException("Request aborted", "AbortError"));
    }, { once: true });
  });
}

function withProviderRequestRetry(fetchImpl: FetchFunction): FetchFunction {
  return async (input, init) => {
    const signal = init?.signal ?? (input instanceof Request ? input.signal : null);
    for (let retry = 0; ; retry++) {
      if (signal?.aborted) throw new DOMException("Request aborted", "AbortError");
      try {
        // A Request body is one-shot, so every retry must use a fresh clone.
        const response = await fetchImpl(input instanceof Request ? input.clone() : input, init);
        if (response.ok || retry >= DESKTOP_PROVIDER_REQUEST_MAX_RETRIES) return response;
        await response.body?.cancel().catch(() => {});
      } catch (error) {
        if (signal?.aborted || isAbortError(error) || retry >= DESKTOP_PROVIDER_REQUEST_MAX_RETRIES) throw error;
      }
      await waitForProviderRetry(500 * 2 ** retry, signal);
    }
  };
}

function configureDesktopProviderRetry(session: AgentSessionLike): void {
  const settingsManager = session.settingsManager as unknown as ProviderRetrySettingsManager;
  settingsManager.applyOverrides({
    // HTTP retries cover failures before headers. Keep the agent retry policy
    // intact as it is also needed for streams that fail after headers arrive.
    retry: {
      provider: { maxRetries: 0 },
    },
  });

  const agent = session.agent as unknown as FetchRetryConfigurableAgent;
  if (configuredProviderRetryAgents.has(agent)) return;
  const originalStreamFunction = agent.streamFunction;
  agent.streamFunction = (model, context, options) => originalStreamFunction(model, context, {
    ...options,
    fetch: withProviderRequestRetry(options?.fetch ?? globalThis.fetch),
  });
  configuredProviderRetryAgents.add(agent);
}

const RUNNING_STATE_EVENT_TYPES = new Set([
  "agent_start",
  "agent_end",
  "agent_settled",
  "auto_compaction_start",
  "auto_compaction_end",
  "compaction_start",
  "compaction_end",
]);

const IDLE_RESET_EVENT_TYPES = new Set([
  "agent_end",
  "agent_settled",
  "auto_compaction_end",
  "compaction_end",
]);

export interface RpcSessionStartOptions {
  toolNames?: string[];
  initialModel?: { provider: string; modelId: string };
  thinkingLevel?: ThinkingLevel;
  uiLocale?: "en" | "zh-CN";
  /** 普通对话模式：整段替换系统提示词，并且不使用任何工具。 */
  chatMode?: boolean;
}

const SYSTEM_PROMPT_TRANSLATIONS: Record<string, string> = {
  "You are an expert coding assistant operating inside pi, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.": "你是运行在 pi 编程代理框架中的专业编程助手。你通过读取文件、执行命令、编辑代码和创建新文件来帮助用户。",
  "You are a coding assistant running inside pi, a coding agent harness. Help users inspect files, run commands, and modify code.": "你是运行在 pi 编程代理框架中的编程助手。你帮助用户检查文件、执行命令并修改代码。",
  "Available tools:": "可用工具：",
  "In addition to the tools above, you may have access to other custom tools depending on the project.": "除上述工具外，你还可能根据项目获得其他自定义工具。",
  "Guidelines:": "行为准则：",
  "Be concise in your responses": "回答应简洁明了",
  "Show file paths clearly when working with files": "处理文件时清晰展示文件路径",
  "Use bash for file operations like ls, rg, find": "使用 bash 执行 ls、rg、find 等文件操作",
  "Use bash or PowerShell for file operations like listing, searching, and finding files": "使用 bash 或 PowerShell 执行列目录、搜索和查找文件等操作",
  "Use PowerShell for file operations like listing, searching, and finding files": "使用 PowerShell 执行列目录、搜索和查找文件等操作",
  "Pi documentation (read only when the user asks about the harness itself, its SDK, extensions, themes, skills, or TUI):": "Pi 文档（仅当用户询问 pi 框架本身、其 SDK、扩展、主题、技能或 TUI 时阅读）：",
  "- Main documentation:": "- 主文档：",
  "- Additional docs:": "- 补充文档：",
  "- Examples:": "- 示例：",
  "(extensions, custom tools, SDK)": "（扩展、自定义工具、SDK）",
  "- When asked about:": "- 当用户询问以下主题时：",
  "When reading pi docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory": "阅读 pi 文档或示例时，应从补充文档中的 docs/... 和示例中的 examples/... 解析路径，而不是当前工作目录",
  "Before implementing harness changes, read the relevant docs and examples in full, following related cross-references.": "在修改 pi 框架本身之前，先完整阅读相关文档与示例，并顺着其中的交叉引用继续读。",
  "extensions (docs/extensions.md, examples/extensions/), themes (docs/themes.md), skills (docs/skills.md), prompt templates (docs/prompt-templates.md), TUI components (docs/tui.md), keybindings (docs/keybindings.md), SDK integrations (docs/sdk.md), custom providers (docs/custom-provider.md), adding models (docs/models.md), packages (docs/packages.md), environment variables (docs/environment-variables.md), MCP servers (docs/mcp.md), codemode scripts and non-LLM models such as classifiers and image models (docs/codemode.md)": "扩展（docs/extensions.md、examples/extensions/）、主题（docs/themes.md）、技能（docs/skills.md）、提示词模板（docs/prompt-templates.md）、TUI 组件（docs/tui.md）、快捷键（docs/keybindings.md）、SDK 集成（docs/sdk.md）、自定义提供商（docs/custom-provider.md）、添加模型（docs/models.md）、Pi 包（docs/packages.md）、环境变量（docs/environment-variables.md）、MCP 服务器（docs/mcp.md）、codemode 脚本与非 LLM 模型（如分类器、图像模型）（docs/codemode.md）",
  "When working on pi topics, read the docs and examples, and follow .md cross-references before implementing": "处理 pi 相关主题时，先完整阅读文档和示例，并在实现前遵循 .md 文件中的交叉引用",
  "Always read pi .md files completely and follow links to related docs (e.g., tui.md for TUI API details)": "始终完整阅读 pi 的 .md 文件，并遵循指向相关文档的链接（例如 TUI API 详情对应 tui.md）",
  "Project-specific instructions and guidelines:": "项目专属说明和行为准则：",
  "Current working directory:": "当前工作目录：",
  "The following skills provide specialized instructions for specific tasks.": "以下技能为特定任务提供专门说明。",
  "Use the read tool to load a skill's file when the task matches its description.": "当任务与某技能的描述相符时，使用 read 工具加载该技能的文件。",
  "Resolve relative paths against the skill's directory (parent of SKILL.md). Use absolute paths in tool calls.": "相对路径以技能目录（SKILL.md 的父目录）为基准解析。工具调用里请用绝对路径。",
  "Use bash to load a skill's file when the task matches its description.": "当任务与某技能的描述相符时，使用 bash 加载该技能的文件。",
  "Load a skill's file when the task matches its description.": "当任务与某技能的描述相符时，加载该技能的文件。",
  "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.": "当技能文件引用相对路径时，请相对技能目录（SKILL.md 所在目录，或该路径的 dirname）解析，并在工具命令中使用该绝对路径。",
  "Read file contents": "读取文件内容",
  "Execute bash commands (ls, grep, find, etc.)": "执行 bash 命令（ls、grep、find 等）",
  "Search file contents for patterns (respects .gitignore)": "在文件内容中搜索模式（遵循 .gitignore）",
  "Find files by glob pattern (respects .gitignore)": "按 glob 模式查找文件（遵循 .gitignore）",
  "List directory contents": "列出目录内容",
  "Execute PowerShell commands": "执行 PowerShell 命令",
  "Make precise file edits with exact text replacement, including multiple disjoint edits in one call": "通过精确替换文本编辑文件，并可在一次调用中完成多个不连续的修改",
  "Create or overwrite files": "创建或覆盖文件",
  "Use read to examine files instead of cat or sed.": "使用 read 查看文件，不要使用 cat 或 sed。",
  "Use edit for precise changes (edits[].oldText must match exactly)": "使用 edit 进行精确修改（edits[].oldText 必须完全匹配）",
  "Use edit for targeted changes. Batch changes to one file in a single call.": "使用 edit 进行定向修改；对同一个文件的批量改动放在一次调用里。",
  "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls": "修改同一文件中的多个独立位置时，请在一次 edit 调用的 edits[] 中提供多个条目，而不是多次调用 edit",
  "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.": "每个 edits[].oldText 都会针对原始文件匹配，而不是针对前面修改后的文件。不要提交相互重叠或嵌套的修改；相邻修改应合并为一次 edit。",
  "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.": "在保证文件中唯一匹配的前提下，让 edits[].oldText 尽可能简短。不要加入大段未修改的内容。",
  "Use write only for new files or complete rewrites.": "仅在创建新文件或完整重写文件时使用 write。",
  "You can inspect PI_* environment variables for current model and session details.": "你可以查看 PI_* 环境变量来获取当前模型和会话详情。",
  "Call system_time before resolving references such as today, tomorrow, yesterday, this week, or a relative deadline. Do not infer the current date from model knowledge.": "在解释今天、明天、昨天、本周或相对截止时间等日期前，先调用 system_time。不要根据模型自身知识推断当前日期。",
  // 桌面自带工具：提示词里的 snippet / guideline 是固定英文文案，同样靠这条替换中文化。
  "read the current host date, time, weekday, and time zone": "读取当前主机的日期、时间、星期与时区",
  "ask the user for non-sensitive clarification, preferences, or confirmation": "向用户询问非敏感的澄清、偏好或确认",
  "Use ask_user only when missing information would materially affect the result and cannot be discovered safely from the project.": "仅当缺少信息会实质影响结果、且无法从项目中安全查明时才使用 ask_user。",
  "When you need multiple related answers, make one ask_user call and put every question in its questions array; the UI will present them one by one. Do not stop after the first question or ask the user to format tool arguments.": "需要多个相关答案时，只做一次 ask_user 调用，把所有问题放进 questions 数组；界面会逐个呈现。不要问完第一个问题就停下，也不要要求用户自行组织工具参数。",
  "Each question must have a unique id, a clear prompt, and the correct kind. Use single_select with 2-8 concrete options for choices. For confirm, text, and editor, set options to an empty array. Use confirm for yes/no decisions, text for short values, and editor for multi-line input.": "每个问题都要有唯一的 id、清晰的 prompt 和正确的 kind。选择题用 single_select 并给出 2-8 个具体选项；confirm、text、editor 的 options 传空数组。是/否决定用 confirm，短值用 text，多行输入用 editor。",
  "ask_user answers are visible to the model and may be saved in the session; never request passwords, tokens, private keys, passphrases, OTP codes, or other secrets. When Git or SSH needs credentials, run the normal command and let Pi Desktop show its secure credential prompt instead.": "ask_user 的答案对模型可见，且可能保存在会话中；绝不要索取密码、令牌、私钥、口令、验证码或其他机密。Git 或 SSH 需要凭据时，照常执行命令，由 Pi Desktop 弹出安全凭据输入框。",
  "Ask at most three related questions at a time and make option tradeoffs explicit in each option description.": "一次最多问三个相关问题，并在每个选项的 description 里写清取舍。",
  "Every single_select also offers a free-form answer, so an answer may be text you did not list; treat it as the user's choice even when it matches no option value.": "每个 single_select 都会额外提供自定义答案，所以返回值可能不在你列出的选项里；即使对不上任何选项值，也要把它当作用户的选择。",
  "create and update the Desktop task list with pending, in-progress, completed, failed, blocked, or skipped states": "创建并更新 Desktop 任务列表，状态可为 pending、in_progress、completed、failed、blocked 或 skipped",
  "Use task_progress for substantial work with multiple distinct steps; do not create a task list for a simple single-step request.": "用于包含多个不同步骤的较大工作量；简单的一步请求不要建任务列表。",
  "Before starting multi-step work, call task_progress with action=replace and a concise ordered list. Keep stable ids and include the complete list when replacing it.": "开始多步骤工作前，用 action=replace 传入一个简洁有序的列表。id 保持稳定，替换时给出完整列表。",
  "Immediately before working on a task, update it to in_progress. Keep at most one task in_progress at a time.": "开始处理某个任务前，先把它更新为 in_progress。同一时刻最多只能有一个任务处于 in_progress。",
  "Mark a task completed only after its required work and verification are finished. Mark failures as failed with the concrete reason, external dependencies as blocked, and intentionally omitted work as skipped.": "只有在所需工作和验证都完成后才标记 completed。失败标记为 failed 并写明具体原因，受外部依赖阻塞标记为 blocked，有意省略的工作标记为 skipped。",
  "Update task_progress whenever execution state changes, not only in the final answer. Before finishing, ensure no task is incorrectly left in_progress. The Desktop automatically hides the task list after every task is completed or skipped.": "执行状态变化时就更新 task_progress，不要只在最终答复里才更新。结束前确认没有任务被错误地留在 in_progress。所有任务完成或跳过之后，Desktop 会自动隐藏任务列表。",
  "When Git, SSH, or another command requests a username, password, passphrase, token, or verification code, run the normal command and wait for Pi Desktop's secure credential prompt. Never ask the user to paste secrets into chat or pass secrets through ask_user.": "当 Git、SSH 或其他命令要求输入用户名、密码、口令、令牌或验证码时，照常执行该命令，等待 Pi Desktop 的安全凭据输入框。绝不要求用户把机密粘贴到对话里，也不通过 ask_user 传递机密。",
};

function normalizeUiLocale(value: unknown): UiLocale {
  return value === "zh-CN" ? "zh-CN" : "en";
}

/** 队列投递模式：`one-at-a-time` 每个完成的 assistant turn 只投一条，`all` 一次投完。 */
function parseQueueMode(value: unknown): QueueMode {
  if (value === "all" || value === "one-at-a-time") return value;
  throw new Error('mode must be "all" or "one-at-a-time"');
}

function translateGeneratedSystemPrompt(prompt: string, locale: UiLocale): string {
  let translated = prompt;
  const entries = Object.entries(SYSTEM_PROMPT_TRANSLATIONS);
  if (locale === "en") {
    for (const [english, chinese] of entries) translated = translated.split(chinese).join(english);
  } else {
    for (const [english, chinese] of entries) translated = translated.split(english).join(chinese);
  }
  return translated;
}

/**
 * 普通对话模式与界面语言的共享状态。
 * 内联扩展在每次运行（`before_agent_start`）时读取它，所以切换模式或语言不需要重建会话。
 * `chatMode` 优先于 `forceEmpty`：两种模式都不用工具，但普通对话仍需一段提示词。
 */
type SessionPromptState = { value: UiLocale; forceEmpty: boolean; chatMode: boolean };

function applyLanguageInstruction(prompt: string, locale: UiLocale): string {
  const withoutPreviousInstruction = prompt
    .replace(`\n\n${LANGUAGE_INSTRUCTION_EN}`, "")
    .replace(`\n\n${LANGUAGE_INSTRUCTION_ZH}`, "")
    .trimEnd();
  const instruction = languageInstruction(locale);
  return withoutPreviousInstruction ? `${withoutPreviousInstruction}\n\n${instruction}` : instruction;
}

/**
 * 请求里是否真的有 `read` 工具。没有它时不能把 AGENTS.md 换成路径——那等于给模型
 * 一条读不到的指引，比多花 token 更糟（工具预设 `off` 与普通对话模式都会落到这里）。
 */
function hasReadTool(toolNames: readonly string[] | undefined): boolean {
  return (toolNames ?? []).includes("read");
}

/**
 * 生成的提示词 → 实际请求的提示词。
 *
 * 顺序有意义：先中文化（整句替换，不能作用在 AGENTS.md 原文上），再把
 * `<project_context>` 压成路径（那份原文是本项目 90KB 的 AGENTS.md，每轮都发）。
 */
function effectivePromptText(prompt: string, locale: UiLocale, canRead: boolean): string {
  return applyLanguageInstruction(
    digestProjectContext(translateGeneratedSystemPrompt(prompt, locale), locale, canRead),
    locale,
  );
}

function createLanguagePromptExtension(promptState: SessionPromptState): InlineExtension {
  return {
    name: "pi-desktop-language-prompt",
    hidden: true,
    factory: (pi) => {
      pi.on("before_agent_start", (event) => ({
        systemPrompt: promptState.chatMode
          ? buildChatModeSystemPrompt(promptState.value)
          : promptState.forceEmpty
            ? ""
            : effectivePromptText(
                event.systemPrompt,
                promptState.value,
                hasReadTool(event.systemPromptOptions.selectedTools),
              ),
      }));
    },
  };
}

const CODING_TOOL_NAMES = ["read", "bash", "powershell", "edit", "write", "grep", "find", "ls"];

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function assistantUsageSummary(value: unknown): Record<string, number> | undefined {
  if (!isRecord(value)) return undefined;
  const keys = ["input", "output", "cacheRead", "cacheWrite", "totalTokens"];
  const usage: Record<string, number> = {};
  for (const key of keys) {
    if (typeof value[key] === "number") usage[key] = value[key];
  }
  return Object.keys(usage).length > 0 ? usage : undefined;
}

function assistantContentSummary(value: unknown): { contentTypes: string[]; toolNames: string[] } {
  if (!Array.isArray(value)) return { contentTypes: [], toolNames: [] };
  const contentTypes = new Set<string>();
  const toolNames = new Set<string>();
  for (const block of value) {
    if (!isRecord(block)) continue;
    if (typeof block.type === "string") contentTypes.add(block.type);
    if (block.type === "toolCall") {
      const name = typeof block.name === "string"
        ? block.name
        : (typeof block.toolName === "string" ? block.toolName : undefined);
      if (name) toolNames.add(name);
    }
  }
  return { contentTypes: [...contentTypes], toolNames: [...toolNames] };
}

// Extensions require a complete Theme, while the web UI applies its own styling.
class PlainTextTheme extends Theme {
  constructor() {
    super(
      // Theme derives scrollbar colors from muted/text starting in Pi 0.85.
      { text: "", muted: "", thinkingXhigh: "", searchMatchText: "" } as ConstructorParameters<typeof Theme>[0],
      { selectedBg: "" } as ConstructorParameters<typeof Theme>[1],
      "truecolor",
    );
  }

  override fg(...[, text]: Parameters<Theme["fg"]>): string { return text; }
  override bg(...[, text]: Parameters<Theme["bg"]>): string { return text; }
  override bold(text: string): string { return text; }
  override italic(text: string): string { return text; }
  override underline(text: string): string { return text; }
  override inverse(text: string): string { return text; }
  override strikethrough(text: string): string { return text; }
  override getFgAnsi(): string { return ""; }
  override getBgAnsi(): string { return ""; }
  override getThinkingBorderColor(): (text: string) => string {
    return (text) => text;
  }
  override getBashModeBorderColor(): (text: string) => string { return (text) => text; }
}

const PLAIN_TEXT_THEME = new PlainTextTheme();
const CUSTOM_UI_KEYBINDINGS = new TuiKeybindingsManager(TUI_KEYBINDINGS);

function withExtensionTools(session: AgentSessionLike, toolNames: string[]): string[] {
  if (toolNames.length === 0) return [];

  const codingToolNames = new Set(CODING_TOOL_NAMES);
  const extensionToolNames = session
    .getAllTools()
    .map((t) => t.name)
    .filter((name) => !codingToolNames.has(name));

  return [...new Set([...toolNames, ...extensionToolNames])];
}

// ============================================================================
// AgentSessionWrapper
// Wraps AgentSession with the same interface the rest of the app expects
// ============================================================================

export class AgentSessionWrapper {
  private listeners: EventListener[] = [];
  private pendingUiResponses = new Map<string, PendingUiResponse>();
  private pendingUiRequests = new Map<string, AgentEvent>();
  private activeCustomUis = new Map<string, ActiveCustomUi>();
  private extensionStatuses = new Map<string, string>();
  private extensionWidgets = new Map<string, ExtensionWidgetItem>();
  private activeExtensionWidgets = new Map<string, ActiveExtensionWidget>();
  private extensionWidgetGenerations = new Map<string, number>();
  private extensionWidgetsResetting = false;
  private pendingPromptCount = 0;
  private promptAdmissionTail: Promise<void> = Promise.resolve();
  private extensionsBound = false;
  private extensionBindingPromise: Promise<void> | null = null;
  private extensionBindingError: unknown = null;
  private forceEmptySystemPrompt = false;
  private chatMode = false;
  /** 进入普通对话模式前的工具集，切回工作模式时恢复。 */
  private toolsBeforeChatMode: string[] | null = null;
  private uiLocale: UiLocale = "en";
  private unsubscribe: (() => void) | null = null;
  private idleTimer: ReturnType<typeof setTimeout> | null = null;
  private onDestroyCallback: (() => void) | null = null;
  private shutdownPromise: Promise<void> | null = null;
  private _alive = true;

  constructor(
    public readonly inner: AgentSessionLike,
    private readonly onUiLocaleChange?: (locale: UiLocale) => void,
    private readonly onForceEmptyChange?: (force: boolean) => void,
    private readonly onChatModeChange?: (on: boolean) => void,
  ) {}

  /**
   * The prompt a run actually sends.
   *
   * Pi >= 0.86 replays `agent.state.systemPrompt` from the transcript's leading
   * system message and exposes it as a getter-only property, so the effective prompt
   * can no longer be written back into state — assigning it throws. The language
   * instruction reaches the model through `createLanguagePromptExtension`
   * (`before_agent_start`); this accessor exists so `get_state` and the System Prompt
   * panel report that same text instead of a state value nothing updates.
   */
  private effectiveSystemPrompt(): string {
    // 普通对话模式优先：它虽然同样不使用任何工具，但需要一整段闲聊提示词。
    if (this.chatMode) return buildChatModeSystemPrompt(this.uiLocale);
    if (this.forceEmptySystemPrompt) return "";
    // 与 `createLanguagePromptExtension` 走同一个函数，面板显示必须等于实际请求。
    const activeTools = this.inner.agent?.state?.tools?.map((tool: { name: string }) => tool.name);
    return effectivePromptText(
      this.inner.agent.state?.systemPrompt ?? "",
      this.uiLocale,
      hasReadTool(activeTools),
    );
  }

  setUiLocale(locale: unknown): void {
    this.uiLocale = normalizeUiLocale(locale);
    this.onUiLocaleChange?.(this.uiLocale);
  }

  get sessionId(): string {
    return this.inner.sessionId;
  }

  get sessionFile(): string {
    return this.inner.sessionFile ?? "";
  }

  get cwd(): string {
    return this.inner.sessionManager.getCwd();
  }

  get streamingMessage() {
    return this.inner.agent.state?.streamingMessage;
  }

  get isStreaming(): boolean {
    return this.inner.isStreaming;
  }

  isAlive(): boolean {
    return this._alive;
  }

  isRunning(): boolean {
    return this._alive && (this.pendingPromptCount > 0 || this.inner.isStreaming || this.inner.isCompacting || this.inner.isBashRunning);
  }

  hasPendingUiRequests(): boolean {
    return this._alive && this.pendingUiRequests.size > 0;
  }

  start(): void {
    this.unsubscribe = this.inner.subscribe((event: AgentEvent) => {
      this.logEventDiagnostic(event);
      if (event.type === "agent_end") {
        invalidateSessionListCache();
      }
      if (IDLE_RESET_EVENT_TYPES.has(event.type)) this.resetIdleTimer();
      this.emit(event);
      if (
        RUNNING_STATE_EVENT_TYPES.has(event.type)
        || event.type === "extension_ui_request"
        || event.type === "extension_ui_resolved"
      ) {
        notifyRunningChange();
      }
    });
    this.resetIdleTimer();
    notifyRunningChange();
  }

  setForceEmptySystemPrompt(force: boolean): void {
    this.forceEmptySystemPrompt = force;
    // The inline language extension reads this flag on every run; nothing is written
    // into agent state, which pi >= 0.86 makes getter-only.
    this.onForceEmptyChange?.(force);
  }

  /**
   * 切换普通对话模式（输入框的 π 图标）。
   * 只改提示词来源与工具集，不重建 AgentSession：提示词通过内联扩展在每次运行时重算。
   * `toolsBeforeChatMode` 只在以普通对话模式创建会话时传入，
   * 因为后续的 `set_chat_mode` 自己会捕获快照。
   */
  setChatMode(on: boolean, toolsBeforeChatMode?: string[]): void {
    if (on && toolsBeforeChatMode) this.toolsBeforeChatMode = toolsBeforeChatMode;
    this.chatMode = on;
    this.onChatModeChange?.(on);
  }

  beginExtensionBinding(options: ExtensionBindingOptions = {}): void {
    void this.ensureExtensionsBound(options).catch((err) => {
      this.logDiagnostic("error", "extension_binding_failed", {
        errorMessage: diagnosticErrorMessage(err),
      });
    });
  }

  async waitUntilReady(): Promise<void> {
    await this.waitForExtensionsBound();
  }

  private ensureExtensionsBound(options: ExtensionBindingOptions = {}): Promise<void> {
    if (options.forceEmptySystemPrompt) this.forceEmptySystemPrompt = true;
    if (this.extensionsBound) return Promise.resolve();
    if (this.extensionBindingPromise) return this.extensionBindingPromise;

    this.extensionBindingError = null;
    this.extensionBindingPromise = (async () => {
      if (!this._alive) return;
      const uiContext = this.createExtensionUiContext();
      if (typeof this.inner.bindExtensions === "function") {
        const bindExtensions = this.inner.bindExtensions as (bindings: {
          uiContext?: ExtensionUiContextLike;
          mode?: "rpc";
          commandContextActions?: ExtensionCommandContextActionsLike;
          shutdownHandler?: () => void;
          onError?: (error: { extensionPath: string; event: string; error: string }) => void;
        }) => Promise<void>;
        await bindExtensions.call(this.inner, {
          uiContext,
          mode: "rpc",
          commandContextActions: this.createExtensionCommandContextActions(),
          shutdownHandler: () => this.emit({
            type: "extension_ui_request",
            id: randomUUID(),
            method: "notify",
            notifyType: "warning",
            message: "Extension requested shutdown, but shutdown is not supported in Pi Desktop.",
          } as ExtensionUiRequest as AgentEvent),
          onError: (error) => {
            this.logDiagnostic("error", "extension_failed", {
              extensionPath: error.extensionPath,
              extensionEvent: error.event,
              errorMessage: error.error,
            });
            this.emit({
              type: "extension_error",
              extensionPath: error.extensionPath,
              event: error.event,
              error: error.error,
            });
          },
        });
      } else {
        this.inner.extensionRunner.setUIContext?.(uiContext, "rpc");
      }
      // 扩展可能在绑定时激活自己的工具；普通对话模式必须保持空工具集。
      if (this.chatMode) this.inner.setActiveToolsByName([]);
      this.extensionsBound = true;
      console.log(`[pi-desktop] session_start dispatched to extensions for session ${this.inner.sessionId}`);
    })().catch((err) => {
      this.extensionBindingError = err;
      throw err;
    });

    return this.extensionBindingPromise;
  }

  private async waitForExtensionsBound(): Promise<void> {
    try {
      if (this.extensionBindingPromise) await this.extensionBindingPromise;
    } catch (err) {
      throw err instanceof Error ? err : new Error(String(err));
    }
    if (this.extensionBindingError) {
      throw this.extensionBindingError instanceof Error
        ? this.extensionBindingError
        : new Error(String(this.extensionBindingError));
    }
  }

  private shouldWaitForExtensions(type: string): boolean {
    return type === "prompt"
      || type === "steer"
      || type === "follow_up"
      || type === "get_commands"
      || type === "get_state";
  }

  private async withFinalRunningNotification<T>(operation: () => Promise<T>): Promise<T> {
    try {
      return await operation();
    } finally {
      this.resetIdleTimer();
      notifyRunningChange();
    }
  }

  /**
   * 停止当前这一轮，并且覆盖「这一轮还没真正开始」的空档。
   *
   * `AgentSession.abort()` 只有在 `_isAgentRunActive` / activeRun 存在时才真的取消。
   * 而一轮 prompt 在开始之前还要先跑 `reloadResourcesBeforePrompt()`（技能 / 插件 /
   * 扩展重载）、扩展 preflight 和模型鉴权；新建会话的第一条消息还要等整个
   * `startRpcSession()`。这段时间里点停止，`abort()` 立刻返回成功却什么都没取消，
   * 紧接着这一轮照常开始并回答模型 —— 表现就是「点了停止没反应，得等它答完再点」。
   * 所以只要还有未结算的 prompt，就隔一小段时间再取消一次，让它一开始就被中止。
   */
  private async abortRunOrPreparation(): Promise<void> {
    for (let attempt = 0; attempt < ABORT_PREPARATION_ATTEMPTS; attempt++) {
      const wasStreaming = this.inner.isStreaming;
      await this.inner.abort();
      // 正在跑的一轮已经被中止（流、工具、压缩都在 abort() 内部一起收掉）。
      if (wasStreaming || this.pendingPromptCount === 0) return;
      await new Promise((resolve) => setTimeout(resolve, ABORT_PREPARATION_POLL_MS));
    }
  }

  private emit(event: AgentEvent): void {
    for (const listener of this.listeners) {
      try {
        listener(event);
      } catch (error) {
        this.logDiagnostic("error", "agent_event_delivery_failed", {
          agentEvent: event.type,
          errorMessage: diagnosticErrorMessage(error),
        });
      }
    }
  }

  private diagnosticContext(): Record<string, unknown> {
    const model = this.inner.model;
    let cwd: string | undefined;
    try {
      cwd = this.cwd;
    } catch {
      cwd = undefined;
    }
    return {
      sessionId: this.inner.sessionId,
      cwd,
      provider: model?.provider,
      model: model?.id,
    };
  }

  private logDiagnostic(
    level: "error" | "warn",
    event: string,
    details: Record<string, unknown> = {},
  ): void {
    logAgentDiagnostic(level, event, { ...this.diagnosticContext(), ...details });
  }

  private logEventDiagnostic(event: AgentEvent): void {
    if (event.type === "message_end" && isRecord(event.message)) {
      const message = event.message;
      if (message.role === "assistant" && message.stopReason === "error") {
        this.logDiagnostic("error", "provider_message_error", {
          api: message.api,
          provider: message.provider,
          model: message.model,
          responseModel: message.responseModel,
          responseId: message.responseId,
          stopReason: message.stopReason,
          rawStopReason: message.rawStopReason,
          errorMessage: message.errorMessage ?? "Provider message ended with an error",
          usage: assistantUsageSummary(message.usage),
          ...assistantContentSummary(message.content),
        });
      }
      return;
    }

    if (event.type === "auto_retry_start") {
      this.logDiagnostic("warn", "provider_retry_scheduled", {
        attempt: event.attempt,
        maxAttempts: event.maxAttempts,
        delayMs: event.delayMs,
        errorMessage: event.errorMessage,
      });
      return;
    }

    if (event.type === "auto_retry_end" && event.success === false) {
      this.logDiagnostic("error", "provider_retry_exhausted", {
        attempt: event.attempt,
        errorMessage: event.finalError ?? "Provider retries exhausted",
      });
      return;
    }

    if (event.type === "compaction_end" && event.errorMessage) {
      this.logDiagnostic("error", "compaction_failed", {
        reason: event.reason,
        aborted: event.aborted,
        willRetry: event.willRetry,
        errorMessage: event.errorMessage,
      });
      return;
    }

    if (event.type === "tool_execution_end" && event.isError === true) {
      this.logDiagnostic("warn", "tool_execution_failed", {
        toolCallId: event.toolCallId,
        toolName: event.toolName,
      });
    }
  }

  private async acquirePromptAdmission(): Promise<() => void> {
    const previous = this.promptAdmissionTail;
    let release!: () => void;
    this.promptAdmissionTail = new Promise<void>((resolve) => {
      release = resolve;
    });
    await previous;
    return release;
  }

  private async reloadResourcesBeforePrompt(): Promise<void> {
    this.extensionStatuses.clear();
    this.resetExtensionWidgetsForReload();
    this.syncProjectTrust();
    await this.inner.reload({
      beforeSessionStart: () => {
        this.inner.extensionRunner.setUIContext?.(this.createExtensionUiContext(), "rpc");
      },
    });
    configureDesktopProviderRetry(this.inner);
  }

  private resetIdleTimer(): void {
    // 已销毁的 wrapper 不再排活：一个延后结算的 prompt 会在 destroy() 之后
    // 才走到 finishPrompt()，那一次 resetIdleTimer() 会给死会话重新挂上 10 分钟的定时器。
    if (!this._alive) return;
    if (this.idleTimer) clearTimeout(this.idleTimer);
    this.idleTimer = setTimeout(() => {
      if (this.isRunning()) {
        this.resetIdleTimer();
        return;
      }
      void this.shutdown().catch((error) => {
        this.logDiagnostic("error", "idle_session_shutdown_failed", {
          errorMessage: diagnosticErrorMessage(error),
        });
      });
    }, 10 * 60 * 1000);
  }

  private persistBashOnlySession(): void {
    const manager = this.inner.sessionManager;
    const sessionFile = manager.getSessionFile();
    if (!sessionFile || existsSync(sessionFile)) return;

    const header = manager.getHeader();
    if (!header) return;

    const content = [header, ...manager.getEntries()]
      .map((entry) => JSON.stringify(entry))
      .join("\n") + "\n";
    writeFileSync(sessionFile, content, { encoding: "utf8", flag: "wx" });

    // Pi normally delays the first flush until an assistant message exists.
    // A leading shell command has no assistant message, so mark this SDK
    // manager as flushed after writing its own generated entries.
    (manager as unknown as { flushed: boolean }).flushed = true;
    cacheSessionPath(this.inner.sessionId, sessionFile);
  }

  onEvent(listener: EventListener): () => void {
    this.listeners.push(listener);
    for (const event of this.pendingUiRequests.values()) listener(event);
    return () => {
      const i = this.listeners.indexOf(listener);
      if (i !== -1) this.listeners.splice(i, 1);
    };
  }

  onDestroy(cb: () => void): void {
    this.onDestroyCallback = cb;
  }

  async send(command: Record<string, unknown>): Promise<unknown> {
    this.resetIdleTimer();
    const type = command.type as string;
    if (command.uiLocale !== undefined) this.setUiLocale(command.uiLocale);
    if (this.shouldWaitForExtensions(type)) await this.waitForExtensionsBound();

    if (type === "prompt" || type === "steer" || type === "follow_up") {
      const imageError = validateAgentImages(command.images);
      if (imageError) throw new Error(imageError);
    }

    switch (type) {
      case "prompt": {
        // Serialize only admission. Once the preceding prompt has either
        // passed or failed preflight, the SDK can atomically decide whether
        // this submission starts a run or joins its streaming queue.
        const releaseAdmission = await this.acquirePromptAdmission();
        try {
          // bash 运行中不得开新的一轮，但引导/续跑（streamingBehavior）不受影响：
          // 命令跑着的时候正是最需要立刻改变方向的时刻，拒绝整条 steer
          // 只会逼用户去 abort。
          if (this.inner.isBashRunning && !command.streamingBehavior) {
            throw new Error("Cannot send a prompt while a shell command is running");
          }
          const promptImages = command.images as Array<{ type: "image"; data: string; mimeType: string }> | undefined;
          const streamingBehavior = command.streamingBehavior as "steer" | "followUp" | undefined;
          // AgentSession snapshots skills into its system prompt. Refresh that snapshot
          // before every new run so local SKILL.md changes apply without restarting.
          if (!streamingBehavior && !this.inner.isStreaming) {
            await this.reloadResourcesBeforePrompt();
          }
          // 普通对话模式承诺「不使用任何工具」：资源重载与扩展都可能在运行前重新激活工具，
          // 所以每次运行前再兜底清空一次（提示词由 before_agent_start 替换）。
          if (this.chatMode) this.inner.setActiveToolsByName([]);
          const skillInvocation = findSkillInvocation(
            command.message as string,
            this.inner.resourceLoader.getSkills().skills,
          );
          let preflightAccepted = false;
          let preflightSettled = false;
          let promptSettled = false;
          let acceptPreflight!: () => void;
          let rejectPreflight!: (error: unknown) => void;
          const preflight = new Promise<void>((resolve, reject) => {
            acceptPreflight = () => {
              preflightAccepted = true;
              if (preflightSettled) return;
              preflightSettled = true;
              resolve();
            };
            rejectPreflight = (error) => {
              if (preflightSettled) return;
              preflightSettled = true;
              reject(error);
            };
          });
          const finishPrompt = () => {
            if (promptSettled) return;
            promptSettled = true;
            this.pendingPromptCount = Math.max(0, this.pendingPromptCount - 1);
            this.resetIdleTimer();
            notifyRunningChange();
          };

          this.pendingPromptCount += 1;
          notifyRunningChange();
          // pi 1.0 起 preflightResult 改为回报「已接受」的派发方式
          // （"handled" | "queued" | "started"），被拒绝时不再回调 false，
          // 而是直接 reject 返回的 promise。这里只关心「是否已被接受」。
          let promptDisposition: "handled" | "queued" | "started" | undefined;
          let prompt: Promise<void>;
          try {
            prompt = this.inner.prompt(command.message as string, {
              ...(promptImages?.length ? { images: promptImages } : {}),
              ...(streamingBehavior ? { streamingBehavior } : {}),
              source: "rpc",
              // Match pi's RPC contract: acknowledge only after synchronous prompt
              // validation and extension preflight have accepted the submission.
              preflightResult: (disposition) => {
                promptDisposition = disposition;
                acceptPreflight();
              },
            });
          } catch (error) {
            this.logDiagnostic("error", "prompt_failed", {
              accepted: false,
              streamingBehavior,
              errorMessage: diagnosticErrorMessage(error),
            });
            finishPrompt();
            throw error;
          }

          void prompt.then(() => {
            // Compatibility fallback if a future SDK resolves without invoking
            // the internal callback. This waits for the run, but never acks early.
            acceptPreflight();
            finishPrompt();
            if (!streamingBehavior) this.emit({ type: "prompt_done" });
          }, (error) => {
            this.logDiagnostic("error", "prompt_failed", {
              accepted: preflightAccepted,
              streamingBehavior,
              errorMessage: diagnosticErrorMessage(error),
            });
            rejectPreflight(error);
            finishPrompt();
            invalidateSessionListCache();
            // A preflight rejection is returned by the POST itself. Only an
            // unexpected failure after acceptance needs the asynchronous event.
            if (preflightAccepted) {
              this.emit({
                type: "prompt_error",
                errorMessage: error instanceof Error ? error.message : String(error),
              });
              if (!streamingBehavior) this.emit({ type: "prompt_done" });
            }
          }).catch((error) => {
            this.logDiagnostic("error", "prompt_completion_handler_failed", {
              errorMessage: diagnosticErrorMessage(error),
            });
          });

          await preflight;
          if (skillInvocation) {
            try {
              recordSkillUsage(skillInvocation);
            } catch (error) {
              this.logDiagnostic("error", "skill_usage_record_failed", {
                skillName: skillInvocation.name,
                errorMessage: diagnosticErrorMessage(error),
              });
            }
          }
          // 与 pi 的 RPC 契约一致：回报本次 prompt 的派发结果，便于客户端区分
          // 「扩展命令已处理」「已排队」与「真正开始了新一轮运行」。
          return promptDisposition ? { disposition: promptDisposition } : null;
        } finally {
          releaseAdmission();
        }
      }

      case "abort":
        await this.withFinalRunningNotification(() => this.abortRunOrPreparation());
        return null;

      case "get_state": {
        const model = this.inner.model;
        const contextUsage = this.inner.getContextUsage();
        return {
          sessionId: this.inner.sessionId,
          sessionFile: this.inner.sessionFile ?? "",
          isStreaming: this.inner.isStreaming,
          isPromptRunning: this.pendingPromptCount > 0,
          isBashRunning: this.inner.isBashRunning,
          isCompacting: this.inner.isCompacting,
          autoCompactionEnabled: this.inner.autoCompactionEnabled,
          autoRetryEnabled: this.inner.autoRetryEnabled,
          model: model ? { id: model.id, provider: model.provider } : undefined,
          messageCount: 0,
          pendingMessageCount: this.inner.pendingMessageCount,
          queuedMessages: {
            steering: [...this.inner.getSteeringMessages()],
            followUp: [...this.inner.getFollowUpMessages()],
          },
          contextUsage: contextUsage
            ? { percent: contextUsage.percent, contextWindow: contextUsage.contextWindow, tokens: contextUsage.tokens }
            : null,
          systemPrompt: this.effectiveSystemPrompt(),
          chatMode: this.chatMode,
          thinkingLevel: this.inner.agent.state?.thinkingLevel ?? "off",
          extensionStatuses: this.getExtensionStatuses(),
          extensionWidgets: this.getExtensionWidgets(),
          pendingUiRequests: [...this.pendingUiRequests.values()],
        };
      }

      case "set_ui_locale": {
        this.setUiLocale(command.locale);
        return { systemPrompt: this.effectiveSystemPrompt() };
      }

      case "set_model": {
        const { provider, modelId } = command as { provider: string; modelId: string };
        // Always refresh (cache-only) first: an existing cached model object may
        // carry stale baseUrl/api config from an older models.json revision,
        // which makes requests 404 even though the settings test route succeeds.
        let model: ReturnType<typeof this.inner.modelRuntime.getModel> | undefined;
        for (let attempt = 0; attempt < 2 && !model; attempt++) {
          model = this.inner.modelRuntime.getModel(provider, modelId);
          if (!model) {
            await this.inner.modelRuntime.refresh({ allowNetwork: false });
            model = this.inner.modelRuntime.getModel(provider, modelId);
          }
        }
        if (!model) throw new Error(`Model not found: ${provider}/${modelId}`);
        await this.inner.setModel(model);
        invalidateModelsCache();
        invalidateSessionListCache();
        return { id: model.id, provider: model.provider };
      }

      case "fork": {
        if (this.inner.isBashRunning) {
          throw new Error("Cannot fork while a shell command is running");
        }
        const entryId = command.entryId as string;
        const sessionManager = this.inner.sessionManager;
        const currentSessionFile = this.inner.sessionFile;

        if (!sessionManager.isPersisted()) return { cancelled: true };
        if (!currentSessionFile) throw new Error("Persisted session is missing a session file");

        const entry = sessionManager.getEntry(entryId);
        if (!entry) throw new Error("Invalid entry ID for forking");

        const sessionDir = sessionManager.getSessionDir();
        let newSessionFile: string;

        if (!entry.parentId) {
          // Fork before the first message: create an empty session linked to this one
          const newManager = SessionManager.create(sessionManager.getCwd(), sessionDir);
          newManager.newSession({ parentSession: currentSessionFile });
          newSessionFile = newManager.getSessionFile() as string;
        } else {
          // Fork after some history: copy path up to (but not including) the fork point
          const sourceManager = SessionManager.open(currentSessionFile, sessionDir);
          const forkedPath = sourceManager.createBranchedSession(entry.parentId);
          if (!forkedPath) throw new Error("Failed to create forked session");
          newSessionFile = forkedPath;
        }

        const newSessionId = SessionManager.open(newSessionFile, sessionDir).getSessionId();
        cacheSessionPath(newSessionId, newSessionFile);
        invalidateSessionListCache();
        await this.shutdown();
        return { cancelled: false, newSessionId };
      }

      case "navigate_tree": {
        if (this.inner.isBashRunning) {
          throw new Error("Cannot navigate while a shell command is running");
        }
        const result = await this.inner.navigateTree(command.targetId as string, {});
        return {
          cancelled: result.cancelled,
          leafId: this.inner.sessionManager.getLeafId(),
        };
      }

      case "set_thinking_level": {
        const level = command.level as string;
        this.inner.setThinkingLevel(level);
        // setThinkingLevel clamps xhigh→high for models where supportsXhigh()===false.
        // If the model has DeepSeek thinking compat (reasoningEffortMap maps xhigh→max),
        // force the state back so the compat layer can use it correctly.
        if (level === "xhigh" && (this.inner.model as { compat?: { thinkingFormat?: string } } | null)?.compat?.thinkingFormat === "deepseek" && this.inner.agent?.state) {
          this.inner.agent.state.thinkingLevel = "xhigh";
        }
        invalidateSessionListCache();
        return null;
      }

      case "compact": {
        try {
          return await this.withFinalRunningNotification(() =>
            this.inner.compact(command.customInstructions as string | undefined)
          );
        } finally {
          invalidateSessionListCache();
        }
      }

      case "set_session_name": {
        const name = (command.name as string | undefined)?.trim();
        if (!name) throw new Error("Session name cannot be empty");
        this.inner.setSessionName(name);
        invalidateSessionListCache();
        return null;
      }

      case "get_session_stats": {
        return {
          ...this.inner.getSessionStats(),
          sessionName: this.inner.sessionManager.getSessionName(),
        };
      }

      case "get_last_assistant_text": {
        return { text: this.inner.getLastAssistantText() ?? "" };
      }

      case "set_auto_compaction": {
        this.inner.setAutoCompactionEnabled(command.enabled as boolean);
        return null;
      }

      case "clear_queue": {
        // Full clear only: pi has no single-item dequeue, and clear+requeue
        // races against the agent loop pulling messages mid-flight.
        return this.inner.clearQueue();
      }

      case "steer": {
        const steerImages = command.images as Array<{ type: "image"; data: string; mimeType: string }> | undefined;
        await this.inner.steer(command.message as string, steerImages?.length ? steerImages : undefined, { source: "rpc" });
        return null;
      }

      case "follow_up": {
        const followImages = command.images as Array<{ type: "image"; data: string; mimeType: string }> | undefined;
        await this.inner.followUp(command.message as string, followImages?.length ? followImages : undefined, { source: "rpc" });
        return null;
      }

      case "set_steering_mode": {
        const mode = parseQueueMode(command.mode);
        this.inner.setSteeringMode?.(mode);
        return { steeringMode: mode };
      }

      case "set_follow_up_mode": {
        const mode = parseQueueMode(command.mode);
        this.inner.setFollowUpMode?.(mode);
        return { followUpMode: mode };
      }

      case "get_tools": {
        const all: ToolInfo[] = this.inner.getAllTools();
        // 普通对话模式下界面也不能显示任何激活工具（见 prompt / ensureExtensionsBound 里的清空）。
        const active = new Set<string>(this.chatMode ? [] : this.inner.getActiveToolNames());
        return all.map((t) => ({
          name: t.name,
          description: t.description,
          active: active.has(t.name),
        }));
      }

      case "get_commands": {
        const commands: SlashCommandInfo[] = [];
        for (const registered of this.inner.extensionRunner.getRegisteredCommands()) {
          commands.push({
            name: registered.invocationName,
            description: registered.description,
            source: "extension",
            sourceInfo: registered.sourceInfo,
          });
        }
        for (const template of this.inner.promptTemplates) {
          commands.push({
            name: template.name,
            description: template.description,
            source: "prompt",
            sourceInfo: template.sourceInfo,
          });
        }
        for (const skill of this.inner.resourceLoader.getSkills().skills) {
          commands.push({
            name: `skill:${skill.name}`,
            description: skill.description,
            source: "skill",
            sourceInfo: skill.sourceInfo,
          });
        }
        return { commands };
      }

      case "set_tools": {
        const toolNames = command.toolNames as string[];
        const resolvedToolNames = applyShellTool(toolNames, resolveShellSelection(this.inner.settingsManager).tool);
        this.setForceEmptySystemPrompt(toolNames.length === 0);
        this.inner.setActiveToolsByName(withExtensionTools(this.inner, resolvedToolNames));
        return null;
      }

      // 输入框 π 图标切换的对话模式。提示词全在内存里（不写 transcript 结构），
      // 因此切换是瞬时的：不需要销毁并重建 AgentSession。
      case "set_chat_mode": {
        const on = command.on === true;
        const requestedToolNames = Array.isArray(command.toolNames) ? command.toolNames as string[] : undefined;
        if (on) {
          if (!this.chatMode) this.toolsBeforeChatMode = this.inner.getActiveToolNames();
          this.setChatMode(true);
          // 普通对话模式同样不启用任何工具，但保留非空提示词。
          this.setForceEmptySystemPrompt(false);
          this.inner.setActiveToolsByName([]);
          return { chatMode: true, systemPrompt: this.effectiveSystemPrompt(), toolNames: [] as string[] };
        }
        this.setChatMode(false);
        const restoreToolNames = requestedToolNames ?? this.toolsBeforeChatMode ?? [];
        this.toolsBeforeChatMode = null;
        const resolution = resolveShellSelection(this.inner.settingsManager);
        const resolvedToolNames = applyShellTool(restoreToolNames, resolution.tool);
        this.setForceEmptySystemPrompt(restoreToolNames.length === 0);
        this.inner.setActiveToolsByName(withExtensionTools(this.inner, resolvedToolNames));
        return {
          chatMode: false,
          systemPrompt: this.effectiveSystemPrompt(),
          toolNames: this.inner.getActiveToolNames(),
        };
      }

      case "reload": {
        await this.waitForExtensionsBound();
        this.extensionStatuses.clear();
        this.resetExtensionWidgetsForReload();
        this.syncProjectTrust();
        await this.inner.reload();
        if (typeof this.inner.bindExtensions !== "function") {
          this.inner.extensionRunner.setUIContext?.(this.createExtensionUiContext(), "rpc");
        }
        invalidateModelsCache();
        return { success: true };
      }

      case "abort_compaction": {
        this.inner.abortCompaction();
        return null;
      }

      case "extension_ui_response": {
        this.resolveExtensionUiResponse(command as ExtensionUiResponse);
        return null;
      }

      case "extension_ui_input": {
        this.handleExtensionUiInput(command.id as string, command.data as string);
        return null;
      }

      case "set_auto_retry": {
        this.inner.setAutoRetryEnabled(command.enabled as boolean);
        return null;
      }

      case "bash": {
        if (this.pendingPromptCount > 0 || this.inner.isStreaming || this.inner.isCompacting || this.inner.isBashRunning) {
          throw new Error("Cannot run a shell command while the session is busy");
        }
        const shellSelection = resolveShellSelection(this.inner.settingsManager);
        const execution = this.inner.executeBash(
          command.command as string,
          undefined,
          {
            excludeFromContext: command.excludeFromContext as boolean | undefined,
            operations: createProjectCommandBashOperations({
              ...(shellSelection.tool === "bash" ? { shellPath: shellSelection.shellPath } : {}),
              ...(shellSelection.tool === "powershell" ? { localOperations: createLocalPowerShellOperations() } : {}),
            }),
          },
        );
        notifyRunningChange();
        try {
          const result = await execution;
          this.persistBashOnlySession();
          return result;
        } finally {
          this.resetIdleTimer();
          invalidateSessionListCache();
          notifyRunningChange();
        }
      }

      case "abort_bash": {
        this.inner.abortBash();
        return null;
      }

      default:
        throw new Error(`Unsupported command: ${type}`);
    }
  }

  destroy(): void {
    if (!this._alive) return;
    this._alive = false;
    if (this.idleTimer) clearTimeout(this.idleTimer);
    if (this.inner.isBashRunning) this.inner.abortBash();
    this.unsubscribe?.();
    for (const pending of this.pendingUiResponses.values()) pending.cancel();
    for (const id of Array.from(this.activeCustomUis.keys())) this.closeCustomUi(id, undefined);
    this.pendingUiResponses.clear();
    this.pendingUiRequests.clear();
    this.clearExtensionWidgets(false);
    try {
      this.inner.dispose();
    } finally {
      try {
        this.onDestroyCallback?.();
      } finally {
        notifyRunningChange();
      }
    }
  }

  async shutdown(): Promise<void> {
    if (this.shutdownPromise) return this.shutdownPromise;
    if (!this._alive) return;

    this.shutdownPromise = (async () => {
      try {
        try {
          await this.waitForExtensionsBound();
        } catch (error) {
          this.logDiagnostic("error", "extension_binding_failed_before_shutdown", {
            errorMessage: diagnosticErrorMessage(error),
          });
        }
        await this.inner.extensionRunner.emit?.({ type: "session_shutdown", reason: "quit" });
      } finally {
        this.destroy();
      }
    })();
    return this.shutdownPromise;
  }

  private resolveExtensionUiResponse(response: ExtensionUiResponse): void {
    const pending = this.pendingUiResponses.get(response.id);
    if (!pending) return;
    // “已作答”的广播在 requestExtensionUi 的 cleanup 里统一发出（那里也是超时/
    // 中止的出口），所以这里只负责唤醒等着的那个 Promise。
    pending.resolve(response);
  }

  private getExtensionStatuses(): Array<{ key: string; text: string }> {
    return Array.from(this.extensionStatuses, ([key, text]) => ({ key, text }));
  }

  private getExtensionWidgets(): ExtensionWidgetItem[] {
    return Array.from(this.extensionWidgets.values());
  }

  private nextExtensionWidgetGeneration(key: string): number {
    const generation = (this.extensionWidgetGenerations.get(key) ?? 0) + 1;
    this.extensionWidgetGenerations.set(key, generation);
    return generation;
  }

  private disposeExtensionWidgetComponent(component: unknown): void {
    if (!component || (typeof component !== "object" && typeof component !== "function")) return;
    const dispose = (component as { dispose?: unknown }).dispose;
    if (typeof dispose !== "function") return;
    try {
      dispose.call(component);
    } catch {
      // Ignore dispose errors from extension widgets.
    }
  }

  private emitExtensionWidgetClear(key: string): void {
    this.emit({
      type: "extension_ui_request",
      id: randomUUID(),
      method: "setWidget",
      widgetKey: key,
      widgetLines: undefined,
      widgetPlacement: undefined,
    } as ExtensionUiRequest as AgentEvent);
  }

  private clearExtensionWidget(key: string, emitClear = true): number {
    const generation = this.nextExtensionWidgetGeneration(key);

    const active = this.activeExtensionWidgets.get(key);
    this.activeExtensionWidgets.delete(key);
    this.extensionWidgets.delete(key);
    if (active) this.disposeExtensionWidgetComponent(active.component);
    if (this.extensionWidgetGenerations.get(key) !== generation) return generation;
    if (emitClear) this.emitExtensionWidgetClear(key);
    return generation;
  }

  private clearExtensionWidgets(emitClear: boolean): void {
    const keys = new Set([
      ...this.extensionWidgets.keys(),
      ...this.activeExtensionWidgets.keys(),
    ]);
    for (const key of keys) this.clearExtensionWidget(key, emitClear);
  }

  private resetExtensionWidgetsForReload(): void {
    this.extensionWidgetsResetting = true;
    try {
      const factoryKeys = [...this.activeExtensionWidgets.keys()];
      for (const key of factoryKeys) this.clearExtensionWidget(key);
      // Keep the existing array-widget reload behavior: snapshots are reset and
      // the next extension session_start repopulates them.
      this.extensionWidgets.clear();
    } finally {
      this.extensionWidgetsResetting = false;
    }
  }

  private emitExtensionWidgetError(key: string, error: unknown): void {
    this.emit({
      type: "extension_error",
      extensionPath: `extension-widget:${key}`,
      event: "setWidget",
      error: error instanceof Error ? error.message : String(error),
    });
  }

  private failExtensionWidget(
    key: string,
    generation: number,
    error: unknown,
    clearEmitted: boolean,
    component?: unknown,
  ): void {
    if (this.extensionWidgetGenerations.get(key) !== generation) {
      this.disposeExtensionWidgetComponent(component);
      return;
    }

    const active = this.activeExtensionWidgets.get(key);
    let shouldEmitClear = !clearEmitted;
    if (active?.generation === generation) {
      shouldEmitClear = active.rendered || !active.clearEmitted;
      this.activeExtensionWidgets.delete(key);
      this.disposeExtensionWidgetComponent(active.component);
    } else {
      this.disposeExtensionWidgetComponent(component);
    }
    if (this.extensionWidgetGenerations.get(key) !== generation) {
      this.emitExtensionWidgetError(key, error);
      return;
    }
    this.extensionWidgets.delete(key);
    if (shouldEmitClear) this.emitExtensionWidgetClear(key);
    this.emitExtensionWidgetError(key, error);
  }

  private renderExtensionWidget(active: ActiveExtensionWidget): void {
    if (
      this.activeExtensionWidgets.get(active.key) !== active
      || this.extensionWidgetGenerations.get(active.key) !== active.generation
    ) return;

    let lines: unknown;
    try {
      lines = active.component.render(DEFAULT_CUSTOM_UI_COLUMNS);
    } catch (error) {
      this.failExtensionWidget(active.key, active.generation, error, active.clearEmitted);
      return;
    }
    if (!Array.isArray(lines) || !lines.every((line) => typeof line === "string")) {
      this.failExtensionWidget(
        active.key,
        active.generation,
        new Error("Extension widget render must return string[]"),
        active.clearEmitted,
      );
      return;
    }
    if (
      this.activeExtensionWidgets.get(active.key) !== active
      || this.extensionWidgetGenerations.get(active.key) !== active.generation
    ) return;

    const widgetLines = lines as string[];
    this.extensionWidgets.set(active.key, {
      key: active.key,
      lines: widgetLines,
      placement: active.placement,
    });
    active.rendered = true;
    this.emit({
      type: "extension_ui_request",
      id: randomUUID(),
      method: "setWidget",
      widgetKey: active.key,
      widgetLines,
      widgetPlacement: active.placement,
    } as ExtensionUiRequest as AgentEvent);
  }

  private setExtensionWidgetFactory(
    key: string,
    factory: ExtensionWidgetFactory,
    options?: { placement?: "aboveEditor" | "belowEditor" },
  ): void {
    const hadPrevious = this.extensionWidgets.has(key) || this.activeExtensionWidgets.has(key);
    const generation = this.clearExtensionWidget(key, hadPrevious);
    if (this.extensionWidgetGenerations.get(key) !== generation) return;
    const tui = createHeadlessCustomUiTui(() => {
      const active = this.activeExtensionWidgets.get(key);
      if (active?.generation === generation) this.renderExtensionWidget(active);
    }, DEFAULT_CUSTOM_UI_COLUMNS);

    let component: unknown;
    try {
      component = factory(tui, PLAIN_TEXT_THEME);
    } catch (error) {
      this.failExtensionWidget(key, generation, error, hadPrevious);
      return;
    }
    if (this.extensionWidgetGenerations.get(key) !== generation) {
      this.disposeExtensionWidgetComponent(component);
      return;
    }
    if (
      !component
      || (typeof component !== "object" && typeof component !== "function")
      || typeof (component as { render?: unknown }).render !== "function"
    ) {
      this.failExtensionWidget(
        key,
        generation,
        new Error("Extension widget factory must return a component with render(width)"),
        hadPrevious,
        component,
      );
      return;
    }

    const active: ActiveExtensionWidget = {
      key,
      component: component as ExtensionWidgetComponent,
      placement: options?.placement ?? "aboveEditor",
      generation,
      clearEmitted: hadPrevious,
      rendered: false,
    };
    this.activeExtensionWidgets.set(key, active);
    this.renderExtensionWidget(active);
  }

  private getCustomUiWidth(options: unknown): number {
    if (!options || typeof options !== "object") return DEFAULT_CUSTOM_UI_COLUMNS;
    const overlayOptions = (options as { overlayOptions?: unknown }).overlayOptions;
    const resolved = typeof overlayOptions === "function" ? overlayOptions() : overlayOptions;
    if (!resolved || typeof resolved !== "object") return DEFAULT_CUSTOM_UI_COLUMNS;
    const width = (resolved as { width?: unknown }).width;
    return typeof width === "number" && Number.isFinite(width)
      ? Math.max(40, Math.min(140, Math.round(width)))
      : 92;
  }

  private emitCustomUiRender(id: string, custom: ActiveCustomUi): void {
    let lines: string[];
    try {
      lines = custom.component.render(custom.width);
    } catch (error) {
      lines = [`Extension custom UI render failed: ${error instanceof Error ? error.message : String(error)}`];
    }
    const event = {
      type: "extension_ui_request",
      id,
      method: "custom",
      lines,
    } as ExtensionUiRequest as AgentEvent;
    this.pendingUiRequests.set(id, event);
    this.emit(event);
  }

  private closeCustomUi(id: string, value: unknown): void {
    const custom = this.activeCustomUis.get(id);
    if (!custom || custom.settled) return;
    custom.settled = true;
    this.activeCustomUis.delete(id);
    this.pendingUiRequests.delete(id);
    try {
      custom.component.dispose?.();
    } catch {
      // Ignore dispose errors from extension UI components.
    }
    this.emit({
      type: "extension_ui_request",
      id,
      method: "custom",
      lines: [],
      closed: true,
    } as ExtensionUiRequest as AgentEvent);
    custom.resolve(value);
  }

  private handleExtensionUiInput(id: string, data: string): void {
    const custom = this.activeCustomUis.get(id);
    if (!custom || typeof data !== "string") return;
    try {
      custom.component.handleInput?.(data);
      if (this.activeCustomUis.has(id)) this.emitCustomUiRender(id, custom);
    } catch (error) {
      this.closeCustomUi(id, undefined);
      this.emit({
        type: "extension_error",
        extensionPath: `custom-ui:${id}`,
        event: "custom_ui_input",
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }

  private requestExtensionCustomUi<T>(
    factory: unknown,
    options?: unknown,
  ): Promise<T> {
    if (typeof factory !== "function") return Promise.resolve(undefined as T);

    const id = randomUUID();
    const width = this.getCustomUiWidth(options);

    return new Promise<T>((resolve) => {
      let completed = false;
      const tui = createHeadlessCustomUiTui(
        () => {
          const custom = this.activeCustomUis.get(id);
          if (custom) this.emitCustomUiRender(id, custom);
        },
        width,
      );
      const finish = (value: T) => {
        if (completed) return;
        completed = true;
        resolve(value);
      };
      const done = (value: T) => {
        if (this.activeCustomUis.has(id)) {
          this.closeCustomUi(id, value);
        } else {
          finish(value);
        }
      };

      Promise.resolve()
        .then(() => factory(tui, PLAIN_TEXT_THEME, CUSTOM_UI_KEYBINDINGS, done))
        .then((component) => {
          if (completed) {
            try {
              (component as CustomUiComponent | undefined)?.dispose?.();
            } catch {
              // Ignore dispose errors from a component completed before mounting.
            }
            return;
          }
          if (!component || typeof component !== "object" || typeof (component as CustomUiComponent).render !== "function") {
            finish(undefined as T);
            return;
          }
          const custom: ActiveCustomUi = {
            component: component as CustomUiComponent,
            width,
            resolve: (value) => finish(value as T),
            settled: false,
          };
          this.activeCustomUis.set(id, custom);
          this.emitCustomUiRender(id, custom);
        })
        .catch((error) => {
          if (completed) return;
          this.emit({
            type: "extension_error",
            extensionPath: `custom-ui:${id}`,
            event: "custom_ui",
            error: error instanceof Error ? error.message : String(error),
          });
          finish(undefined as T);
        });
    });
  }

  private requestExtensionUi<T>(
    request: ExtensionUiRequestBody,
    defaultValue: T,
    parseResponse: (response: ExtensionUiResponse) => T,
    timeout?: number,
    signal?: AbortSignal,
  ): Promise<T> {
    if (signal?.aborted) return Promise.resolve(defaultValue);

    const id = randomUUID();
    const fullRequest = {
      type: "extension_ui_request",
      id,
      ...request,
      ...(timeout ? { timeout, expiresAt: Date.now() + timeout } : {}),
    };

    return new Promise((resolve) => {
      let timeoutId: ReturnType<typeof setTimeout> | undefined;
      let announced = false;
      /**
       * 请求不再存在的那一刻（被回答 / 超时 / 被中止 / 被取消）都从这里走，
       * 所以“这条请求作废了”的广播也放在这里：这是唯一的出口。
       *
       * 不放在 `resolveExtensionUiResponse` 里是因为超时与中止不经过它，
       * 那种情况下另一边只能等 15s 状态对账才能把弹窗收掉。
       */
      const announceResolved = () => {
        if (announced) return;
        announced = true;
        this.emit({ type: "extension_ui_resolved", id } as AgentEvent);
      };
      const cleanup = () => {
        if (timeoutId) clearTimeout(timeoutId);
        signal?.removeEventListener("abort", onAbort);
        this.pendingUiRequests.delete(id);
        this.pendingUiResponses.delete(id);
        announceResolved();
      };
      const settle = (value: T) => {
        cleanup();
        resolve(value);
      };
      const onAbort = () => settle(defaultValue);

      if (timeout) timeoutId = setTimeout(() => settle(defaultValue), timeout);
      signal?.addEventListener("abort", onAbort, { once: true });

      this.pendingUiRequests.set(id, fullRequest as AgentEvent);
      this.pendingUiResponses.set(id, {
        resolve: (response) => settle(parseResponse(response)),
        cancel: () => settle(defaultValue),
      });
      this.emit(fullRequest as AgentEvent);
    });
  }

  private createExtensionUiContext(): ExtensionUiContextLike {
    return {
      select: (title, options, opts) => this.requestExtensionUi(
        { method: "select", title, options, ...(opts?.timeout ? { timeout: opts.timeout } : {}) },
        undefined,
        (response) => "value" in response ? response.value : undefined,
        opts?.timeout,
        opts?.signal,
      ),
      confirm: (title, message, opts) => this.requestExtensionUi(
        { method: "confirm", title, message, ...(opts?.timeout ? { timeout: opts.timeout } : {}) },
        false,
        (response) => "confirmed" in response ? response.confirmed : false,
        opts?.timeout,
        opts?.signal,
      ),
      input: (title, placeholder, opts) => this.requestExtensionUi(
        { method: "input", title, ...(placeholder !== undefined ? { placeholder } : {}), ...(opts?.sensitive ? { sensitive: true } : {}), ...(opts?.timeout ? { timeout: opts.timeout } : {}) },
        undefined,
        (response) => "value" in response ? response.value : undefined,
        opts?.timeout,
        opts?.signal,
      ),
      editor: (title, prefill, opts) => this.requestExtensionUi(
        { method: "editor", title, ...(prefill !== undefined ? { prefill } : {}), ...(opts?.timeout ? { timeout: opts.timeout } : {}) },
        undefined,
        (response) => "value" in response ? response.value : undefined,
        opts?.timeout,
        opts?.signal,
      ),
      notify: (message, type) => {
        this.emit({
          type: "extension_ui_request",
          id: randomUUID(),
          method: "notify",
          message,
          notifyType: type,
        } as ExtensionUiRequest as AgentEvent);
      },
      onTerminalInput: () => () => {},
      setStatus: (key, text) => {
        if (text === undefined) this.extensionStatuses.delete(key);
        else this.extensionStatuses.set(key, text);
        this.emit({
          type: "extension_ui_request",
          id: randomUUID(),
          method: "setStatus",
          statusKey: key,
          statusText: text,
        } as ExtensionUiRequest as AgentEvent);
      },
      setWorkingMessage: () => {},
      setWorkingVisible: () => {},
      setWorkingIndicator: () => {},
      setHiddenThinkingLabel: () => {},
      setWidget: (key, content, options) => {
        if (!this._alive || this.extensionWidgetsResetting) return;
        if (typeof content === "function") {
          this.setExtensionWidgetFactory(
            key,
            content as unknown as ExtensionWidgetFactory,
            options,
          );
          return;
        }
        if (content !== undefined && !Array.isArray(content)) return;
        if (content === undefined) {
          this.clearExtensionWidget(key);
          return;
        }
        const generation = this.activeExtensionWidgets.has(key)
          ? this.clearExtensionWidget(key)
          : this.nextExtensionWidgetGeneration(key);
        if (this.extensionWidgetGenerations.get(key) !== generation) return;
        this.extensionWidgets.set(key, {
          key,
          lines: content,
          placement: options?.placement ?? "aboveEditor",
        });
        this.emit({
          type: "extension_ui_request",
          id: randomUUID(),
          method: "setWidget",
          widgetKey: key,
          widgetLines: content,
          widgetPlacement: options?.placement,
        } as ExtensionUiRequest as AgentEvent);
      },
      setFooter: () => {},
      setHeader: () => {},
      setTitle: (title) => {
        this.emit({
          type: "extension_ui_request",
          id: randomUUID(),
          method: "setTitle",
          title,
        } as ExtensionUiRequest as AgentEvent);
      },
      custom: <T = unknown>(factory: unknown, options?: unknown) => this.requestExtensionCustomUi<T>(factory, options),
      pasteToEditor: (text) => {
        this.emit({
          type: "extension_ui_request",
          id: randomUUID(),
          method: "set_editor_text",
          text,
        } as ExtensionUiRequest as AgentEvent);
      },
      setEditorText: (text) => {
        this.emit({
          type: "extension_ui_request",
          id: randomUUID(),
          method: "set_editor_text",
          text,
        } as ExtensionUiRequest as AgentEvent);
      },
      getEditorText: () => "",
      addAutocompleteProvider: () => {},
      setEditorComponent: () => {},
      getEditorComponent: () => undefined,
      get theme() { return PLAIN_TEXT_THEME; },
      getAllThemes: () => [],
      getTheme: () => undefined,
      setTheme: () => ({ success: false, error: "Theme switching is not supported in Pi Desktop extension UI yet" }),
      getToolsExpanded: () => false,
      setToolsExpanded: () => {},
    };
  }

  private createExtensionCommandContextActions(): ExtensionCommandContextActionsLike {
    return {
      waitForIdle: async () => {
        const agent = this.inner.agent as { waitForIdle?: () => Promise<void> };
        await agent.waitForIdle?.();
      },
      newSession: async () => ({ cancelled: true }),
      fork: async () => ({ cancelled: true }),
      navigateTree: async (targetId, options) => {
        const result = await this.inner.navigateTree(targetId, { summarize: options?.summarize });
        return { cancelled: result.cancelled };
      },
      switchSession: async () => ({ cancelled: true }),
      reload: async () => {
        this.extensionStatuses.clear();
        this.resetExtensionWidgetsForReload();
        this.syncProjectTrust();
        await this.inner.reload({
          beforeSessionStart: () => {
            this.inner.extensionRunner.setUIContext?.(this.createExtensionUiContext(), "rpc");
          },
        });
      },
    };
  }

  private syncProjectTrust(): void {
    const status = getProjectTrustStatus(this.cwd, getAgentDir());
    this.inner.settingsManager.setProjectTrusted(status.trusted);
  }
}

// ============================================================================
// Session registry
// ============================================================================

export interface RunningSessionsSnapshot {
  runningSessionIds: string[];
  pendingUiSessionIds: string[];
}

declare global {
  var __piSessions: Map<string, AgentSessionWrapper> | undefined;
  var __piStartLocks: Map<string, Promise<{ session: AgentSessionWrapper; realSessionId: string }>> | undefined;
  var __piStartingSessionCwds: Map<string, number> | undefined;
  var __piRunningListeners: Set<(snapshot: RunningSessionsSnapshot) => void> | undefined;
}

function getRegistry(): Map<string, AgentSessionWrapper> {
  if (!globalThis.__piSessions) {
    globalThis.__piSessions = new Map();
    const cleanup = () => globalThis.__piSessions?.forEach((s) => s.destroy());
    process.once("exit", cleanup);
    process.once("SIGINT", cleanup);
    process.once("SIGTERM", cleanup);
  }
  return globalThis.__piSessions;
}

function getLocks(): Map<string, Promise<{ session: AgentSessionWrapper; realSessionId: string }>> {
  if (!globalThis.__piStartLocks) globalThis.__piStartLocks = new Map();
  return globalThis.__piStartLocks;
}

function normalizeRpcCwd(cwd: string): string {
  const resolvedCwd = resolve(cwd);
  try {
    return realpathSync(resolvedCwd);
  } catch {
    return resolvedCwd;
  }
}

function getStartingSessionCwds(): Map<string, number> {
  if (!globalThis.__piStartingSessionCwds) globalThis.__piStartingSessionCwds = new Map();
  return globalThis.__piStartingSessionCwds;
}

function trackStartingSession(cwd: string): () => void {
  const startingCwds = getStartingSessionCwds();
  const key = normalizeRpcCwd(cwd);
  startingCwds.set(key, (startingCwds.get(key) ?? 0) + 1);
  return () => {
    const remaining = (startingCwds.get(key) ?? 1) - 1;
    if (remaining > 0) startingCwds.set(key, remaining);
    else startingCwds.delete(key);
  };
}

export function getRpcSession(sessionId: string): AgentSessionWrapper | undefined {
  return getRegistry().get(sessionId);
}

function messageText(content: unknown): string {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .map((block) => (
      block && typeof block === "object" && (block as { type?: string }).type === "text"
        ? String((block as { text?: unknown }).text ?? "")
        : ""
    ))
    .filter(Boolean)
    .join(" ");
}

function runtimeMessageText(entry: SessionMessageEntry): string {
  if (entry.message.role === "bashExecution") return "";
  return messageText(entry.message.content);
}

function runtimeMessageActivityMs(entry: SessionMessageEntry): number | undefined {
  if (entry.message.role !== "user" && entry.message.role !== "assistant") return undefined;
  if (typeof entry.message.timestamp === "number") return entry.message.timestamp;
  const timestamp = new Date(entry.timestamp).getTime();
  return Number.isNaN(timestamp) ? undefined : timestamp;
}

/**
 * Return live sessions that should be visible in the session list. Pi delays
 * the first JSONL flush until an assistant message exists, so an accepted new
 * prompt must temporarily be described from its in-memory SessionManager.
 */
export function getRpcSessionInfos(): SessionInfo[] {
  const sessions: SessionInfo[] = [];
  for (const session of getRegistry().values()) {
    if (!session.isAlive()) continue;

    const manager = session.inner.sessionManager;
    const header = manager.getHeader();
    const entries = manager.getEntries() as unknown as Array<
      { type: string; timestamp: string } | SessionMessageEntry
    >;
    const messages = entries.filter((entry): entry is SessionMessageEntry => entry.type === "message");
    const firstUserMessage = messages.find((entry) => entry.message.role === "user");
    const sessionFile = manager.getSessionFile() ?? session.sessionFile;
    const persisted = Boolean(sessionFile && existsSync(sessionFile));

    // An ensure_session call creates an idle, empty runtime while the composer
    // loads commands. Do not leak it into history before a prompt is accepted.
    //
    // A freshly accepted prompt is already running before its user message
    // reaches the SessionManager: `message_end` is what appends it, and pi
    // delays the first JSONL flush even longer. Gating on `firstUserMessage`
    // made the sidebar wait for the whole answer before the new session
    // appeared, so `isRunning()` alone decides visibility.
    if (!persisted && !session.isRunning()) continue;

    // The prompt the user just sent may only exist in agent state so far.
    const liveFirstUserMessage = firstUserMessage
      ? undefined
      : session.inner.agent?.state?.messages?.find((message) => message.role === "user");
    const firstMessageText = firstUserMessage
      ? runtimeMessageText(firstUserMessage)
      : liveFirstUserMessage
        ? messageText(liveFirstUserMessage.content)
        : "";

    const created = header?.timestamp
      ?? entries[0]?.timestamp
      ?? new Date().toISOString();
    const headerTimestamp = new Date(created).getTime();
    let lastActivityMs = Number.isNaN(headerTimestamp) ? Date.now() : headerTimestamp;
    for (const message of messages) {
      const activityMs = runtimeMessageActivityMs(message);
      if (activityMs !== undefined) lastActivityMs = Math.max(lastActivityMs, activityMs);
    }

    sessions.push({
      path: sessionFile ?? "",
      id: header?.id ?? session.sessionId,
      cwd: header?.cwd ?? session.cwd,
      name: manager.getSessionName(),
      created,
      modified: new Date(lastActivityMs).toISOString(),
      messageCount: messages.length || (liveFirstUserMessage ? 1 : 0),
      firstMessage: firstMessageText || "(no messages)",
      transient: !persisted,
    });
  }
  return sessions;
}

export function hasBusyRpcSessionForCwd(cwd: string): boolean {
  const targetCwd = normalizeRpcCwd(cwd);
  if (getStartingSessionCwds().has(targetCwd)) return true;
  return Array.from(getRegistry().values()).some(
    (session) => normalizeRpcCwd(session.cwd) === targetCwd && session.isRunning(),
  );
}

export async function destroyRpcSessionsForCwd(cwd: string): Promise<number> {
  const targetCwd = normalizeRpcCwd(cwd);
  const sessions = Array.from(getRegistry().values()).filter(
    (session) => normalizeRpcCwd(session.cwd) === targetCwd,
  );
  await Promise.all(sessions.map((session) => session.shutdown()));
  return sessions.length;
}

export function getRunningRpcSessionsSnapshot(): RunningSessionsSnapshot {
  const running = new Set<string>();
  const pendingUi = new Set<string>();
  for (const [sessionId, session] of getRegistry()) {
    const id = session.sessionId || sessionId;
    if (session.isRunning()) running.add(id);
    if (session.hasPendingUiRequests()) pendingUi.add(id);
  }
  return {
    runningSessionIds: [...running],
    pendingUiSessionIds: [...pendingUi],
  };
}

export function getRunningRpcSessionIds(): string[] {
  return getRunningRpcSessionsSnapshot().runningSessionIds;
}

export function getPendingUiRpcSessionIds(): string[] {
  return getRunningRpcSessionsSnapshot().pendingUiSessionIds;
}

// ----------------------------------------------------------------------------
// Running-status broadcaster
//
// Pushes running state and pending user prompts to subscribers whenever either
// may have changed. This lets the sidebar receive live updates over SSE instead
// of polling. Listeners live on globalThis so they survive Next.js hot-reload.
// ----------------------------------------------------------------------------

function getRunningListeners(): Set<(snapshot: RunningSessionsSnapshot) => void> {
  if (!globalThis.__piRunningListeners) globalThis.__piRunningListeners = new Set();
  return globalThis.__piRunningListeners;
}

/** Subscribe to running/pending-ui snapshot changes. Returns an unsubscribe function. */
export function subscribeRunningSessions(listener: (snapshot: RunningSessionsSnapshot) => void): () => void {
  const listeners = getRunningListeners();
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

let lastRunningSnapshot = "";

/**
 * Recompute the running/pending-ui snapshot and, if it changed since the last
 * notification, broadcast it to subscribers.
 */
export function notifyRunningChange(): void {
  const listeners = getRunningListeners();
  if (listeners.size === 0) {
    // A future subscriber receives its own initial snapshot. Clear this one so
    // its first state transition cannot match stale state from an old listener.
    lastRunningSnapshot = "";
    return;
  }
  const snapshot = getRunningRpcSessionsSnapshot();
  const serialized = JSON.stringify({
    runningSessionIds: [...snapshot.runningSessionIds].sort(),
    pendingUiSessionIds: [...snapshot.pendingUiSessionIds].sort(),
  });
  if (serialized === lastRunningSnapshot) return;
  lastRunningSnapshot = serialized;
  for (const listener of listeners) {
    try { listener(snapshot); } catch { /* ignore listener errors */ }
  }
}

/**
 * Get or create an AgentSession for the given session.
 * For new sessions (sessionFile === ""), pi generates its own id.
 * New sessions resolve enabledModels before construction so the initial model,
 * thinking pin, and SDK scopedModels share one settings snapshot.
 * Pass options.toolNames to pre-configure active tools (empty = all disabled).
 * Pass options.chatMode to start in the plain-conversation mode: it forces the tool set
 * empty and replaces the whole system prompt (see lib/chat-mode-prompt.ts).
 */
export async function startRpcSession(
  sessionId: string,
  sessionFile: string,
  cwd: string | undefined,
  options: RpcSessionStartOptions = {},
): Promise<{ session: AgentSessionWrapper; realSessionId: string }> {
  const { initialModel, thinkingLevel, uiLocale, chatMode } = options;
  // 普通对话模式不使用任何工具，所以调用方传什么工具集都会被忽略。
  // 注意它与 forceEmpty 的区别：两者都不用工具，但只有 forceEmpty 会清空提示词。
  const toolNames = chatMode ? [] : options.toolNames;
  const forceEmpty = toolNames?.length === 0 && chatMode !== true;
  const sessionLocale: SessionPromptState = {
    value: normalizeUiLocale(uiLocale),
    forceEmpty,
    chatMode: chatMode === true,
  };
  const registry = getRegistry();
  const locks = getLocks();

  const existing = registry.get(sessionId);
  if (existing?.isAlive()) return { session: existing, realSessionId: sessionId };

  const inflight = locks.get(sessionId);
  if (inflight) return inflight;

  let sessionManager: SessionManager;
  if (sessionFile) {
    sessionManager = SessionManager.open(sessionFile, undefined);
  } else {
    if (!cwd) throw new Error("cwd is required for a new session");
    sessionManager = SessionManager.create(cwd, undefined);
  }
  const sessionCwd = sessionManager.getCwd();
  const finishStartingSession = trackStartingSession(sessionCwd);
  const starting = (async () => {
    // Some extensions access the SDK's global theme even outside the terminal UI.
    initTheme();
    // 打包版从 Dock 启动时继承的是 launchd 最小 PATH：`bash` 工具里没有 node/npm，
    // 插件安装也会 `spawn npm ENOENT`。会话启动时先把找到的 node 目录补进 PATH，
    // 之后 pi 派生的所有子进程（bash 工具、包管理器、git）都跟着受益。
    ensureHostToolPath();
    const agentDir = getAgentDir();

    // Determine which tools to pass based on requested toolNames.
    // Since v0.68.0, session creation expects string[] tool names instead of Tool[] instances.
    let toolsOption: string[] | undefined;
    if (toolNames !== undefined && !chatMode) {
      // toolNames === [] -> "all off" (an empty allow-list disables every tool).
      // Otherwise DO NOT pass a builtin-only allow-list: passing CODING_TOOL_NAMES
      // set allowedToolNames to coding builtins only, which filtered every
      // extension/package-provided tool (e.g. subagents, web access) out of the
      // tool registry — so they were unavailable in Pi Desktop sessions even though the
      // `pi` CLI keeps them. Leaving the allow-list unset lets the SDK register all
      // tools (and activate extension tools); we narrow the ACTIVE set below.
      //
      // 普通对话模式必须走 `undefined` 而不是 `[]`：空 allow-list 会让 SDK 的
      // 工具注册表也是空的，之后 `set_chat_mode { on: false }` 就再也激活不回任何工具。
      // 它只禁用激活，见下方 `setActiveToolsByName([])`。
      toolsOption = toolNames.length === 0 ? [] : undefined;
    }

    // Build services first so extension-registered providers are available
    // before the SDK restores the saved model from the session file.
    // Gate untrusted project extensions so opening a repository does not run
    // its .pi/extensions code automatically (see lib/project-trust.ts, #236).
    const trustReloadOptions = projectTrustReloadOptions(sessionCwd, agentDir);
    const settingsManager = SettingsManager.create(sessionCwd, agentDir);
    // On Windows without Git Bash, expose PowerShell through the same preset
    // slot instead of letting every bash call fail with "No bash shell found".
    const shellSelection = resolveShellSelection(settingsManager);
    const shellTool = shellSelection.tool;
    const modelRuntime = await createDesktopModelRuntime({
      authPath: `${agentDir}/auth.json`,
      modelsPath: `${agentDir}/models.json`,
    });
    const services = await createAgentSessionServices({
      cwd: sessionCwd,
      agentDir,
      settingsManager,
      modelRuntime,
      resourceLoaderOptions: {
        extensionFactories: [
          createLanguagePromptExtension(sessionLocale),
          createSystemTimeExtension(),
          createAskUserExtension(),
          createTaskProgressExtension(),
          createProjectCommandBashExtension({
            cwd: sessionCwd,
            settings: settingsManager,
            shellSelection,
          }),
        ],
        extensionsOverride: preferUserBashExtension,
      },
      ...(trustReloadOptions ? { resourceLoaderReloadOptions: trustReloadOptions } : {}),
    });
    // 本地缓存（models-store.json / pi.sqlite 目录缓存）已能给出已配置服务商的
    // 模型；远端刷新不阻塞会话创建，否则发第一条消息要等 models.dev 数秒。
    void refreshDesktopProviderCatalogs(services.modelRuntime).catch(() => {});
    const scope = await resolveVisibleModels(
      services.modelRuntime,
      services.settingsManager.getEnabledModels(),
    );
    const defaultProvider = services.settingsManager.getDefaultProvider();
    const defaultModelId = services.settingsManager.getDefaultModel();
    const hasExistingMessages = sessionManager.getBranch().some((entry) => entry.type === "message");
    const initial = hasExistingMessages
      ? { scopedModels: [...scope.scopedModels] }
      : selectInitialModelScope(scope, {
        ...(initialModel ? { requestedModel: initialModel } : {}),
        ...(defaultProvider && defaultModelId
          ? { defaultModel: { provider: defaultProvider, modelId: defaultModelId } }
          : {}),
        ...(thinkingLevel ? { thinkingLevel } : {}),
      });
    const { session: inner } = await createAgentSessionFromServices({
      services,
      sessionManager,
      ...(initial.model ? { model: initial.model } : {}),
      ...(initial.thinkingLevel ? { thinkingLevel: initial.thinkingLevel } : {}),
      ...(initial.scopedModels.length > 0 ? { scopedModels: initial.scopedModels } : {}),
      ...(toolsOption !== undefined ? { tools: toolsOption } : {}),
    });
    // Context compaction is automatic in Pi Desktop. Keep this explicit so
    // sessions inherit the product behavior even if the SDK default changes
    // or a persisted session was created with auto compaction disabled.
    inner.setAutoCompactionEnabled(true);
    configureDesktopProviderRetry(inner);

    const persistedPreferences = await persistExplicitStartupPreferences(
      services.settingsManager,
      {
        ...(initialModel ? { model: initialModel } : {}),
        ...(thinkingLevel ? { thinkingLevel } : {}),
      },
      {
        ...(inner.model
          ? { model: { provider: inner.model.provider, modelId: inner.model.id } }
          : {}),
        thinkingLevel: inner.thinkingLevel,
        supportsThinking: inner.supportsThinking(),
      },
    );
    if (persistedPreferences.modelDefaultChanged) invalidateModelsCache();

    // If specific tool names were requested (non-empty), set the active tools to the
    // requested builtin coding tools PLUS all extension/package tools, so installed
    // extensions stay usable in Pi Desktop just like in the `pi` CLI.
    // 普通对话模式：工具仍全部注册（见 toolsOption），只是没有任何一个处于激活状态，
    // 这样切回工作模式时能按快照恢复。
    const workModeToolNames = chatMode ? inner.getActiveToolNames() : undefined;
    if (chatMode) {
      inner.setActiveToolsByName([]);
    } else if (toolNames && toolNames.length > 0) {
      inner.setActiveToolsByName(withExtensionTools(inner, applyShellTool(toolNames, shellTool)));
    } else if (toolNames === undefined) {
      // Existing sessions are opened without toolNames and inherit pi's bash
      // default (plus PowerShell accidentally surfaced as an extension tool).
      // Normalize that inherited set so an already-created session also recovers.
      const activeToolNames = inner.getActiveToolNames();
      const normalizedToolNames = normalizeActiveShellTool(activeToolNames, shellTool);
      const changed = normalizedToolNames.length !== activeToolNames.length
        || normalizedToolNames.some((name, index) => name !== activeToolNames[index]);
      if (changed) inner.setActiveToolsByName(normalizedToolNames);
    }

    const wrapper = new AgentSessionWrapper(
      inner,
      (locale) => { sessionLocale.value = locale; },
      (force) => { sessionLocale.forceEmpty = force; },
      (on) => { sessionLocale.chatMode = on; },
    );
    // When all tools are disabled, clear the system prompt entirely.
    // pi's buildSystemPrompt always produces a non-empty prompt even with no tools;
    // keep this forced after extension resource discovery and reloads as well.
    if (toolNames?.length === 0) {
      wrapper.setForceEmptySystemPrompt(forceEmpty);
    }
    wrapper.setUiLocale(uiLocale);
    wrapper.setChatMode(chatMode === true, workModeToolNames);
    wrapper.start();

    const realSessionId = inner.sessionId as string;
    const realSessionFile = inner.sessionFile as string | undefined;
    if (realSessionFile) cacheSessionPath(realSessionId, realSessionFile);

    wrapper.onDestroy(() => registry.delete(realSessionId));
    registry.set(realSessionId, wrapper);
    wrapper.beginExtensionBinding({ forceEmptySystemPrompt: forceEmpty });

    return { session: wrapper, realSessionId };
  })().finally(() => {
    locks.delete(sessionId);
    finishStartingSession();
  });

  locks.set(sessionId, starting);
  return starting;
}
