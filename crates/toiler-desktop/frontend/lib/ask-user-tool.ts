import type { ExtensionAPI, InlineExtension } from "@earendil-works/pi-coding-agent";
import { StringEnum } from "@earendil-works/pi-ai";
import { Text } from "@earendil-works/pi-tui";
import { Type } from "typebox";

const ASK_USER_EXTENSION_NAME = "pi-desktop-ask-user";
const MAX_QUESTIONS = 3;
const MAX_OPTIONS = 8;
const MAX_TEXT_LENGTH = 20_000;
const MAX_TIMEOUT_MS = 10 * 60 * 1000;

export type AskUserQuestionKind = "single_select" | "confirm" | "text" | "editor";

export interface AskUserOption {
  value: string;
  label: string;
  description?: string;
}

export interface AskUserQuestion {
  id: string;
  prompt: string;
  kind: AskUserQuestionKind;
  options?: AskUserOption[];
  required?: boolean;
  placeholder?: string;
  defaultValue?: string | boolean;
}

export interface AskUserInput {
  questions: AskUserQuestion[];
  timeoutMs?: number;
}

export type AskUserStatus = "answered" | "cancelled" | "expired" | "aborted";

export interface AskUserAnswer {
  value: string | boolean;
  label?: string;
  custom?: boolean;
}

export interface AskUserResult {
  status: AskUserStatus;
  answers: Record<string, AskUserAnswer>;
}

const OPTION_SCHEMA = Type.Object({
  value: Type.String({ minLength: 1, maxLength: 500, description: "Stable answer value returned to the model" }),
  label: Type.String({ minLength: 1, maxLength: 500, description: "Human-readable option label shown to the user" }),
  description: Type.Optional(Type.String({ maxLength: 1_000, description: "Explain the impact or tradeoff of this option" })),
});

const QUESTION_SCHEMA = Type.Object({
  id: Type.String({ minLength: 1, maxLength: 100, description: "Unique identifier for this question in the current request" }),
  prompt: Type.String({ minLength: 1, maxLength: 4_000, description: "The question shown to the user" }),
  kind: StringEnum(["single_select", "confirm", "text", "editor"] as const),
  options: Type.Array(OPTION_SCHEMA, {
    minItems: 0,
    maxItems: MAX_OPTIONS,
    description: "Options for single_select (2-8 required); use an empty array for confirm, text, and editor questions",
  }),
  required: Type.Optional(Type.Boolean({ description: "Whether the user must provide an answer" })),
  placeholder: Type.Optional(Type.String({ maxLength: 500, description: "Placeholder for text input" })),
  defaultValue: Type.Optional(Type.Union([
    Type.String({ maxLength: MAX_TEXT_LENGTH }),
    Type.Boolean(),
  ])),
});

const ASK_USER_PARAMETERS = Type.Object({
  questions: Type.Array(QUESTION_SCHEMA, {
    minItems: 1,
    maxItems: MAX_QUESTIONS,
    description: "One to three related questions. Put all questions needed for the current clarification in this array so they are presented sequentially in one interaction.",
  }),
  timeoutMs: Type.Optional(Type.Integer({ minimum: 1_000, maximum: MAX_TIMEOUT_MS, description: "Optional overall timeout for the complete questionnaire" })),
});

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function normalizeQuestion(value: unknown, index: number): AskUserQuestion {
  if (!isRecord(value)) throw new Error(`Question ${index + 1} must be an object`);

  const id = typeof value.id === "string" ? value.id.trim() : "";
  const prompt = typeof value.prompt === "string" ? value.prompt.trim() : "";
  const kind = value.kind;
  if (!id) throw new Error(`Question ${index + 1} is missing id`);
  if (!prompt) throw new Error(`Question ${id} is missing prompt`);
  if (typeof kind !== "string" || !["single_select", "confirm", "text", "editor"].includes(kind)) {
    throw new Error(`Question ${id} has an unsupported kind`);
  }

  const question: AskUserQuestion = {
    id,
    prompt,
    kind: kind as AskUserQuestionKind,
    ...(value.required === undefined ? {} : { required: Boolean(value.required) }),
    ...(typeof value.placeholder === "string" ? { placeholder: value.placeholder } : {}),
    ...(typeof value.defaultValue === "string" || typeof value.defaultValue === "boolean"
      ? { defaultValue: value.defaultValue }
      : {}),
  };

  if (kind === "single_select") {
    if (!Array.isArray(value.options) || value.options.length < 2 || value.options.length > MAX_OPTIONS) {
      throw new Error(`Question ${id} must provide between 2 and ${MAX_OPTIONS} options`);
    }
    const options = value.options.map((option, optionIndex) => {
      if (!isRecord(option)) throw new Error(`Question ${id} option ${optionIndex + 1} must be an object`);
      const optionValue = typeof option.value === "string" ? option.value : "";
      const label = typeof option.label === "string" ? option.label : "";
      if (!optionValue || !label) throw new Error(`Question ${id} has an invalid option`);
      return {
        value: optionValue,
        label,
        ...(typeof option.description === "string" ? { description: option.description } : {}),
      };
    });
    if (new Set(options.map((option) => option.value)).size !== options.length) {
      throw new Error(`Question ${id} has duplicate option values`);
    }
    question.options = options;
  } else if (Array.isArray(value.options) && value.options.length === 0) {
    question.options = [];
  } else if (value.options !== undefined) {
    throw new Error(`Question ${id} must use an empty options array for ${kind}`);
  } else {
    question.options = [];
  }

  return question;
}

