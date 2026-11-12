import type { ExtensionAPI, ExtensionContext, InlineExtension } from "@earendil-works/pi-coding-agent";
import { StringEnum } from "@earendil-works/pi-ai";
import { Text } from "@earendil-works/pi-tui";
import { Type } from "typebox";

const TASK_PROGRESS_EXTENSION_NAME = "pi-desktop-task-progress";
const TASK_WIDGET_KEY = "任务进度";
const TASK_STATUS_KEY = "task-progress";
const MAX_TASKS = 20;
const MAX_TEXT_LENGTH = 2_000;

export type TaskProgressStatus = "pending" | "in_progress" | "completed" | "failed" | "blocked" | "skipped";

export interface TaskProgressItem {
  id: string;
  title: string;
  status: TaskProgressStatus;
  detail?: string;
}

export interface TaskProgressState {
  tasks: TaskProgressItem[];
}

export type TaskProgressInput =
  | { action: "replace"; tasks: TaskProgressItem[] }
  | { action: "update"; id: string; title?: string; status?: TaskProgressStatus; detail?: string }
  | { action: "clear" };

const STATUS_SCHEMA = StringEnum([
  "pending",
  "in_progress",
  "completed",
  "failed",
  "blocked",
  "skipped",
] as const);

const ACTION_SCHEMA = StringEnum(["replace", "update", "clear"] as const);

const TASK_SCHEMA = Type.Object({
  id: Type.String({ minLength: 1, maxLength: 100, description: "Stable task identifier" }),
  title: Type.String({ minLength: 1, maxLength: 500, description: "Short user-facing task title" }),
  status: STATUS_SCHEMA,
  detail: Type.Optional(Type.String({ maxLength: MAX_TEXT_LENGTH, description: "Optional current activity, result, failure, or blocking reason" })),
});

// Some OpenAI-compatible providers reject a top-level anyOf tool schema.
// Keep the root an object and enforce action-specific requirements at runtime.
const TASK_PROGRESS_PARAMETERS = Type.Object({
  action: ACTION_SCHEMA,
  tasks: Type.Optional(Type.Array(TASK_SCHEMA, {
    minItems: 1,
    maxItems: MAX_TASKS,
    description: "Required for replace: the complete ordered task list. Existing tasks are replaced atomically.",
  })),
  id: Type.Optional(Type.String({ minLength: 1, maxLength: 100, description: "Required for update: existing task identifier" })),
  title: Type.Optional(Type.String({ minLength: 1, maxLength: 500 })),
  status: Type.Optional(STATUS_SCHEMA),
  detail: Type.Optional(Type.String({ maxLength: MAX_TEXT_LENGTH })),
});

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function normalizeText(value: unknown, field: string, maxLength: number): string {
  if (typeof value !== "string") throw new Error(`${field} must be a string`);
  const text = value.trim();
  if (!text) throw new Error(`${field} must not be empty`);
  if (text.length > maxLength) throw new Error(`${field} is too long`);
  return text;
}

function normalizeStatus(value: unknown): TaskProgressStatus {
  if (typeof value !== "string" || ![
    "pending",
    "in_progress",
    "completed",
    "failed",
    "blocked",
    "skipped",
  ].includes(value)) {
    throw new Error("task status is invalid");
  }
  return value as TaskProgressStatus;
}

function normalizeTask(value: unknown, index: number): TaskProgressItem {
  if (!isRecord(value)) throw new Error(`Task ${index + 1} must be an object`);
  return {
    id: normalizeText(value.id, `Task ${index + 1} id`, 100),
    title: normalizeText(value.title, `Task ${index + 1} title`, 500),
    status: normalizeStatus(value.status),
    ...(typeof value.detail === "string" && value.detail.trim()
      ? { detail: normalizeText(value.detail, `Task ${index + 1} detail`, MAX_TEXT_LENGTH) }
      : {}),
  };
}

