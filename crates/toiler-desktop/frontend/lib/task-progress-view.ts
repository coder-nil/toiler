/**
 * `task_progress` 的读取与归一：把两个来源折成一份 UI 可以直接渲染的任务数组。
 *
 * | 来源 | 何时有 | 形状 |
 * |---|---|---|
 * | 活会话的 widget | agent 正在跑（SSE `setWidget` + `get_state.extensionWidgets`） | 文本行 |
 * | 会话里的工具结果 | 永远有（v3 文件里逐条存着） | `details.tasks` |
 *
 * 文本行的格式由 `crates/toiler-server/src/desktop_tools/task_progress.rs::presentation`
 * 决定：首行 `已完成 3/8`，其后每行 `{符号} {标题}` 或 `{符号} {标题} — {细节}`。
 * 符号：`○` 待办、`●` 进行中、`✓` 已完成、`✕` 失败、`!` 阻塞、`−` 跳过。
 *
 * 为什么两条路都要：widget 只存在于活会话。刷新界面或打开历史会话时后端不会重放
 * 事件，只看 widget 的话任务清单会凭空消失 —— 而 `details` 一直在会话文件里。
 *
 * 「进行中」游标由程序自己维护（[`advanceActiveTask`]），与后端
 * `desktop_tools::task_progress::advance` 同一套规则：模型经常一步到位把某条写成
 * `completed`、跳过 `in_progress`，界面就会停在「还有 2 项未完成」却说不出在做哪一条。
 */

import type { AgentMessage, ExtensionWidgetItem } from "./types";

/** 与后端 `WIDGET_KEY` / `STATUS_KEY` 对齐。 */
export const TASK_PROGRESS_WIDGET_KEY = "任务进度";
export const TASK_PROGRESS_STATUS_KEY = "task-progress";
export const TASK_PROGRESS_TOOL_NAME = "task_progress";

export type TaskStatus =
  | "pending"
  | "in_progress"
  | "completed"
  | "failed"
  | "blocked"
  | "skipped";

export interface TaskProgressEntry {
  /** widget 行里没有 id，按位置生成（列表顺序由后端固定，不会乱跳）。 */
  id: string;
  title: string;
  status: TaskStatus;
  detail?: string;
}

export const TASK_STATUS_VALUES: readonly TaskStatus[] = [
  "pending",
  "in_progress",
  "completed",
  "failed",
  "blocked",
  "skipped",
];

/** 这些状态算「已收尾」。 */
const FINISHED_STATUSES: ReadonlySet<TaskStatus> = new Set<TaskStatus>(["completed", "skipped"]);

const STATUS_BY_SYMBOL: Record<string, TaskStatus> = {
  "○": "pending",
  "●": "in_progress",
  "✓": "completed",
  "✕": "failed",
  "!": "blocked",
  "−": "skipped",
  "-": "skipped",
};

const SUMMARY_PATTERN = /^已完成\s*(\d+)\s*\/\s*(\d+)$/u;
const LINE_PATTERN = /^(\S)\s+(\S.*)$/u;
/** 后端用 em dash 连接标题与细节。 */
const DETAIL_SEPARATOR = /\s+[—–]\s+/u;

function isTaskStatus(value: unknown): value is TaskStatus {
  return typeof value === "string" && (TASK_STATUS_VALUES as readonly string[]).includes(value);
}

/**
 * 解析 widget 文本行。
 * @returns 不是任务 widget 时返回 `null`（好让调用方回落到工具结果）
 */
export function parseTaskWidgetLines(lines: readonly string[]): TaskProgressEntry[] | null {
  if (lines.length === 0) return null;
  if (!SUMMARY_PATTERN.test(lines[0]?.trim() ?? "")) return null;

  const tasks: TaskProgressEntry[] = [];
  const body = lines.slice(1);
  for (let index = 0; index < body.length; index += 1) {
    const line = body[index]?.trim() ?? "";
    const match = LINE_PATTERN.exec(line);
    if (!match) continue;
    const status = STATUS_BY_SYMBOL[match[1] ?? ""];
    if (!status) continue;
    const rest = match[2] ?? "";
    const separator = DETAIL_SEPARATOR.exec(rest);
    const title = (separator ? rest.slice(0, separator.index) : rest).trim();
    if (!title) continue;
    const detail = separator ? rest.slice(separator.index + separator[0].length).trim() : "";
    tasks.push({
      id: `widget-${index}`,
      title,
      status,
      ...(detail ? { detail } : {}),
    });
  }
  return tasks;
}

