import type {
  AgentSessionEvent,
  BashOperations,
  SessionManager,
  SettingsManager,
  SlashCommandInfo,
  Theme,
} from "@earendil-works/pi-coding-agent";
import type { AgentMessage as PiAgentMessage } from "@earendil-works/pi-agent-core";

export interface ContextUsage {
  percent: number | null;
  contextWindow: number;
  tokens: number | null;
}

export interface ModelLike {
  id: string;
  provider: string;
}

export interface ToolInfo {
  name: string;
  description: string;
}

export interface NavigateTreeResult {
  editorText?: string;
  cancelled: boolean;
  aborted?: boolean;
}

export interface SessionStatsInfo {
  sessionFile?: string;
  sessionId: string;
  sessionName?: string;
  userMessages: number;
  assistantMessages: number;
  toolCalls: number;
  toolResults: number;
  totalMessages: number;
  tokens: {
    input: number;
    output: number;
    cacheRead: number;
    cacheWrite: number;
    total: number;
  };
  cost: number;
  contextUsage?: ContextUsage;
  /** Estimated active time across all entries in the session file. */
  totalActiveMs?: number;
}

interface PromptTemplateLike {
  name: string;
  description?: string;
  sourceInfo: SlashCommandInfo["sourceInfo"];
}

interface SkillLike {
  name: string;
  filePath: string;
  description?: string;
  sourceInfo: SlashCommandInfo["sourceInfo"];
}

interface ResourceLoaderLike {
  getSkills(): { skills: SkillLike[] };
}

interface ExtensionRunnerLike {
  getRegisteredCommands(): Array<{
    invocationName: string;
    description?: string;
    sourceInfo: SlashCommandInfo["sourceInfo"];
  }>;
  emit?(event: { type: "session_shutdown"; reason: "quit" }): Promise<unknown>;
  setUIContext?(uiContext?: unknown, mode?: "tui" | "rpc" | "json" | "print"): void;
}

type DialogOptionsLike = {
  signal?: AbortSignal;
  timeout?: number;
  sensitive?: boolean;
};

type WidgetOptionsLike = {
  placement?: "aboveEditor" | "belowEditor";
};

export interface ExtensionUiContextLike {
  select(title: string, options: string[], opts?: DialogOptionsLike): Promise<string | undefined>;
  confirm(title: string, message: string, opts?: DialogOptionsLike): Promise<boolean>;
  input(title: string, placeholder?: string, opts?: DialogOptionsLike): Promise<string | undefined>;
  editor(title: string, prefill?: string, opts?: DialogOptionsLike): Promise<string | undefined>;
  notify(message: string, type?: "info" | "warning" | "error"): void;
  onTerminalInput(): () => void;
  setStatus(key: string, text: string | undefined): void;
  setWorkingMessage(message?: string): void;
  setWorkingVisible(visible: boolean): void;
  setWorkingIndicator(options?: { frames?: string[]; intervalMs?: number }): void;
  setHiddenThinkingLabel(label?: string): void;
  setWidget(key: string, content: string[] | ((...args: never[]) => unknown) | undefined, options?: WidgetOptionsLike): void;
  setFooter(factory: unknown): void;
  setHeader(factory: unknown): void;
  setTitle(title: string): void;
  custom<T = unknown>(...args: unknown[]): Promise<T>;
  pasteToEditor(text: string): void;
  setEditorText(text: string): void;
  getEditorText(): string;
  addAutocompleteProvider(): void;
  setEditorComponent(): void;
  getEditorComponent(): undefined;
  readonly theme: Theme;
  getAllThemes(): unknown[];
  getTheme(name: string): undefined;
  setTheme(theme: unknown): { success: boolean; error?: string };
  getToolsExpanded(): boolean;
  setToolsExpanded(expanded: boolean): void;
}