export function normalizeAskUserInput(value: unknown): AskUserInput {
  if (!isRecord(value) || !Array.isArray(value.questions)) {
    throw new Error("ask_user requires a questions array");
  }
  if (value.questions.length < 1 || value.questions.length > MAX_QUESTIONS) {
    throw new Error(`ask_user accepts between 1 and ${MAX_QUESTIONS} questions`);
  }

  const questions = value.questions.map(normalizeQuestion);
  if (new Set(questions.map((question) => question.id)).size !== questions.length) {
    throw new Error("ask_user question ids must be unique");
  }

  const timeoutMs = typeof value.timeoutMs === "number" && Number.isFinite(value.timeoutMs)
    ? Math.max(1_000, Math.min(MAX_TIMEOUT_MS, Math.floor(value.timeoutMs)))
    : undefined;
  return { questions, ...(timeoutMs === undefined ? {} : { timeoutMs }) };
}

function titleFor(question: AskUserQuestion, index: number, total: number): string {
  return total === 1 ? question.prompt : `[${index + 1}/${total}] ${question.prompt}`;
}

function answerLabel(question: AskUserQuestion, value: string | boolean): string | undefined {
  if (typeof value !== "string") return undefined;
  return question.options?.find((option) => option.value === value)?.label;
}

export function formatAskUserSummary(result: AskUserResult): string {
  if (result.status !== "answered") return `User ${result.status} the questions.`;
  const lines = Object.entries(result.answers).map(([id, answer]) => {
    if (typeof answer.value === "boolean") return `${id}: ${answer.value}`;
    return `${id}: ${answer.label ?? answer.value}`;
  });
  return lines.length > 0 ? `User answered:\n${lines.join("\n")}` : "User answered the questions.";
}