/**
 * 从工具结果的 `details` 里取任务数组。
 * @returns 形状不认识时返回 `null`
 */
export function tasksFromDetails(details: unknown): TaskProgressEntry[] | null {
  if (!details || typeof details !== "object") return null;
  const raw = (details as { tasks?: unknown }).tasks;
  if (!Array.isArray(raw)) return null;

  const tasks: TaskProgressEntry[] = [];
  for (const [index, item] of raw.entries()) {
    if (!item || typeof item !== "object") continue;
    const record = item as { id?: unknown; title?: unknown; status?: unknown; detail?: unknown };
    if (typeof record.title !== "string") continue;
    if (!isTaskStatus(record.status)) continue;
    const id = typeof record.id === "string" && record.id ? record.id : `detail-${index}`;
    const detail = typeof record.detail === "string" && record.detail.trim() ? record.detail.trim() : undefined;
    tasks.push({ id, title: record.title, status: record.status, ...(detail ? { detail } : {}) });
  }
  return tasks;
}

/**
 * 会话里最后一条 `task_progress` 结果就是当前清单（`clear` 会写成空数组）。
 * 从后往前找而不是从前往后，避免把已作废的中间状态当成现状。
 */
export function tasksFromMessages(messages: readonly AgentMessage[]): TaskProgressEntry[] {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message?.role !== "toolResult" || message.toolName !== TASK_PROGRESS_TOOL_NAME) continue;
    const tasks = tasksFromDetails(message.details);
    if (tasks) return tasks;
  }
  return [];
}

/** 活会话优先（更新更及时），否则回落到会话记录。 */
export function resolveTaskProgress(
  widgets: readonly ExtensionWidgetItem[],
  messages: readonly AgentMessage[],
): TaskProgressEntry[] {
  const widget = widgets.find((item) => item.key === TASK_PROGRESS_WIDGET_KEY);
  if (widget) {
    const parsed = parseTaskWidgetLines(widget.lines);
    if (parsed) return advanceActiveTask(parsed);
  }
  return advanceActiveTask(tasksFromMessages(messages));
}

/**
 * 推进「进行中」游标：完成一条之后，下一条就是进行中的。
 *
 * 后端 `desktop_tools::task_progress::advance` 做同一件事，两边必须一致：
 * 活会话收到的是已经推进过的 widget（这里幂等），历史会话只能从工具结果的
 * `details` 里自己补位 —— 那些 details 就是模型跳过 `in_progress` 写出来的。
 *
 * 规则：已有 `in_progress` 不动；有 `blocked` / `failed` 不动（卡住时不能
 * 假装在推进）；否则把第一条 `pending` 提上来；全部收尾时不动，让
 * `summarizeTaskProgress().finished` 把面板收掉。
 */
export function advanceActiveTask(tasks: TaskProgressEntry[]): TaskProgressEntry[] {
  if (tasks.length === 0) return tasks;
  if (tasks.some((task) => task.status === "in_progress")) return tasks;
  if (tasks.some((task) => task.status === "blocked" || task.status === "failed")) return tasks;
  const index = tasks.findIndex((task) => task.status === "pending");
  if (index < 0) return tasks;
  const next = [...tasks];
  next[index] = { ...next[index], status: "in_progress" };
  return next;
}

export interface TaskProgressSummary {
  completed: number;
  total: number;
  /** 0–1，用于进度条宽度。 */
  ratio: number;
  /** 进行中的那条（没有则 undefined）。 */
  active?: TaskProgressEntry;
  /** 全部收尾；后端这时会主动收掉 widget。 */
  finished: boolean;
}

export function summarizeTaskProgress(tasks: readonly TaskProgressEntry[]): TaskProgressSummary {
  const total = tasks.length;
  const completed = tasks.filter((task) => FINISHED_STATUSES.has(task.status)).length;
  return {
    completed,
    total,
    ratio: total === 0 ? 0 : completed / total,
    active: tasks.find((task) => task.status === "in_progress"),
    finished: total > 0 && completed === total,
  };
}