export function normalizeTaskProgressInput(value: unknown): TaskProgressInput {
  if (!isRecord(value) || typeof value.action !== "string") {
    throw new Error("task_progress requires an action");
  }
  if (value.action === "clear") return { action: "clear" };
  if (value.action === "replace") {
    if (!Array.isArray(value.tasks) || value.tasks.length < 1 || value.tasks.length > MAX_TASKS) {
      throw new Error(`task_progress replace requires between 1 and ${MAX_TASKS} tasks`);
    }
    const tasks = value.tasks.map(normalizeTask);
    if (new Set(tasks.map((task) => task.id)).size !== tasks.length) {
      throw new Error("task ids must be unique");
    }
    if (tasks.filter((task) => task.status === "in_progress").length > 1) {
      throw new Error("only one task may be in_progress");
    }
    return { action: "replace", tasks };
  }
  if (value.action === "update") {
    const id = normalizeText(value.id, "task id", 100);
    const title = value.title === undefined ? undefined : normalizeText(value.title, "task title", 500);
    const status = value.status === undefined ? undefined : normalizeStatus(value.status);
    const detail = value.detail === undefined ? undefined : normalizeText(value.detail, "task detail", MAX_TEXT_LENGTH);
    if (title === undefined && status === undefined && detail === undefined) {
      throw new Error("task_progress update requires title, status, or detail");
    }
    return {
      action: "update",
      id,
      ...(title === undefined ? {} : { title }),
      ...(status === undefined ? {} : { status }),
      ...(detail === undefined ? {} : { detail }),
    };
  }
  throw new Error("task_progress action is invalid");
}

function statusSymbol(status: TaskProgressStatus): string {
  switch (status) {
    case "in_progress": return "●";
    case "completed": return "✓";
    case "failed": return "✕";
    case "blocked": return "!";
    case "skipped": return "−";
    default: return "○";
  }
}

export function formatTaskProgressLines(tasks: TaskProgressItem[]): string[] {
  if (tasks.length === 0) return [];
  const completed = tasks.filter((task) => task.status === "completed" || task.status === "skipped").length;
  return [
    `已完成 ${completed}/${tasks.length}`,
    ...tasks.map((task) => `${statusSymbol(task.status)} ${task.title}${task.detail ? ` — ${task.detail}` : ""}`),
  ];
}

function updateTaskProgressUi(ctx: ExtensionContext, tasks: TaskProgressItem[]): void {
  const finished = tasks.length > 0 && tasks.every((task) => task.status === "completed" || task.status === "skipped");
  if (tasks.length === 0 || finished) {
    ctx.ui.setWidget(TASK_WIDGET_KEY, undefined);
    ctx.ui.setStatus(TASK_STATUS_KEY, undefined);
    return;
  }

  const completed = tasks.filter((task) => task.status === "completed" || task.status === "skipped").length;
  const active = tasks.find((task) => task.status === "in_progress");
  const blocked = tasks.find((task) => task.status === "blocked");
  const failed = tasks.find((task) => task.status === "failed");
  const suffix = active
    ? ` · 正在执行：${active.title}`
    : blocked
      ? ` · 已阻塞：${blocked.title}`
      : failed
        ? ` · 执行失败：${failed.title}`
        : completed === tasks.length
          ? " · 已完成"
          : "";

  ctx.ui.setStatus(TASK_STATUS_KEY, `任务 ${completed}/${tasks.length}${suffix}`);
  ctx.ui.setWidget(TASK_WIDGET_KEY, formatTaskProgressLines(tasks), { placement: "aboveEditor" });
}

function reconstructTaskProgress(ctx: ExtensionContext): TaskProgressItem[] {
  let tasks: TaskProgressItem[] = [];
  for (const entry of ctx.sessionManager.getBranch()) {
    if (entry.type !== "message") continue;
    const message = entry.message;
    if (message.role !== "toolResult" || message.toolName !== "task_progress") continue;
    const details = message.details as TaskProgressState | undefined;
    if (Array.isArray(details?.tasks)) tasks = details.tasks;
  }
  return tasks;
}