async function executeAskUser(
  params: AskUserInput,
  signal: AbortSignal | undefined,
  ctx: { hasUI?: boolean; ui: {
    select: (title: string, options: string[], opts?: { signal?: AbortSignal; timeout?: number }) => Promise<string | undefined>;
    confirm: (title: string, message: string, opts?: { signal?: AbortSignal; timeout?: number }) => Promise<boolean>;
    input: (title: string, placeholder?: string, opts?: { signal?: AbortSignal; timeout?: number }) => Promise<string | undefined>;
    editor: (title: string, prefill?: string, opts?: { signal?: AbortSignal; timeout?: number }) => Promise<string | undefined>;
  } },
): Promise<AskUserResult> {
  if (ctx.hasUI === false) {
    return { status: "aborted", answers: {} };
  }

  const answers: Record<string, AskUserAnswer> = {};
  const deadline = params.timeoutMs === undefined ? undefined : Date.now() + params.timeoutMs;
  for (const [index, question] of params.questions.entries()) {
    if (signal?.aborted) return { status: "aborted", answers };

    const remaining = deadline === undefined ? undefined : Math.max(1, deadline - Date.now());
    if (remaining !== undefined && remaining <= 1) return { status: "expired", answers };
    const options = question.options ?? [];
    const title = titleFor(question, index, params.questions.length);
    let value: string | boolean | undefined;
    // 自由文本答案：text / editor，以及选择题里用户自己写的那一条。
    let customAnswer = question.kind === "text" || question.kind === "editor";

    if (question.kind === "single_select") {
      const selected = await ctx.ui.select(title, options.map((option) => option.label), {
        ...(signal ? { signal } : {}),
        ...(remaining === undefined ? {} : { timeout: remaining }),
      });
      if (selected === undefined) {
        return signal?.aborted
          ? { status: "aborted", answers }
          : { status: deadline !== undefined && Date.now() >= deadline ? "expired" : "cancelled", answers };
      }
      const selectedOption = options.find((option) => option.label === selected);
      if (selectedOption) {
        value = selectedOption.value;
      } else {
        // UI 永远给选择题多留一个「自定义答案」入口，所以回来的不一定是一个选项标签：
        // 匹配不上就说明用户自己写了答案，这段文本本身就是这一题的答案。
        const typed = selected.trim();
        if (!typed) return { status: "cancelled", answers };
        value = typed;
        customAnswer = true;
      }
    } else if (question.kind === "confirm") {
      value = await ctx.ui.confirm(title, "请确认是否继续。", {
        ...(signal ? { signal } : {}),
        ...(remaining === undefined ? {} : { timeout: remaining }),
      });
      if (signal?.aborted) return { status: "aborted", answers };
    } else if (question.kind === "text") {
      value = await ctx.ui.input(title, question.placeholder, {
        ...(signal ? { signal } : {}),
        ...(remaining === undefined ? {} : { timeout: remaining }),
      });
      if (value === undefined) {
        return signal?.aborted
          ? { status: "aborted", answers }
          : { status: deadline !== undefined && Date.now() >= deadline ? "expired" : "cancelled", answers };
      }
    } else {
      value = await ctx.ui.editor(title, typeof question.defaultValue === "string" ? question.defaultValue : undefined, {
        ...(signal ? { signal } : {}),
        ...(remaining === undefined ? {} : { timeout: remaining }),
      });
      if (value === undefined) {
        return signal?.aborted
          ? { status: "aborted", answers }
          : { status: deadline !== undefined && Date.now() >= deadline ? "expired" : "cancelled", answers };
      }
    }

    if (typeof value === "string" && value.length > MAX_TEXT_LENGTH) {
      return { status: "aborted", answers };
    }
    answers[question.id] = {
      value,
      ...(answerLabel(question, value) ? { label: answerLabel(question, value) } : {}),
      ...(customAnswer ? { custom: true } : {}),
    };
  }

  return { status: "answered", answers };
}

export function createAskUserExtension(): InlineExtension {
  return {
    name: ASK_USER_EXTENSION_NAME,
    hidden: true,
    factory: (pi: ExtensionAPI) => {
      pi.registerTool({
        name: "ask_user",
        label: "Ask User",
        description: "Ask the user for non-sensitive information needed to continue. Use one call with 1-3 related questions; all questions in the questions array are shown sequentially. Do not use for passwords, tokens, private keys, passphrases, OTP codes, or other secrets.",
        promptSnippet: "ask the user for non-sensitive clarification, preferences, or confirmation",
        promptGuidelines: [
          "Use ask_user only when missing information would materially affect the result and cannot be discovered safely from the project.",
          "When you need multiple related answers, make one ask_user call and put every question in its questions array; the UI will present them one by one. Do not stop after the first question or ask the user to format tool arguments.",
          "Each question must have a unique id, a clear prompt, and the correct kind. Use single_select with 2-8 concrete options for choices. For confirm, text, and editor, set options to an empty array. Use confirm for yes/no decisions, text for short values, and editor for multi-line input.",
          "ask_user answers are visible to the model and may be saved in the session; never request passwords, tokens, private keys, passphrases, OTP codes, or other secrets. When Git or SSH needs credentials, run the normal command and let Pi Desktop show its secure credential prompt instead.",
          "Ask at most three related questions at a time and make option tradeoffs explicit in each option description.",
          "Every single_select also offers a free-form answer, so an answer may be text you did not list; treat it as the user's choice even when it matches no option value.",
        ],
        parameters: ASK_USER_PARAMETERS,
        executionMode: "sequential",
        async execute(_toolCallId, rawParams, signal, _onUpdate, ctx) {
          const params = normalizeAskUserInput(rawParams);
          const result = await executeAskUser(params, signal, ctx);
          return {
            content: [{ type: "text", text: formatAskUserSummary(result) }],
            details: result,
          };
        },
        renderCall(args, theme) {
          const questions = Array.isArray(args.questions) ? args.questions : [];
          return new Text(
            theme.fg("toolTitle", theme.bold("ask_user "))
              + theme.fg("muted", `${questions.length} question${questions.length === 1 ? "" : "s"}`),
            0,
            0,
          );
        },
        renderResult(result, _options, theme) {
          const details = result.details as AskUserResult | undefined;
          if (!details) return new Text("", 0, 0);
          if (details.status === "answered") return new Text(theme.fg("success", "User answered"), 0, 0);
          return new Text(theme.fg("warning", `User ${details.status}`), 0, 0);
        },
      });
    },
  };
}