export interface AgentSessionLike {
  readonly sessionId: string;
  readonly sessionFile: string | undefined;
  readonly isStreaming: boolean;
  readonly isCompacting: boolean;
  readonly autoCompactionEnabled: boolean;
  readonly autoRetryEnabled: boolean;
  readonly model: ModelLike | undefined;
  readonly modelRuntime: {
    getModel: (provider: string, modelId: string) => ModelLike | undefined;
    refresh: (options?: { allowNetwork?: boolean }) => Promise<unknown>;
  };
  readonly sessionManager: SessionManager;
  readonly settingsManager: SettingsManager;
  readonly agent: {
    state?: {
      /**
       * Replayed from the transcript's leading system message since pi 0.86 and
       * exposed as a getter-only property. Read it; assigning throws at runtime.
       * The effective prompt for display lives in `AgentSessionWrapper.effectiveSystemPrompt()`.
       */
      readonly systemPrompt?: string;
      thinkingLevel?: string;
      streamingMessage?: PiAgentMessage;
      /**
       * Executable tools for the next request. `read` being absent means the prompt
       * must keep `<project_context>` inline: a path the model cannot open is worse
       * than the tokens (`lib/project-context-digest.ts`).
       */
      tools?: readonly { readonly name: string }[];
      /**
       * Agent-state transcript. A just-accepted prompt lives here before its
       * `message_end` appends it to the SessionManager, so the session list
       * reads it as the first-message fallback.
       */
      messages?: PiAgentMessage[];
    };
  };
  readonly extensionRunner: ExtensionRunnerLike;
  readonly promptTemplates: readonly PromptTemplateLike[];
  readonly resourceLoader: ResourceLoaderLike;

  readonly bindExtensions?: unknown;
  dispose(): void;
  reload(options?: { beforeSessionStart?: () => void | Promise<void> }): Promise<void>;
  subscribe(listener: (event: AgentSessionEvent) => void): () => void;
  prompt(text: string, options?: {
    expandPromptTemplates?: boolean;
    images?: Array<{ type: "image"; data: string; mimeType: string }>;
    streamingBehavior?: "steer" | "followUp";
    source?: "interactive" | "rpc" | "extension";
    /**
     * Since pi 1.0 this reports how an accepted prompt was dispatched
     * (`"handled"` = an extension command ran, `"queued"` = joined the
     * streaming queue, `"started"` = a run began). Rejections no longer call it
     * with `false`; they reject the returned promise instead.
     */
    preflightResult?: (disposition: "handled" | "queued" | "started") => void;
  }): Promise<void>;
  abort(): Promise<void>;
  executeBash(command: string, onChunk?: (chunk: string) => void, options?: {
    excludeFromContext?: boolean;
    operations?: BashOperations;
  }): Promise<{ output: string; exitCode?: number; cancelled?: boolean; truncated?: boolean; fullOutputPath?: string }>;
  abortBash(): void;
  readonly isBashRunning: boolean;
  setModel(model: ModelLike): Promise<void>;
  navigateTree(targetId: string, options?: { summarize?: boolean }): Promise<NavigateTreeResult>;
  setThinkingLevel(level: string): void;
  compact(customInstructions?: string): Promise<unknown>;
  setSessionName(name: string): void;
  getSessionStats(): Omit<SessionStatsInfo, "sessionName">;
  getLastAssistantText(): string | undefined;
  setAutoCompactionEnabled(enabled: boolean): void;
  setAutoRetryEnabled(enabled: boolean): void;
  /** pi 1.0 起 steer/followUp 回报 "handled" | "queued"。 */
  steer(text: string, images?: Array<{ type: "image"; data: string; mimeType: string }>, options?: {
    source?: "interactive" | "rpc" | "extension";
  }): Promise<"handled" | "queued">;
  followUp(text: string, images?: Array<{ type: "image"; data: string; mimeType: string }>, options?: {
    source?: "interactive" | "rpc" | "extension";
  }): Promise<"handled" | "queued">;
  readonly pendingMessageCount: number;
  /**
   * 引导/续跑队列的投递模式（pi 自己的 `steeringMode` / `followUpMode`）。
   * 结构类型不会把 SDK 的真实签名带进来，遗漏也不会被 tsc 发现，所以两边都声明。
   */
  setSteeringMode?(mode: "all" | "one-at-a-time"): void;
  setFollowUpMode?(mode: "all" | "one-at-a-time"): void;
  getSteeringMessages(): readonly string[];
  getFollowUpMessages(): readonly string[];
  clearQueue(): { steering: string[]; followUp: string[] };
  getAllTools(): ToolInfo[];
  getActiveToolNames(): string[];
  setActiveToolsByName(names: string[]): void;
  abortCompaction(): void;
  getContextUsage(): ContextUsage | undefined;
}