export function createTaskProgressExtension(): InlineExtension {
  return {
    name: TASK_PROGRESS_EXTENSION_NAME,
    hidden: true,
    factory: (pi: ExtensionAPI) => {
      let tasks: TaskProgressItem[] = [];

      const restore = (ctx: ExtensionContext) => {
        tasks = reconstructTaskProgress(ctx);
        updateTaskProgressUi(ctx, tasks);
      };

      pi.on("session_start", async (_event, ctx) => restore(ctx));
      pi.on("session_tree", async (_event, ctx) => restore(ctx));

      pi.registerTool({
        name: "task_progress",
        label: "Task Progress",
        description: "Create and update the structured task list shown in the Pi Desktop interface. Use it for substantial multi-step work so the user can see each task's live state.",
        promptSnippet: "create and update the Desktop task list with pending, in-progress, completed, failed, blocked, or skipped states",
        promptGuidelines: [
          "Use task_progress for substantial work with multiple distinct steps; do not create a task list for a simple single-step request.",
          "Before starting multi-step work, call task_progress with action=replace and a concise ordered list. Keep stable ids and include the complete list when replacing it.",
          "Immediately before working on a task, update it to in_progress. Keep at most one task in_progress at a time.",
          "Mark a task completed only after its required work and verification are finished. Mark failures as failed with the concrete reason, external dependencies as blocked, and intentionally omitted work as skipped.",
          "Update task_progress whenever execution state changes, not only in the final answer. Before finishing, ensure no task is incorrectly left in_progress. The Desktop automatically hides the task list after every task is completed or skipped.",
        ],
        parameters: TASK_PROGRESS_PARAMETERS,
        executionMode: "sequential",
        async execute(_toolCallId, rawParams, _signal, _onUpdate, ctx) {
          const params = normalizeTaskProgressInput(rawParams);
          if (params.action === "clear") {
            tasks = [];
          } else if (params.action === "replace") {
            tasks = params.tasks;
          } else {
            const index = tasks.findIndex((task) => task.id === params.id);
            if (index < 0) throw new Error(`Task ${params.id} does not exist; use action=replace to create tasks`);
            if (params.status === "in_progress" && tasks.some((task, taskIndex) => taskIndex !== index && task.status === "in_progress")) {
              throw new Error("only one task may be in_progress");
            }
            const current = tasks[index];
            tasks = tasks.map((task, taskIndex) => {
              if (taskIndex !== index) return task;
              const updated: TaskProgressItem = {
                ...current,
                ...(params.title === undefined ? {} : { title: params.title }),
                ...(params.status === undefined ? {} : { status: params.status }),
                ...(params.detail === undefined ? {} : { detail: params.detail }),
              };
              if (params.status !== undefined && params.detail === undefined) delete updated.detail;
              return updated;
            });
          }

          updateTaskProgressUi(ctx, tasks);
          const state: TaskProgressState = { tasks: tasks.map((task) => ({ ...task })) };
          return {
            content: [{ type: "text", text: tasks.length === 0 ? "Task progress cleared." : formatTaskProgressLines(tasks).join("\n") }],
            details: state,
          };
        },
        renderCall(args, theme) {
          return new Text(
            theme.fg("toolTitle", theme.bold("task_progress "))
              + theme.fg("muted", typeof args.action === "string" ? args.action : "update"),
            0,
            0,
          );
        },
        renderResult(result, _options, theme) {
          const state = result.details as TaskProgressState | undefined;
          const count = state?.tasks.length ?? 0;
          return new Text(theme.fg("success", count === 0 ? "Task list cleared" : `Task list updated (${count})`), 0, 0);
        },
      });
    },
  };
}
