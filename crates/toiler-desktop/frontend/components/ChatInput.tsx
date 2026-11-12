"use client";

import React, {forwardRef, KeyboardEvent, useCallback, useEffect, useImperativeHandle, useLayoutEffect, useRef, useState} from "react";
import type {BuiltinSlashCommandResult, CompactResultInfo, QueuedMessages, SlashCommandInfo} from "@/hooks/useAgentSession";
import type {SkillInfo, SkillsResponse} from "@/lib/api-types";
import type {TextContent, UserMessage} from "@/lib/types";
import {type ChatDraftCommand, type ChatDraftCommandKind, type ChatDraftImage, type ChatDraftMention, clearDraft, getDraft, mergeRestoredSubmissionDraft, mergeRestoredSubmissionText, rekeyDraft as rekeyStoredDraft, setDraft,} from "@/lib/draft-store";
import {isBase64ImageWithinLimits, MAX_ATTACHED_IMAGE_BYTES, MAX_ATTACHED_IMAGES,} from "@/lib/image-attachments";
import {type AtQueryMatch, buildAtInsertText, buildEntriesFromFiles, extractAtQuery, type FileIndexEntry, filterFileEntries,} from "@/lib/file-fuzzy";
import {ChevronDown} from "lucide-react";
import {FolderIcon, getFileIcon} from "./FileIcons";
import {useIsMobile} from "@/hooks/useIsMobile";
import {useI18n} from "@/hooks/useI18n";
import {useChatAppearance} from "@/hooks/useChatAppearance";
import {MobilePairDialog} from "./MobilePairDialog";
import type {ToolPreset} from "@/lib/tool-presets";

export interface AttachedImage {
  data: string;   // base64, no prefix
  mimeType: string;
  previewUrl: string; // object URL for display
}

interface ModelOption {
  provider: string;
  modelId: string;
  name: string;
}

interface Props {
  onSend: (message: string, images?: AttachedImage[]) => void;
  onAbort: () => void;
  onSteer?: (message: string, images?: AttachedImage[]) => void;
  onFollowUp?: (message: string, images?: AttachedImage[]) => void;
  onPromptWithStreamingBehavior?: (message: string, behavior: "steer" | "followUp", images?: AttachedImage[]) => void;
  isStreaming: boolean;
  model?: { provider: string; modelId: string } | null;
  isAutoModelSelection?: boolean;
  modelNames?: Record<string, string>;
  modelList?: { id: string; name: string; provider: string }[];
  modelError?: string | null;
  /** Diagnostics from resolving `enabledModels`, e.g. a pattern that matched nothing. */
  modelScopeWarnings?: string[];
  onModelChange?: (provider: string, modelId: string) => void;
  modelSwitching?: boolean;
  compactError?: string | null;
  compactResult?: CompactResultInfo | null;
  toolPreset?: ToolPreset;
  onToolPresetChange?: (preset: ToolPreset) => void;
  /** 普通对话模式：π 图标切换，不使用任何工具。 */
  chatMode?: "work" | "chat";
  onChatModeChange?: (mode: "work" | "chat") => void;
  thinkingLevel?: "auto" | "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max";
  onThinkingLevelChange?: (level: "auto" | "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max") => void;
  availableThinkingLevels?: string[] | null;
  thinkingLevelMap?: Record<string, string | null> | null;
  retryInfo?: { attempt: number; maxAttempts: number; errorMessage?: string } | null;
  promptFailure?: string | null;
  onRetryPrompt?: () => void;
  queuedMessages?: QueuedMessages | null;
  inputHistory?: string[];
  onRecallQueue?: () => void;
  slashCommands?: SlashCommandInfo[];
  slashCommandsLoading?: boolean;
  onLoadSlashCommands?: () => Promise<SlashCommandInfo[]> | SlashCommandInfo[];
  onBuiltinCommand?: (message: string) => Promise<BuiltinSlashCommandResult>;
  onAudioUnlock?: () => void;
  onOpenConsole?: () => void;
  consoleActive?: boolean;
  draftKey?: string;
  /** Session working directory — enables the @ file autocomplete menu */
  cwd?: string | null;
  /** 当前会话 id，用于生成手机遥控二维码。 */
  sessionId?: string;
  /** 对话向上滚动离开底部时，在输入框上沿拱出「回到最新」。 */
  showJumpToLatest?: boolean;
  onJumpToLatest?: () => void;
}

export interface ChatInputHandle {
  insertText: (text: string) => void;
  insertIfEmpty: (text: string) => void;
  replaceMessage: (message: UserMessage) => void;
  prependText: (text: string) => void;
  addImages: (files: File[]) => void;
  rekeyDraft: (previousKey: string, nextKey: string) => void;
  restoreSubmission: (text: string, images?: ChatDraftImage[], targetDraftKey?: string) => void;
}

const TOOL_PRESETS = ["off", "read-only", "default", "full"] as const;
type ToolPresetLabel = typeof TOOL_PRESETS[number];
const TOOL_PRESET_MAP: Record<ToolPresetLabel, ToolPreset> = {
  off: "none",
  "read-only": "read-only",
  default: "default",
  full: "full",
};
const COMPOSITION_END_ENTER_GRACE_MS = 100;
const MODEL_FILTER_THRESHOLD = 8;
const MODEL_OPTION_COLLATOR = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });
const ANCHORED_MENU_GAP = 8;
const MACOS_ARROW_FUNCTION_KEY_PATTERN = /[\uF700-\uF703]/g;
type QueueSendMode = "steer" | "followup";

export function stripMacOSArrowFunctionKeys(value: string): string {
  return value.replace(MACOS_ARROW_FUNCTION_KEY_PATTERN, "");
}

export function getQueueShortcutMode(event: {
  altKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  code: string;
  key: string;
}): QueueSendMode | null {
  if (!event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return null;
  if (event.code === "KeyF" || event.key.toLowerCase() === "f") return "steer";
  if (event.code === "KeyQ" || event.key.toLowerCase() === "q") return "followup";
  return null;
}

export function getUpwardMenuMaxHeight(menuBottom: number, visibleTop: number, gap = ANCHORED_MENU_GAP): number {
  return Math.max(0, Math.floor(menuBottom - visibleTop - gap));
}

function getVisibleTopBoundary(element: HTMLElement): number {
  let visibleTop = window.visualViewport?.offsetTop ?? 0;

  for (let parent = element.parentElement; parent; parent = parent.parentElement) {
    const overflowY = window.getComputedStyle(parent).overflowY;
    if (overflowY === "auto" || overflowY === "scroll" || overflowY === "hidden" || overflowY === "clip") {
      visibleTop = Math.max(visibleTop, parent.getBoundingClientRect().top + parent.clientTop);
    }
  }

  return visibleTop;
}

/**
 * The composer field is the scroll container (the scrollbar has to sit on the
 * outer edge), so the textarea grows to its full content height instead of
 * scrolling on its own. A classic scrollbar appearing in the field narrows the
 * text and reflows it, hence the second measurement.
 */
function resizeComposerTextarea(textarea: HTMLTextAreaElement | null): void {
  if (!textarea) return;
  textarea.style.height = "auto";
  const measured = textarea.scrollHeight;
  textarea.style.height = `${measured}px`;
  const settled = textarea.scrollHeight;
  if (settled > measured) textarea.style.height = `${settled}px`;
}

/**
 * Width the field's `scrollbar-gutter: stable` reserves on the right. It is the
 * platform's scrollbar width: 0 with overlay scrollbars (the macOS default), the
 * 4px our `::-webkit-scrollbar` rule asks for on Chromium, and the classic 15px
 * scrollbar once macOS is set to always show scrollbars. Only the invisible
 * actions spacer subtracts this width; the buttons are outside the scrollport.
 */
function composerScrollbarGutter(field: HTMLElement): number {
  const styles = window.getComputedStyle(field);
  const borders = (parseFloat(styles.borderLeftWidth) || 0) + (parseFloat(styles.borderRightWidth) || 0);
  return Math.max(0, field.offsetWidth - field.clientWidth - borders);
}

function compareModelOptions(a: ModelOption, b: ModelOption): number {
  return MODEL_OPTION_COLLATOR.compare(a.name || a.modelId, b.name || b.modelId)
    || MODEL_OPTION_COLLATOR.compare(a.provider, b.provider)
    || MODEL_OPTION_COLLATOR.compare(a.modelId, b.modelId);
}

export function filterModelOptions(options: ModelOption[], query: string): ModelOption[] {
  const normalizedQuery = query.trim().toLocaleLowerCase();
  if (!normalizedQuery) return options;

  return options.filter((option) => (
    `${option.name} ${option.modelId}`
      .toLocaleLowerCase()
      .includes(normalizedQuery)
  ));
}

const THINKING_LEVELS = ["auto", "off", "minimal", "low", "medium", "high", "xhigh", "max"] as const;
const THINKING_LEVEL_DESC_KEYS: Record<typeof THINKING_LEVELS[number], string> = {
  auto: "chat.thinkingUseDefault", off: "chat.thinkingOff", minimal: "chat.thinkingMinimal", low: "chat.thinkingLow",
  medium: "chat.thinkingMedium", high: "chat.thinkingHigh", xhigh: "chat.thinkingXhigh", max: "chat.thinkingMax",
};

function formatTokenCount(tokens: number): string {
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}M`;
  if (tokens >= 1_000) return `${Math.round(tokens / 1_000)}k`;
  return tokens.toLocaleString();
}

type SlashCommandPaletteItem = SlashCommandInfo | {
  name: string;
  description: string;
  source: "builtin";
};

type SlashCommandSource = SlashCommandPaletteItem["source"];
type SelectedSlashCommand = ChatDraftCommand;
type SelectedAtMention = ChatDraftMention;
type SkillMenuGroupId = "official" | "project" | "global" | "other";

export interface SkillMenuGroup {
  id: SkillMenuGroupId;
  skills: SkillInfo[];
}

const BUILTIN_SLASH_COMMANDS: SlashCommandPaletteItem[] = [
  { name: "new", description: "chat.commandNew", source: "builtin" },
  { name: "compact", description: "chat.commandCompact", source: "builtin" },
  { name: "reload", description: "chat.commandReload", source: "builtin" },
  { name: "name", description: "chat.commandName", source: "builtin" },
  { name: "session", description: "chat.commandSession", source: "builtin" },
  { name: "copy", description: "chat.commandCopy", source: "builtin" },
];

const SLASH_SOURCES: SlashCommandSource[] = ["builtin", "extension", "prompt", "skill"];

const SLASH_SOURCE_GROUP_LABEL_KEYS: Record<SlashCommandSource, string> = {
  builtin: "chat.builtIn",
  extension: "chat.extensions",
  prompt: "chat.prompts",
  skill: "chat.skills",
};

const SLASH_SOURCE_ORDER: Record<SlashCommandSource, number> = {
  builtin: 0,
  extension: 1,
  prompt: 2,
  skill: 3,
};

function slashMatchRank(command: SlashCommandPaletteItem, query: string, t: (key: string) => string): number {
  const name = command.name.toLowerCase();
  const description = getSlashDescription(command, t).toLowerCase();
  if (name === query) return 0;
  if (name.startsWith(query)) return 1;
  if (name.includes(query)) return 2;
  if (description.includes(query)) return 3;
  return 4;
}

function getSlashDescription(command: SlashCommandPaletteItem, t: (key: string) => string): string {
  return command.source === "builtin" ? t(command.description) : command.description ?? "";
}

export function getSlashCommandTagKind(command: SlashCommandPaletteItem): ChatDraftCommandKind {
  if (command.source === "skill") return "skill";

  const sourceInfo = "sourceInfo" in command ? command.sourceInfo : undefined;
  const source = `${sourceInfo?.source ?? ""} ${sourceInfo?.path ?? ""}`.toLowerCase();
  if (source.includes("mcp")) return "mcp";
  if (command.source === "extension" && (sourceInfo?.origin === "package" || source.includes("plugin"))) return "plugin";
  return "command";
}

const COMMAND_TAG_STYLES: Record<ChatDraftCommandKind, { background: string; border: string; color: string }> = {
  command: { background: "rgba(59, 130, 246, 0.12)", border: "rgba(59, 130, 246, 0.36)", color: "rgb(37, 99, 235)" },
  skill: { background: "rgba(16, 185, 129, 0.12)", border: "rgba(16, 185, 129, 0.36)", color: "rgb(5, 150, 105)" },
  mcp: { background: "rgba(245, 158, 11, 0.13)", border: "rgba(245, 158, 11, 0.38)", color: "rgb(180, 83, 9)" },
  plugin: { background: "rgba(236, 72, 153, 0.12)", border: "rgba(236, 72, 153, 0.36)", color: "rgb(190, 24, 93)" },
};

const AT_MENTION_TAG_STYLE = {
  background: "rgba(6, 182, 212, 0.12)",
  border: "rgba(6, 182, 212, 0.36)",
  color: "rgb(8, 145, 178)",
};

export function parseSelectedAtMentions(text: string): SelectedAtMention[] | null {
  const mentions: SelectedAtMention[] = [];
  let cursor = 0;
  while (cursor < text.length) {
    while (cursor < text.length && /\s/.test(text[cursor])) cursor += 1;
    if (cursor >= text.length) break;
    const match = /^(?:@"[^"\n]+"(?::\d+(?:-\d+)?)?|@[^\s"]+)/.exec(text.slice(cursor));
    if (!match) return null;
    const end = cursor + match[0].length;
    if (end < text.length && !/\s/.test(text[end])) return null;
    mentions.push({ text: match[0] });
    cursor = end;
  }
  return mentions.length > 0 ? mentions : null;
}

export function parseSelectedAtMention(text: string): SelectedAtMention | null {
  const mentions = parseSelectedAtMentions(text);
  return mentions?.length === 1 ? mentions[0] : null;
}

export function buildComposerMessage(
  value: string,
  selectedSlashCommand: SelectedSlashCommand | null,
  selectedAtMentions: SelectedAtMention[],
): string {
  const args = [
    ...selectedAtMentions.map((mention) => mention.text),
    value.trim(),
  ].filter(Boolean).join(" ");
  return selectedSlashCommand
    ? `/${selectedSlashCommand.name}${args ? ` ${args}` : ""}`
    : args;
}

// Skill slash commands are named "skill:<skillName>"; look the skill up in the
// dormancy map fetched from /api/skills. Unknown skills are treated as active.
function isDormantSkillCommand(command: SlashCommandPaletteItem, dormancy: Record<string, boolean>): boolean {
  if (command.source !== "skill" || !command.name.startsWith("skill:")) return false;
  return dormancy[command.name.slice("skill:".length)];
}

function getSkillMenuGroupId(skill: SkillInfo): SkillMenuGroupId {
  const source = skill.sourceInfo?.source?.toLowerCase() ?? "";
  const scope = skill.sourceInfo?.scope?.toLowerCase() ?? "";
  const normalizedPath = skill.filePath.replaceAll("\\", "/").toLowerCase();
  if (
    source === "builtin" ||
    source === "sdk" ||
    source === "system" ||
    source === "official" ||
    source.includes("official") ||
    normalizedPath.includes("/.codex/skills/.system/")
  ) return "official";
  if (scope === "project" || source === "project") return "project";
  if (scope === "user" || source === "user") return "global";
  return "other";
}

export function buildSkillMenuGroups(skills: SkillInfo[], query: string): SkillMenuGroup[] {
  const normalizedQuery = query.trim().toLocaleLowerCase();
  const filtered = skills
    .filter((skill) => !normalizedQuery || `${skill.name} ${skill.description}`.toLocaleLowerCase().includes(normalizedQuery))
    .sort((a, b) => Number(a.disableModelInvocation) - Number(b.disableModelInvocation)
      || MODEL_OPTION_COLLATOR.compare(a.name, b.name));
  const groupOrder: SkillMenuGroupId[] = ["official", "project", "global", "other"];
  return groupOrder
    .map((id) => ({ id, skills: filtered.filter((skill) => getSkillMenuGroupId(skill) === id) }))
    .filter((group) => group.skills.length > 0);
}

export function buildSlashCommandLayout(
  commands: SlashCommandPaletteItem[],
  dormancy: Record<string, boolean>,
) {
  let index = 0;
  const groups = SLASH_SOURCES
    .map((source) => {
      const sourceCommands = commands.filter((command) => command.source === source);
      const orderedCommands = source === "skill"
        ? [
            ...sourceCommands.filter((command) => !isDormantSkillCommand(command, dormancy)),
            ...sourceCommands.filter((command) => isDormantSkillCommand(command, dormancy)),
          ]
        : sourceCommands;
      return {
        source,
        items: orderedCommands.map((command) => ({ command, index: index++ })),
      };
    })
    .filter((group) => group.items.length > 0);

  return {
    commands: groups.flatMap((group) => group.items.map(({ command }) => command)),
    groups,
  };
}

function imageToDraftImage(image: AttachedImage): ChatDraftImage {
  return { data: image.data, mimeType: image.mimeType };
}

function draftImageToAttachedImage(image: ChatDraftImage): AttachedImage {
  return {
    ...image,
    previewUrl: `data:${image.mimeType};base64,${image.data}`,
  };
}

function draftImagesToAttachedImages(images: ChatDraftImage[] | undefined): AttachedImage[] {
  return (images ?? [])
    .filter(isBase64ImageWithinLimits)
    .slice(0, MAX_ATTACHED_IMAGES)
    .map(draftImageToAttachedImage);
}

export function canRestoreUserMessage(
  value: string,
  attachedImageCount: number,
  pendingImageCount: number,
  selectedTagCount = 0,
): boolean {
  return !value.trim() && attachedImageCount === 0 && pendingImageCount === 0 && selectedTagCount === 0;
}

export function getUserMessageText(message: UserMessage): string {
  if (typeof message.content === "string") return message.content;
  return message.content
    .filter((block): block is TextContent => block.type === "text")
    .map((block) => block.text)
    .join("\n");
}

export function getUserMessageDraftImages(message: UserMessage): ChatDraftImage[] {
  if (typeof message.content === "string") return [];
  return message.content.flatMap((block) => {
    if (block.type !== "image") return [];

    // Support both the current nested image format and older flat pi-ai entries.
    const flat = block as unknown as { data?: unknown; mimeType?: unknown };
    const data = block.source?.type === "base64" ? block.source.data : flat.data;
    const mimeType = block.source?.type === "base64" ? block.source.media_type : flat.mimeType;
    if (typeof data !== "string" || typeof mimeType !== "string") return [];

    const image = { data, mimeType };
    return isBase64ImageWithinLimits(image) ? [image] : [];
  });
}

function revokeImagePreview(image: AttachedImage): void {
  if (image.previewUrl.startsWith("blob:")) {
    URL.revokeObjectURL(image.previewUrl);
  }
}

function QueuedMessageRow({ kind, text }: { kind: "steer" | "follow-up"; text: string }) {
  const { t } = useI18n();

  return (
    <div
      title={text}
      style={{
        display: "flex",
        alignItems: "center",
        gap: 8,
        padding: "3px 10px",
        fontSize: 12,
        color: "var(--text-muted)",
        minWidth: 0,
      }}
    >
      <span
        style={{
          flexShrink: 0,
          fontSize: 10,
          fontFamily: "var(--font-mono)",
          padding: "1px 7px",
          borderRadius: 999,
          border: `1px solid ${kind === "steer" ? "color-mix(in srgb, var(--accent) 45%, transparent)" : "var(--border)"}`,
          color: kind === "steer" ? "var(--accent)" : "var(--text-dim)",
        }}
      >
        {kind === "steer" ? t("chat.steer") : t("chat.followUp")}
      </span>
      <span style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{text}</span>
    </div>
  );
}

function ModelNoticeBanner({ tone, title, body }: { tone: "error" | "warning"; title: string; body: string }) {
  const color = tone === "error" ? "239,68,68" : "234,179,8";
  return (
    <div
      role="alert"
      style={{
        display: "flex",
        alignItems: "flex-start",
        gap: 8,
        maxHeight: 120,
        marginBottom: 8,
        padding: "7px 10px",
        overflowY: "auto",
        border: `1px solid rgba(${color},0.3)`,
        borderRadius: 6,
        background: `rgba(${color},0.07)`,
        color: `rgb(${color})`,
        fontSize: 11,
        lineHeight: 1.45,
      }}
    >
      <svg
        width="13"
        height="13"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        style={{ flexShrink: 0, marginTop: 1 }}
        aria-hidden="true"
      >
        <path d="M10.3 2.9 1.8 17a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 2.9a2 2 0 0 0-3.4 0Z" />
        <line x1="12" y1="9" x2="12" y2="13" />
        <line x1="12" y1="17" x2="12.01" y2="17" />
      </svg>
      <div style={{ minWidth: 0 }}>
        <div style={{ fontWeight: 600 }}>{title}</div>
        <div style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>{body}</div>
      </div>
    </div>
  );
}

export function ModelErrorBanner({ error }: { error?: string | null }) {
  const { t } = useI18n();

  if (!error) return null;
  return <ModelNoticeBanner tone="error" title={t("chat.modelError")} body={error} />;
}

/** Surfaces `enabledModels` patterns that matched nothing, so a typo is visible (#307). */
export function ModelScopeWarningBanner({ warnings }: { warnings?: string[] }) {
  const { t } = useI18n();

  if (!warnings || warnings.length === 0) return null;
  return (
    <ModelNoticeBanner
      tone="warning"
      title={warnings.length > 1 ? t("chat.modelScopeWarnings") : t("chat.modelScopeWarning")}
      body={warnings.join("\n")}
    />
  );
}

export const ChatInput = forwardRef<ChatInputHandle, Props>(function ChatInput({
  onSend, onAbort, onSteer, onFollowUp, isStreaming, model, isAutoModelSelection, modelNames, modelList, modelError, modelScopeWarnings, onModelChange, modelSwitching,
  compactError, compactResult, toolPreset, onToolPresetChange, chatMode, onChatModeChange,
  thinkingLevel, onThinkingLevelChange, availableThinkingLevels, thinkingLevelMap,
  retryInfo, promptFailure, onRetryPrompt, queuedMessages, inputHistory = [], onRecallQueue,
  slashCommands, slashCommandsLoading, onLoadSlashCommands,
  onBuiltinCommand,
  onAudioUnlock,
  onOpenConsole,
  consoleActive = false,
  onPromptWithStreamingBehavior,
  draftKey,
  cwd,
  sessionId,
  showJumpToLatest = false,
  onJumpToLatest,
}: Props, ref) {
  const { t } = useI18n();
  const isMobile = useIsMobile();
  // 普通对话模式（π 图标）：没有工具，输入框的 ! shell 模式和工具预设都不适用。
  const chatModeActive = (chatMode ?? "work") === "chat";
  const [value, setValue] = useState(() => (draftKey ? getDraft(draftKey)?.value ?? "" : ""));
  const [selectedSlashCommand, setSelectedSlashCommand] = useState<SelectedSlashCommand | null>(() => (
    draftKey ? getDraft(draftKey)?.selectedCommand ?? null : null
  ));
  const [selectedAtMentions, setSelectedAtMentions] = useState<SelectedAtMention[]>(() => (
    draftKey ? getDraft(draftKey)?.selectedMentions ?? [] : []
  ));
  const [modelDropdownOpen, setModelDropdownOpen] = useState(false);
  const [pairDialogOpen, setPairDialogOpen] = useState(false);
  const [modelDropdownRect, setModelDropdownRect] = useState<{ top: number; left: number; width: number } | null>(null);
  const [modelFilter, setModelFilter] = useState("");
  const [toolDropdownOpen, setToolDropdownOpen] = useState(false);
  const [thinkingDropdownOpen, setThinkingDropdownOpen] = useState(false);
  const [controlsMenuOpen, setControlsMenuOpen] = useState(false);
  const [queueMenuOpen, setQueueMenuOpen] = useState(false);
  const [queueCountdown, setQueueCountdown] = useState(3);
  const [queueCountdownPaused, setQueueCountdownPaused] = useState(false);
  const [queueActiveMode, setQueueActiveMode] = useState<QueueSendMode>("followup");
  const queueActionsRef = useRef<HTMLDivElement>(null);
  const queueSteerOptionRef = useRef<HTMLButtonElement>(null);
  const queueDefaultOptionRef = useRef<HTMLButtonElement>(null);
  const queueMenuId = React.useId();
  const [attachedImages, setAttachedImages] = useState<AttachedImage[]>(() => (
    draftKey ? draftImagesToAttachedImages(getDraft(draftKey)?.images) : []
  ));
  const trimmedValue = value.trimStart();
  const bashMode = !chatModeActive && attachedImages.length === 0 && trimmedValue.startsWith("!");
  const bashExcluded = bashMode && trimmedValue.startsWith("!!");
  const [slashMenuOpen, setSlashMenuOpen] = useState(false);
  const [slashActiveIndex, setSlashActiveIndex] = useState(0);
  const [slashMenuMaxHeight, setSlashMenuMaxHeight] = useState<number | null>(null);
  const [atQuery, setAtQuery] = useState<AtQueryMatch | null>(null);
  const [atMenuOpen, setAtMenuOpen] = useState(false);
  const [atActiveIndex, setAtActiveIndex] = useState(0);
  const [historyMenuOpen, setHistoryMenuOpen] = useState(false);
  const [historyActiveIndex, setHistoryActiveIndex] = useState(0);
  const [fileIndex, setFileIndex] = useState<{ cwd: string; entries: FileIndexEntry[]; truncated: boolean } | null>(null);
  const [fileIndexLoading, setFileIndexLoading] = useState(false);
  const [atServerResult, setAtServerResult] = useState<{ cwd: string; query: string; matches: FileIndexEntry[] } | null>(null);
  const [skillDormancyState, setSkillDormancyState] = useState<{
    cwd: string;
    values: Record<string, boolean>;
  } | null>(null);
  const skillDormancy = cwd && skillDormancyState?.cwd === cwd
    ? skillDormancyState.values
    : {};
  const [skillMenuOpen, setSkillMenuOpen] = useState(false);
  const [skillFilter, setSkillFilter] = useState("");
  const [skillMenuMaxHeight, setSkillMenuMaxHeight] = useState<number | null>(null);
  const [skillMenuState, setSkillMenuState] = useState<{
    cwd: string;
    skills: SkillInfo[];
  } | null>(null);
  const [skillMenuLoading, setSkillMenuLoading] = useState(false);
  const [skillMenuError, setSkillMenuError] = useState<string | null>(null);

  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const composerFieldRef = useRef<HTMLDivElement>(null);
  const composerActionsRef = useRef<HTMLDivElement>(null);
  // 字号同时驱动输入框自身与高度重算：这里是全应用最早挂载的消费者，
  // 自定义属性也随这次订阅应用到文档根节点上（见 hooks/useChatAppearance.ts）。
  const { fontSize: chatFontSize } = useChatAppearance();
  const dropdownRef = useRef<HTMLDivElement>(null);
  const modelDropdownPanelRef = useRef<HTMLDivElement>(null);
  const toolDropdownRef = useRef<HTMLDivElement>(null);
  const thinkingDropdownRef = useRef<HTMLDivElement>(null);
  const controlsMenuRef = useRef<HTMLDivElement>(null);
  const historyMenuRef = useRef<HTMLDivElement>(null);
  const skillMenuRef = useRef<HTMLDivElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const isComposingRef = useRef(false);
  const lastCompositionEndAtRef = useRef(0);
  const slashCommandsRequestedRef = useRef(false);
  const slashMenuRef = useRef<HTMLDivElement>(null);
  const slashItemRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const atItemRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const historyItemRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const fileIndexMetaRef = useRef<{ cwd: string; fetchedAt: number } | null>(null);
  const fileIndexFetchingRef = useRef<string | null>(null);
  const draftKeyRef = useRef(draftKey);
  const valueRef = useRef(value);
  const attachedImagesRef = useRef(attachedImages);
  const selectedSlashCommandRef = useRef<SelectedSlashCommand | null>(selectedSlashCommand);
  const selectedAtMentionsRef = useRef<SelectedAtMention[]>(selectedAtMentions);
  const pendingImageCountRef = useRef(0);
  valueRef.current = value;
  attachedImagesRef.current = attachedImages;
  selectedSlashCommandRef.current = selectedSlashCommand;
  selectedAtMentionsRef.current = selectedAtMentions;

  useImperativeHandle(ref, () => ({
    insertIfEmpty(text: string) {
      const ta = textareaRef.current;
      const current = ta ? ta.value : value;
      if (current.trim()) return;
      valueRef.current = text;
      setValue(text);
      setSelectedSlashCommand(null);
      setSelectedAtMentions([]);
      setAtQuery(null);
      requestAnimationFrame(() => {
        if (!ta) return;
        ta.focus();
        resizeComposerTextarea(ta);
      });
    },
    replaceMessage(message: UserMessage) {
      const ta = textareaRef.current;
      const current = ta ? ta.value : value;
      const selectedTagCount = selectedAtMentionsRef.current.length + (selectedSlashCommandRef.current ? 1 : 0);
      if (!canRestoreUserMessage(current, attachedImagesRef.current.length, pendingImageCountRef.current, selectedTagCount)) return;

      const restoredText = getUserMessageText(message);
      const restoredImages = draftImagesToAttachedImages(getUserMessageDraftImages(message));
      valueRef.current = restoredText;
      attachedImagesRef.current = restoredImages;
      setValue(restoredText);
      setSelectedSlashCommand(null);
      setSelectedAtMentions([]);
      setAtQuery(null);
      setHistoryMenuOpen(false);
      setAttachedImages((prev) => {
        prev.forEach(revokeImagePreview);
        return restoredImages;
      });
      requestAnimationFrame(() => {
        if (!ta) return;
        ta.focus();
        resizeComposerTextarea(ta);
      });
    },
    prependText(text: string) {
      if (!text.trim()) return;
      const ta = textareaRef.current;
      const current = ta ? ta.value : value;
      // Mirrors the TUI's queue restore: queued text first, then whatever
      // the user already typed, separated by a blank line.
      const combined = [text, current].filter((t) => t.trim()).join("\n\n");
      valueRef.current = combined;
      setValue(combined);
      setSelectedSlashCommand(null);
      setSelectedAtMentions([]);
      setAtQuery(null);
      requestAnimationFrame(() => {
        if (!ta) return;
        ta.focus();
        ta.setSelectionRange(combined.length, combined.length);
        resizeComposerTextarea(ta);
      });
    },
    rekeyDraft(previousKey: string, nextKey: string) {
      if (previousKey === nextKey) return;
      if (draftKeyRef.current !== previousKey) {
        rekeyStoredDraft(previousKey, nextKey);
        return;
      }

      const currentDraft = {
        value: valueRef.current,
        images: attachedImagesRef.current.map(imageToDraftImage),
        ...(selectedSlashCommandRef.current && { selectedCommand: selectedSlashCommandRef.current }),
        ...(selectedAtMentionsRef.current.length && { selectedMentions: selectedAtMentionsRef.current }),
      };
      const moved = rekeyStoredDraft(previousKey, nextKey, currentDraft) ?? { value: "", images: [] };
      const unchanged = moved.value === currentDraft.value
        && moved.images.length === currentDraft.images.length
        && moved.images.every((image, index) => (
          image.data === currentDraft.images[index]?.data
          && image.mimeType === currentDraft.images[index]?.mimeType
        ))
        && moved.selectedCommand?.name === currentDraft.selectedCommand?.name
        && moved.selectedCommand?.kind === currentDraft.selectedCommand?.kind
        && (moved.selectedMentions?.length ?? 0) === (currentDraft.selectedMentions?.length ?? 0)
        && (moved.selectedMentions ?? []).every((mention, index) => (
          mention.text === currentDraft.selectedMentions?.[index]?.text
        ));
      draftKeyRef.current = nextKey;
      if (unchanged) return;

      const movedImages = draftImagesToAttachedImages(moved.images);
      valueRef.current = moved.value;
      attachedImagesRef.current = movedImages;
      setValue(moved.value);
      setSelectedSlashCommand(moved.selectedCommand ?? null);
      setSelectedAtMentions(moved.selectedMentions ?? []);
      setAttachedImages((current) => {
        current.forEach(revokeImagePreview);
        return movedImages;
      });
      setAtQuery(null);
      setHistoryMenuOpen(false);
    },
    restoreSubmission(text: string, images?: ChatDraftImage[], targetDraftKey?: string) {
      if (!text.trim() && !images?.length) return;

      // clearInput is queued before the submission handler runs. Compose with
      // that queued state so a fast rejection cannot observe stale DOM text and
      // then get overwritten by the clear.
      const currentDraftKey = draftKeyRef.current;
      const destinationDraftKey = targetDraftKey ?? currentDraftKey;
      const targetsCurrentComposer = destinationDraftKey === currentDraftKey;
      const storedDraft = !targetsCurrentComposer && destinationDraftKey
        ? getDraft(destinationDraftKey)
        : null;
      const restoredDraft = mergeRestoredSubmissionDraft(
        text,
        images,
        targetsCurrentComposer ? valueRef.current : (storedDraft?.value ?? ""),
        targetsCurrentComposer
          ? attachedImagesRef.current.map(imageToDraftImage)
          : (storedDraft?.images ?? []),
      );
      // The first optimistic message switches ChatWindow out of its empty-state
      // layout and remounts this component. Persist synchronously so recovery is
      // not lost if this instance is the one being unmounted.
      if (destinationDraftKey) setDraft(destinationDraftKey, restoredDraft);
      if (!targetsCurrentComposer) return;
      const restoredImages = images?.length
        ? [
            ...draftImagesToAttachedImages(images).slice(
              0,
              Math.max(0, MAX_ATTACHED_IMAGES - attachedImagesRef.current.length),
            ),
            ...attachedImagesRef.current,
          ].slice(0, MAX_ATTACHED_IMAGES)
        : attachedImagesRef.current;
      // Session promotion can rekey this composer before React flushes the
      // functional updates below, so update the imperative snapshot first.
      valueRef.current = restoredDraft.value;
      attachedImagesRef.current = restoredImages;
      setValue((current) => {
        const restored = mergeRestoredSubmissionText(text, current);
        valueRef.current = restored;
        return restored;
      });
      setSelectedSlashCommand(null);
      setSelectedAtMentions([]);
      setAtQuery(null);
      setHistoryMenuOpen(false);
      if (images?.length) {
        setAttachedImages((current) => {
          const available = Math.max(0, MAX_ATTACHED_IMAGES - current.length);
          const restored = draftImagesToAttachedImages(images)
            .slice(0, available);
          const next = restored.length > 0 ? [...restored, ...current] : current;
          attachedImagesRef.current = next;
          return next;
        });
      }
      requestAnimationFrame(() => {
        const ta = textareaRef.current;
        if (!ta) return;
        ta.focus();
        ta.setSelectionRange(ta.value.length, ta.value.length);
        resizeComposerTextarea(ta);
      });
    },
    insertText(text: string) {
      const mentions = parseSelectedAtMentions(text);
      if (mentions) {
        setSelectedAtMentions((current) => [...current, ...mentions]);
        setAtQuery(null);
        requestAnimationFrame(() => textareaRef.current?.focus());
        return;
      }
      const ta = textareaRef.current;
      if (!ta) {
        setValue((v) => v + (v ? " " : "") + text);
        return;
      }
      const start = ta.selectionStart ?? ta.value.length;
      const end = ta.selectionEnd ?? ta.value.length;
      const before = ta.value.slice(0, start);
      const after = ta.value.slice(end);
      const sep = before.length > 0 && !before.endsWith(" ") ? " " : "";
      const newVal = before + sep + text + after;
      valueRef.current = newVal;
      setValue(newVal);
      setAtQuery(null);
      requestAnimationFrame(() => {
        if (!ta) return;
        const pos = start + sep.length + text.length;
        ta.setSelectionRange(pos, pos);
        ta.focus();
        resizeComposerTextarea(ta);
      });
    },
    addImages(files: File[]) {
      processImageFiles(files);
    },
  }));

  const processImageFiles = useCallback(async (files: File[]) => {
    const remaining = Math.max(
      0,
      MAX_ATTACHED_IMAGES - attachedImagesRef.current.length - pendingImageCountRef.current,
    );
    const imageFiles = files
      .filter((f) => f.type.startsWith("image/") && f.size <= MAX_ATTACHED_IMAGE_BYTES)
      .slice(0, remaining);
    if (!imageFiles.length) return;
    pendingImageCountRef.current += imageFiles.length;
    try {
      const newImages = await Promise.all(
        imageFiles.map(
          (file) =>
            new Promise<AttachedImage>((resolve, reject) => {
              const reader = new FileReader();
              reader.onload = () => {
                const result = reader.result as string;
                // result is "data:<mime>;base64,<data>"
                const base64 = result.split(",")[1];
                resolve({ data: base64, mimeType: file.type, previewUrl: URL.createObjectURL(file) });
              };
              reader.onerror = reject;
              reader.readAsDataURL(file);
            })
        )
      );
      setAttachedImages((prev) => {
        const accepted = newImages.slice(0, Math.max(0, MAX_ATTACHED_IMAGES - prev.length));
        newImages.slice(accepted.length).forEach(revokeImagePreview);
        const next = [...prev, ...accepted];
        attachedImagesRef.current = next;
        return next;
      });
    } finally {
      pendingImageCountRef.current -= imageFiles.length;
    }
  }, []);

  const removeImage = useCallback((index: number) => {
    setAttachedImages((prev) => {
      const next = [...prev];
      const [removed] = next.splice(index, 1);
      if (removed) revokeImagePreview(removed);
      attachedImagesRef.current = next;
      return next;
    });
  }, []);

  const clearImages = useCallback(() => {
    attachedImagesRef.current = [];
    setAttachedImages((prev) => {
      prev.forEach(revokeImagePreview);
      return [];
    });
  }, []);

  const clearInput = useCallback(() => {
    valueRef.current = "";
    setValue("");
    setSelectedSlashCommand(null);
    setSelectedAtMentions([]);
    setAtQuery(null);
    setHistoryMenuOpen(false);
    if (draftKey) clearDraft(draftKey);
    if (draftKeyRef.current && draftKeyRef.current !== draftKey) clearDraft(draftKeyRef.current);
    clearImages();
    if (textareaRef.current) {
      textareaRef.current.style.height = "auto";
    }
  }, [clearImages, draftKey]);

  useEffect(() => {
    if (!draftKey || draftKeyRef.current !== draftKey) return;
    setDraft(draftKey, {
      value,
      images: attachedImages.map(imageToDraftImage),
      ...(selectedSlashCommand && { selectedCommand: selectedSlashCommand }),
      ...(selectedAtMentions.length && { selectedMentions: selectedAtMentions }),
    });
  }, [attachedImages, draftKey, selectedAtMentions, selectedSlashCommand, value]);

  useEffect(() => {
    const previousDraftKey = draftKeyRef.current;
    if (previousDraftKey === draftKey) return;

    if (previousDraftKey) {
      setDraft(previousDraftKey, {
        value: valueRef.current,
        images: attachedImagesRef.current.map(imageToDraftImage),
        ...(selectedSlashCommand && { selectedCommand: selectedSlashCommand }),
        ...(selectedAtMentions.length && { selectedMentions: selectedAtMentions }),
      });
    }

    const draft = draftKey ? getDraft(draftKey) : null;
    draftKeyRef.current = draftKey;
    const nextValue = draft?.value ?? "";
    const nextImages = draftImagesToAttachedImages(draft?.images);
    valueRef.current = nextValue;
    attachedImagesRef.current = nextImages;
    setValue(nextValue);
    setSelectedSlashCommand(draft?.selectedCommand ?? null);
    setSelectedAtMentions(draft?.selectedMentions ?? []);
    setAtQuery(null);
    setHistoryMenuOpen(false);
    setAttachedImages((prev) => {
      prev.forEach(revokeImagePreview);
      return nextImages;
    });
  }, [draftKey, selectedAtMentions, selectedSlashCommand]);

  useEffect(() => {
    resizeComposerTextarea(textareaRef.current);
  }, [value, chatFontSize]);

  // The controls live outside the scrollport, anchored to the shell's padding.
  // Reserve their measured size inside the field so text cannot run under them.
  useLayoutEffect(() => {
    const field = composerFieldRef.current;
    const actions = composerActionsRef.current;
    if (!field || !actions) return;
    let previousMetrics = "";
    const syncLayout = () => {
      const { width, height } = actions.getBoundingClientRect();
      field.style.setProperty("--composer-actions-width", `${width}px`);
      field.style.setProperty("--composer-actions-height", `${height}px`);
      field.style.setProperty("--composer-scrollbar-gutter", `${composerScrollbarGutter(field)}px`);
      const metrics = `${width}:${height}:${textareaRef.current?.clientWidth}`;
      if (metrics !== previousMetrics) {
        previousMetrics = metrics;
        resizeComposerTextarea(textareaRef.current);
      }
    };
    syncLayout();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(syncLayout);
    observer.observe(field);
    observer.observe(actions);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    return () => {
      attachedImagesRef.current.forEach(revokeImagePreview);
    };
  }, []);

  const handleSend = useCallback(async () => {
    const msg = buildComposerMessage(value, selectedSlashCommand, selectedAtMentions);
    if (!msg && !attachedImages.length) return;
    if (isStreaming) return;
    onAudioUnlock?.();
    if (!attachedImages.length && msg.startsWith("/") && onBuiltinCommand) {
      const result = await onBuiltinCommand(msg);
      if (result.handled) {
        if (!result.error) clearInput();
        return;
      }
    }
    clearInput();
    onSend(msg, attachedImages.length ? attachedImages : undefined);
  }, [value, attachedImages, isStreaming, onBuiltinCommand, onSend, clearInput, onAudioUnlock, selectedAtMentions, selectedSlashCommand]);

  const slashQuery = !selectedSlashCommand && value.startsWith("/") && !/\s/.test(value.slice(1))
    ? value.slice(1).toLowerCase()
    : null;

  const filteredSlashCommands = (() => {
    if (slashQuery === null) return [];
    const commands = [...(isStreaming ? [] : BUILTIN_SLASH_COMMANDS), ...(slashCommands ?? [])];
    return [...commands]
      .filter((command) => {
        const name = command.name.toLowerCase();
        const description = getSlashDescription(command, t).toLowerCase();
        return name.includes(slashQuery) || description.includes(slashQuery);
      })
      .sort((a, b) => {
        const rankDelta = slashMatchRank(a, slashQuery, t) - slashMatchRank(b, slashQuery, t);
        if (rankDelta !== 0) return rankDelta;
        return SLASH_SOURCE_ORDER[a.source] - SLASH_SOURCE_ORDER[b.source]
          || MODEL_OPTION_COLLATOR.compare(a.name, b.name);
      });
  })();

  const {
    commands: displayedSlashCommands,
    groups: groupedSlashCommands,
  } = buildSlashCommandLayout(filteredSlashCommands, skillDormancy);

  const slashCommandCountLabel = filteredSlashCommands.length === 1
    ? t(slashQuery ? "chat.match" : "chat.command")
    : t(slashQuery ? "chat.matches" : "chat.commands", { count: filteredSlashCommands.length });
  const hasInputText = Boolean(value.trim()) || selectedSlashCommand !== null || selectedAtMentions.length > 0;
  const canQueueStreamingMessage = hasInputText || attachedImages.length > 0;
  const queueMenuVisible = queueMenuOpen && isStreaming && canQueueStreamingMessage && !!(onSteer || onFollowUp);
  useEffect(() => {
    if (queueMenuVisible) queueDefaultOptionRef.current?.focus();
    else setQueueMenuOpen(false);
  }, [queueMenuVisible]);

  // ── @ file autocomplete ──────────────────────────────────────────────────
  // Recomputed from the text before the caret on every change/caret move.
  // Disabled entirely when there is no cwd (new session without a directory).
  const updateAtQuery = useCallback((text: string, cursor: number | null) => {
    if (!cwd) {
      setAtQuery(null);
      return;
    }
    const pos = cursor ?? text.length;
    setAtQuery(extractAtQuery(text.slice(0, pos)));
  }, [cwd]);

  const atQueryText = atQuery?.query ?? null;
  const atLocalMatches: FileIndexEntry[] = React.useMemo(() => (
    atQueryText !== null && fileIndex && fileIndex.cwd === cwd
      ? filterFileEntries(fileIndex.entries, atQueryText)
      : []
  ), [atQueryText, fileIndex, cwd]);

  // When the client index is truncated (repo larger than the index cap),
  // local filtering cannot see deep files, so queries are also ranked
  // server-side against the full listing. Local matches render immediately
  // and are replaced when the (debounced) server result for the current
  // query arrives; stale responses are ignored via the query/cwd tag.
  const needsServerSearch = Boolean(atQueryText && fileIndex?.truncated && fileIndex.cwd === cwd);
  useEffect(() => {
    if (!needsServerSearch || !cwd || !atQueryText) return;
    const fetchCwd = cwd;
    const query = atQueryText;
    const timer = setTimeout(() => {
      fetch(`/api/file-index?cwd=${encodeURIComponent(fetchCwd)}&q=${encodeURIComponent(query)}`)
        .then((res) => {
          if (!res.ok) throw new Error(`file search failed: ${res.status}`);
          return res.json() as Promise<{ matches?: FileIndexEntry[] }>;
        })
        .then((data) => setAtServerResult({ cwd: fetchCwd, query, matches: data.matches ?? [] }))
        .catch(() => {
          // Keep showing local matches; the next keystroke retries.
        });
    }, 150);
    return () => clearTimeout(timer);
  }, [needsServerSearch, atQueryText, cwd]);

  const serverResultInUse = needsServerSearch
    && atServerResult !== null
    && atServerResult.cwd === cwd
    && atServerResult.query === atQueryText;
  const atMatches: FileIndexEntry[] = serverResultInUse ? atServerResult.matches : atLocalMatches;

  // Open/reset the menu whenever the @token appears or changes (mirrors the
  // slash menu: Escape closes it, the next keystroke re-opens it).
  const atTokenKey = atQuery === null ? null : `${atQuery.start}:${atQuery.quoted ? 1 : 0}:${atQuery.query}`;
  useEffect(() => {
    if (atTokenKey === null) {
      setAtMenuOpen(false);
      setAtActiveIndex(0);
      return;
    }
    setAtMenuOpen(true);
    setAtActiveIndex(0);
  }, [atTokenKey]);

  // Fetch the file index when the menu opens. The server caches per cwd for
  // ~10s, so re-opening refreshes cheaply; while typing nothing refetches.
  const atTokenActive = atQuery !== null;
  useEffect(() => {
    if (!atTokenActive || !cwd) return;
    const meta = fileIndexMetaRef.current;
    if (meta && meta.cwd === cwd && Date.now() - meta.fetchedAt < 10_000) return;
    if (fileIndexFetchingRef.current === cwd) return;
    fileIndexFetchingRef.current = cwd;
    const fetchCwd = cwd;
    setFileIndexLoading(true);
    fetch(`/api/file-index?cwd=${encodeURIComponent(fetchCwd)}`)
      .then((res) => {
        if (!res.ok) throw new Error(`file index failed: ${res.status}`);
        return res.json() as Promise<{ files?: string[]; truncated?: boolean }>;
      })
      .then((data) => {
        setFileIndex({ cwd: fetchCwd, entries: buildEntriesFromFiles(data.files ?? []), truncated: !!data.truncated });
        fileIndexMetaRef.current = { cwd: fetchCwd, fetchedAt: Date.now() };
      })
      .catch(() => {
        // Leave any previous index in place; next open retries.
        fileIndexMetaRef.current = null;
      })
      .finally(() => {
        fileIndexFetchingRef.current = null;
        setFileIndexLoading(false);
      });
  }, [atTokenActive, cwd]);

  const applyAtCompletion = useCallback((entry: FileIndexEntry) => {
    if (!atQuery) return;
    const ta = textareaRef.current;
    const cursor = ta?.selectionStart ?? value.length;
    const before = value.slice(0, atQuery.start);
    let after = value.slice(cursor);
    // Completing inside a quoted token (@"my dir/… with the caret before the
    // closing quote): the replacement carries its own closing quote, so drop
    // the old one right after the caret (mirrors the TUI's applyCompletion).
    if (atQuery.quoted && after.startsWith('"')) {
      after = after.slice(1);
    }
    const insert = buildAtInsertText(entry.path, entry.isDir, atQuery.quoted);
    const mention = parseSelectedAtMention(insert.text);
    if (mention) setSelectedAtMentions((current) => [...current, mention]);
    const newValue = before + after;
    valueRef.current = newValue;
    setValue(newValue);
    setAtQuery(null);
    requestAnimationFrame(() => {
      const el = textareaRef.current;
      if (!el) return;
      const newPos = before.length;
      el.focus();
      el.setSelectionRange(newPos, newPos);
      resizeComposerTextarea(el);
    });
  }, [atQuery, value]);

  useEffect(() => {
    if (atActiveIndex >= atMatches.length) {
      setAtActiveIndex(Math.max(0, atMatches.length - 1));
    }
  }, [atMatches.length, atActiveIndex]);

  useEffect(() => {
    atItemRefs.current.length = atMatches.length;
  }, [atMatches.length]);

  useEffect(() => {
    if (!atMenuOpen) return;
    atItemRefs.current[atActiveIndex]?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [atActiveIndex, atMenuOpen]);

  useEffect(() => {
    if (historyActiveIndex >= inputHistory.length) {
      setHistoryActiveIndex(Math.max(0, inputHistory.length - 1));
    }
  }, [inputHistory.length, historyActiveIndex]);

  useEffect(() => {
    historyItemRefs.current.length = inputHistory.length;
  }, [inputHistory.length]);

  useEffect(() => {
    if (!historyMenuOpen) return;
    historyItemRefs.current[historyActiveIndex]?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [historyActiveIndex, historyMenuOpen]);

  const applyHistoryInput = useCallback((text: string) => {
    setValue(text);
    setSelectedSlashCommand(null);
    setHistoryMenuOpen(false);
    setHistoryActiveIndex(0);
    setAtQuery(null);
    requestAnimationFrame(() => {
      const ta = textareaRef.current;
      if (!ta) return;
      ta.focus();
      ta.setSelectionRange(text.length, text.length);
      resizeComposerTextarea(ta);
    });
  }, []);

  const applySlashCommand = useCallback((command: SlashCommandPaletteItem) => {
    setSlashMenuOpen(false);
    setSlashActiveIndex(0);

    // /new 没有参数可填，选中即执行：不往输入框里挂一个还要再回车一次的命令 chip。
    if (command.source === "builtin" && command.name === "new" && onBuiltinCommand) {
      setValue("");
      void onBuiltinCommand("/new").then((result) => {
        // 没接住（无可用项目目录等）时退回挂 chip，不让这次选择静默丢掉。
        if (!result.handled) {
          setSelectedSlashCommand({ name: command.name, kind: getSlashCommandTagKind(command) });
        }
      });
      return;
    }

    setSelectedSlashCommand({ name: command.name, kind: getSlashCommandTagKind(command) });
    setValue("");
    requestAnimationFrame(() => {
      const ta = textareaRef.current;
      if (!ta) return;
      ta.focus();
      ta.setSelectionRange(0, 0);
      resizeComposerTextarea(ta);
    });
  }, [onBuiltinCommand]);

  const loadSkillMenu = useCallback(async () => {
    if (!cwd) return;
    const requestCwd = cwd;
    setSkillMenuLoading(true);
    setSkillMenuError(null);
    try {
      const res = await fetch(`/api/skills?cwd=${encodeURIComponent(requestCwd)}`);
      if (!res.ok) throw new Error(`skills fetch failed: ${res.status}`);
      const data = await res.json() as Partial<SkillsResponse>;
      setSkillMenuState({ cwd: requestCwd, skills: data.skills ?? [] });
    } catch (error) {
      setSkillMenuError(error instanceof Error ? error.message : String(error));
    } finally {
      setSkillMenuLoading(false);
    }
  }, [cwd]);

  const applySkill = useCallback((skill: SkillInfo) => {
    applySlashCommand({
      name: `skill:${skill.name}`,
      description: skill.description,
      source: "skill",
    });
    setSkillMenuOpen(false);
    setSkillFilter("");
  }, [applySlashCommand]);

  const removeSelectedSlashCommand = useCallback(() => {
    setSelectedSlashCommand(null);
    requestAnimationFrame(() => textareaRef.current?.focus());
  }, []);

  const removeSelectedAtMention = useCallback((index: number) => {
    setSelectedAtMentions((current) => current.filter((_, mentionIndex) => mentionIndex !== index));
    requestAnimationFrame(() => textareaRef.current?.focus());
  }, []);

  const sendQueued = useCallback(async (mode: QueueSendMode) => {
    const msg = buildComposerMessage(value, selectedSlashCommand, selectedAtMentions);
    if (!msg && !attachedImages.length) return;
    setQueueMenuOpen(false);
    textareaRef.current?.focus();
    onAudioUnlock?.();
    // 内置命令必须先走 onBuiltinCommand：漏掉这一层时 /compact 会原样变成一条
    // 引导给模型的用户消息。/name /session /copy 在运行中安全，/compact 与
    // /reload 由 handleBuiltinSlashCommand 按 Agent 运行状态拒绝。
    const builtinName = msg.startsWith("/") ? msg.slice(1).split(/\s+/, 1)[0].toLowerCase() : null;
    if (builtinName && BUILTIN_SLASH_COMMANDS.some((command) => command.name === builtinName)) {
      if (!onBuiltinCommand) return;
      const result = await onBuiltinCommand(msg);
      if (result.handled && !result.error) clearInput();
      return;
    }
    const streamingBehavior = mode === "steer" ? "steer" : "followUp";
    if (msg.startsWith("/") && onPromptWithStreamingBehavior) {
      clearInput();
      onPromptWithStreamingBehavior(msg, streamingBehavior, attachedImages.length ? attachedImages : undefined);
      return;
    }
    clearInput();
    if (mode === "steer" && onSteer) {
      onSteer(msg, attachedImages.length ? attachedImages : undefined);
    } else if (mode === "followup" && onFollowUp) {
      onFollowUp(msg, attachedImages.length ? attachedImages : undefined);
    }
  }, [value, attachedImages, onPromptWithStreamingBehavior, onSteer, onFollowUp, onBuiltinCommand, clearInput, onAudioUnlock, selectedAtMentions, selectedSlashCommand]);

  const requestQueuedSend = useCallback(() => {
    if (!canQueueStreamingMessage) return;
    // 内置命令不排进倒计时：sendQueued 会把它直接交给 handleBuiltinSlashCommand。
    // /new 因此和 Cmd+N 一样即时生效（运行中的会话留在后台继续，不被中断），
    // 运行中不合法的 /compact、/reload 也会马上得到拒绝提示，而不是等 3 秒。
    const pending = buildComposerMessage(value, selectedSlashCommand, selectedAtMentions);
    const builtinName = pending.startsWith("/") ? pending.slice(1).split(/\s+/, 1)[0].toLowerCase() : null;
    if (builtinName && BUILTIN_SLASH_COMMANDS.some((command) => command.name === builtinName)) {
      void sendQueued("followup");
      return;
    }
    if (onSteer && onFollowUp) {
      setQueueCountdown(3);
      setQueueCountdownPaused(false);
      setQueueActiveMode("followup");
      setQueueMenuOpen(true);
    }
    else if (onSteer || onFollowUp) void sendQueued(onSteer ? "steer" : "followup");
  }, [canQueueStreamingMessage, onSteer, onFollowUp, selectedAtMentions, selectedSlashCommand, sendQueued, value]);

  const selectQueueMode = useCallback((mode: QueueSendMode) => {
    setQueueCountdownPaused(true);
    setQueueActiveMode(mode);
    const option = mode === "steer" ? queueSteerOptionRef.current : queueDefaultOptionRef.current;
    option?.focus();
  }, []);

  useEffect(() => {
    if (!queueMenuVisible || !onFollowUp || queueCountdownPaused) return;
    const countdownTimers = [
      window.setTimeout(() => setQueueCountdown(2), 1000),
      window.setTimeout(() => setQueueCountdown(1), 2000),
      window.setTimeout(() => { void sendQueued("followup"); }, 3000),
    ];
    return () => countdownTimers.forEach(window.clearTimeout);
  }, [queueCountdownPaused, queueMenuVisible, onFollowUp, sendQueued]);

  const getNextSlashIndex = useCallback((direction: "up" | "down" | "left" | "right") => {
    const lastIndex = displayedSlashCommands.length - 1;
    if (lastIndex < 0) return 0;

    if (direction === "left") return Math.max(0, slashActiveIndex - 1);
    if (direction === "right") return Math.min(lastIndex, slashActiveIndex + 1);

    const currentNode = slashItemRefs.current[slashActiveIndex];
    if (!currentNode) {
      return direction === "down"
        ? Math.min(lastIndex, slashActiveIndex + 1)
        : Math.max(0, slashActiveIndex - 1);
    }

    const currentRect = currentNode.getBoundingClientRect();
    const currentX = currentRect.left + currentRect.width / 2;
    const currentY = currentRect.top + currentRect.height / 2;
    let bestIndex = -1;
    let bestScore = Number.POSITIVE_INFINITY;

    for (let index = 0; index <= lastIndex; index += 1) {
      if (index === slashActiveIndex) continue;
      const node = slashItemRefs.current[index];
      if (!node) continue;
      const rect = node.getBoundingClientRect();
      const candidateY = rect.top + rect.height / 2;
      const verticalDelta = candidateY - currentY;
      if (direction === "down" ? verticalDelta <= 4 : verticalDelta >= -4) continue;

      const candidateX = rect.left + rect.width / 2;
      const score = Math.abs(verticalDelta) * 1000 + Math.abs(candidateX - currentX);
      if (score < bestScore) {
        bestIndex = index;
        bestScore = score;
      }
    }

    if (bestIndex >= 0) return bestIndex;
    return direction === "down"
      ? Math.min(lastIndex, slashActiveIndex + 1)
      : Math.max(0, slashActiveIndex - 1);
  }, [displayedSlashCommands.length, slashActiveIndex]);

  const handleKeyDown = useCallback(
    (e: KeyboardEvent<HTMLTextAreaElement>) => {
      const nativeEvent = e.nativeEvent;
      const sendShortcut = e.key === "Enter" && !e.shiftKey && (!isMobile || e.ctrlKey || e.metaKey);
      const isLeftArrow =
        e.key === "ArrowLeft" ||
        e.code === "ArrowLeft" ||
        e.key === "\uF702" ||
        nativeEvent.keyCode === 37;
      const isRightArrow =
        e.key === "ArrowRight" ||
        e.code === "ArrowRight" ||
        e.key === "\uF703" ||
        nativeEvent.keyCode === 39;
      const recentlyComposed = Date.now() - lastCompositionEndAtRef.current < COMPOSITION_END_ENTER_GRACE_MS;
      const isComposing =
        isComposingRef.current ||
        nativeEvent.isComposing ||
        nativeEvent.keyCode === 229;

      if (sendShortcut && (isComposing || recentlyComposed)) {
        if (recentlyComposed) e.preventDefault();
        return;
      }

      // Tags sit immediately before the editor. With an empty editor,
      // Backspace removes the nearest mention first, then the command tag.
      if (
        e.key === "Backspace" &&
        !isComposing &&
        (selectedAtMentions.length > 0 || selectedSlashCommand) &&
        value.length === 0 &&
        e.currentTarget.selectionStart === 0 &&
        e.currentTarget.selectionEnd === 0
      ) {
        e.preventDefault();
        if (selectedAtMentions.length > 0) {
          removeSelectedAtMention(selectedAtMentions.length - 1);
        } else {
          removeSelectedSlashCommand();
        }
        return;
      }

      if (historyMenuOpen && !isComposing) {
        if (e.key === "ArrowDown") {
          e.preventDefault();
          setHistoryActiveIndex((i) => Math.min(Math.max(0, inputHistory.length - 1), i + 1));
          return;
        }
        if (e.key === "ArrowUp") {
          e.preventDefault();
          setHistoryActiveIndex((i) => Math.max(0, i - 1));
          return;
        }
        if (e.key === "Escape") {
          e.preventDefault();
          setHistoryMenuOpen(false);
          return;
        }
        if ((e.key === "Tab" || sendShortcut) && inputHistory[historyActiveIndex]) {
          e.preventDefault();
          applyHistoryInput(inputHistory[historyActiveIndex]);
          return;
        }
      }

      if (slashMenuOpen && slashQuery !== null) {
        if (e.key === "ArrowDown") {
          e.preventDefault();
          setSlashActiveIndex(getNextSlashIndex("down"));
          return;
        }
        if (e.key === "ArrowUp") {
          e.preventDefault();
          setSlashActiveIndex(getNextSlashIndex("up"));
          return;
        }
        if (isRightArrow) {
          e.preventDefault();
          setSlashActiveIndex(getNextSlashIndex("right"));
          return;
        }
        if (isLeftArrow) {
          e.preventDefault();
          setSlashActiveIndex(getNextSlashIndex("left"));
          return;
        }
        if (e.key === "Escape") {
          e.preventDefault();
          setSlashMenuOpen(false);
          return;
        }
        if ((e.key === "Tab" || sendShortcut) && displayedSlashCommands[slashActiveIndex]) {
          e.preventDefault();
          applySlashCommand(displayedSlashCommands[slashActiveIndex]);
          return;
        }
      }

      // @ file menu — skip while composing so IME candidate navigation
      // (arrows/Enter/Tab) is never intercepted.
      if (atMenuOpen && atQuery !== null && !isComposing) {
        if (e.key === "ArrowDown") {
          e.preventDefault();
          setAtActiveIndex((i) => Math.min(Math.max(0, atMatches.length - 1), i + 1));
          return;
        }
        if (e.key === "ArrowUp") {
          e.preventDefault();
          setAtActiveIndex((i) => Math.max(0, i - 1));
          return;
        }
        if (e.key === "Escape") {
          e.preventDefault();
          setAtMenuOpen(false);
          return;
        }
        if ((e.key === "Tab" || sendShortcut) && atMatches[atActiveIndex]) {
          e.preventDefault();
          applyAtCompletion(atMatches[atActiveIndex]);
          return;
        }
      }

      if (e.key === "ArrowUp" && !isComposing && !isStreaming && inputHistory.length > 0 && value.trim().length === 0) {
        e.preventDefault();
        setSlashMenuOpen(false);
        setAtMenuOpen(false);
        setHistoryActiveIndex(inputHistory.length - 1);
        setHistoryMenuOpen(true);
        return;
      }

      // Esc stops the agent when no slash/@/history menu or IME composition is active.
      if (e.key === "Escape" && !isComposing && isStreaming && onAbort) {
        e.preventDefault();
        onAbort();
        return;
      }

      // At a text boundary WKWebView can expose a macOS arrow function key as
      // printable text instead of leaving the caret in place. Prevent only
      // that no-op boundary action; native movement inside the text remains
      // responsible for selections, emoji, and composed graphemes.
      if (
        (isLeftArrow || isRightArrow) &&
        !isComposing &&
        !e.altKey &&
        !e.ctrlKey &&
        !e.metaKey &&
        !e.shiftKey
      ) {
        const ta = e.currentTarget;
        const selectionIsCollapsed = ta.selectionStart === ta.selectionEnd;
        const atStart = selectionIsCollapsed && ta.selectionStart === 0;
        const atEnd = selectionIsCollapsed && ta.selectionEnd === ta.value.length;
        if ((isLeftArrow && atStart) || (isRightArrow && atEnd)) {
          e.preventDefault();
          return;
        }
      }

      const queueShortcutMode = getQueueShortcutMode(e);
      if (
        queueShortcutMode &&
        !isComposing &&
        isStreaming &&
        canQueueStreamingMessage &&
        (queueShortcutMode === "steer" ? onSteer : onFollowUp)
      ) {
        e.preventDefault();
        void sendQueued(queueShortcutMode);
        return;
      }

      if (sendShortcut) {
        e.preventDefault();
        if (isStreaming && (onSteer || onFollowUp)) {
          if (e.altKey && onFollowUp) void sendQueued("followup");
          else requestQueuedSend();
        } else {
          handleSend();
        }
      }
    },
    [isMobile, isStreaming, canQueueStreamingMessage, onSteer, onFollowUp, onAbort, requestQueuedSend, slashMenuOpen, slashQuery, displayedSlashCommands, slashActiveIndex, applySlashCommand, sendQueued, handleSend, getNextSlashIndex, atMenuOpen, atQuery, atMatches, atActiveIndex, applyAtCompletion, historyMenuOpen, inputHistory, historyActiveIndex, applyHistoryInput, value, selectedAtMentions, selectedSlashCommand, removeSelectedAtMention, removeSelectedSlashCommand]
  );

  const handleInput = useCallback((e: React.FormEvent<HTMLTextAreaElement>) => {
    const ta = e.currentTarget;
    const rawValue = ta.value;
    const nextValue = stripMacOSArrowFunctionKeys(rawValue);

    // React's synthetic onChange can be skipped by macOS InputMethodKit in a
    // WKWebView. Sanitize the live DOM value from the native input event too.
    if (nextValue !== rawValue) {
      const nextSelectionStart = stripMacOSArrowFunctionKeys(
        rawValue.slice(0, ta.selectionStart),
      ).length;
      const nextSelectionEnd = stripMacOSArrowFunctionKeys(
        rawValue.slice(0, ta.selectionEnd),
      ).length;
      const selectionDirection = ta.selectionDirection;
      ta.value = nextValue;
      valueRef.current = nextValue;
      setValue(nextValue);
      ta.setSelectionRange(nextSelectionStart, nextSelectionEnd, selectionDirection);
      updateAtQuery(nextValue, nextSelectionStart);
    }

    resizeComposerTextarea(ta);
  }, [updateAtQuery]);

  const handleBeforeInput = useCallback((e: React.FormEvent<HTMLTextAreaElement>) => {
    const insertedText = (e.nativeEvent as InputEvent).data;
    if (insertedText && stripMacOSArrowFunctionKeys(insertedText) !== insertedText) {
      e.preventDefault();
    }
  }, []);

  const handlePaste = useCallback((e: React.ClipboardEvent) => {
    const items = Array.from(e.clipboardData?.items ?? []);
    const imageItems = items.filter((item) => item.type.startsWith("image/"));
    if (!imageItems.length) return;
    e.preventDefault();
    const files = imageItems.map((item) => item.getAsFile()).filter((f): f is File => f !== null);
    processImageFiles(files);
  }, [processImageFiles]);

  useEffect(() => {
    if (slashQuery === null) {
      setSlashMenuOpen(false);
      setSlashActiveIndex(0);
      slashCommandsRequestedRef.current = false;
      return;
    }
    setSlashMenuOpen(true);
    setSlashActiveIndex(0);
    if (!slashCommandsRequestedRef.current && onLoadSlashCommands) {
      slashCommandsRequestedRef.current = true;
      Promise.resolve(onLoadSlashCommands()).catch(() => {
        slashCommandsRequestedRef.current = false;
      });
    }
  }, [slashQuery, onLoadSlashCommands]);

  // Lazy-load skill dormancy (disable-model-invocation) each time the slash
  // palette opens, so toggles made in the skills panel are reflected on the
  // next open. Failures degrade silently to the unannotated palette.
  useEffect(() => {
    if (!slashMenuOpen || !cwd) return;
    const requestCwd = cwd;
    let cancelled = false;
    setSkillDormancyState({ cwd: requestCwd, values: {} });
    fetch(`/api/skills?cwd=${encodeURIComponent(requestCwd)}`)
      .then((res) => {
        if (!res.ok) throw new Error(`skills fetch failed: ${res.status}`);
        return res.json() as Promise<Partial<SkillsResponse>>;
      })
      .then((data) => {
        if (cancelled) return;
        const dormancy: Record<string, boolean> = {};
        for (const skill of data.skills ?? []) dormancy[skill.name] = skill.disableModelInvocation;
        setSkillDormancyState({ cwd: requestCwd, values: dormancy });
      })
      .catch(() => {
        if (!cancelled) setSkillDormancyState({ cwd: requestCwd, values: {} });
      });
    return () => {
      cancelled = true;
    };
  }, [slashMenuOpen, cwd]);

  useEffect(() => {
    setSkillMenuOpen(false);
    setSkillFilter("");
    setSkillMenuError(null);
  }, [cwd]);

  useEffect(() => {
    if (slashActiveIndex >= displayedSlashCommands.length) {
      setSlashActiveIndex(Math.max(0, displayedSlashCommands.length - 1));
    }
  }, [displayedSlashCommands.length, slashActiveIndex]);

  useEffect(() => {
    slashItemRefs.current.length = displayedSlashCommands.length;
  }, [displayedSlashCommands.length]);

  useEffect(() => {
    if (!slashMenuOpen) return;
    slashItemRefs.current[slashActiveIndex]?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [slashActiveIndex, slashMenuOpen]);

  useLayoutEffect(() => {
    if (!slashMenuOpen || slashQuery === null) {
      setSlashMenuMaxHeight(null);
      return;
    }

    const menu = slashMenuRef.current;
    if (!menu) return;

    let frameId: number | null = null;
    const update = () => {
      frameId = null;
      const nextHeight = getUpwardMenuMaxHeight(
        menu.getBoundingClientRect().bottom,
        getVisibleTopBoundary(menu),
      );
      setSlashMenuMaxHeight((current) => current === nextHeight ? current : nextHeight);
    };
    const scheduleUpdate = () => {
      if (frameId !== null) cancelAnimationFrame(frameId);
      frameId = requestAnimationFrame(update);
    };

    update();
    const anchorObserver = typeof ResizeObserver === "undefined" || !menu.parentElement
      ? null
      : new ResizeObserver(scheduleUpdate);
    if (menu.parentElement) anchorObserver?.observe(menu.parentElement);
    const viewport = window.visualViewport;
    viewport?.addEventListener("resize", scheduleUpdate);
    viewport?.addEventListener("scroll", scheduleUpdate);
    window.addEventListener("resize", scheduleUpdate);
    window.addEventListener("scroll", scheduleUpdate, true);

    return () => {
      anchorObserver?.disconnect();
      viewport?.removeEventListener("resize", scheduleUpdate);
      viewport?.removeEventListener("scroll", scheduleUpdate);
      window.removeEventListener("resize", scheduleUpdate);
      window.removeEventListener("scroll", scheduleUpdate, true);
      if (frameId !== null) cancelAnimationFrame(frameId);
    };
  }, [slashMenuOpen, slashQuery]);

  useLayoutEffect(() => {
    if (!skillMenuOpen) {
      setSkillMenuMaxHeight(null);
      return;
    }

    const menu = skillMenuRef.current?.lastElementChild;
    if (!(menu instanceof HTMLDivElement)) return;

    let frameId: number | null = null;
    const update = () => {
      frameId = null;
      const nextHeight = getUpwardMenuMaxHeight(
        menu.getBoundingClientRect().bottom,
        getVisibleTopBoundary(menu),
      );
      setSkillMenuMaxHeight((current) => current === nextHeight ? current : nextHeight);
    };
    const scheduleUpdate = () => {
      if (frameId !== null) cancelAnimationFrame(frameId);
      frameId = requestAnimationFrame(update);
    };

    update();
    const anchorObserver = typeof ResizeObserver === "undefined" || !menu.parentElement
      ? null
      : new ResizeObserver(scheduleUpdate);
    if (menu.parentElement) anchorObserver?.observe(menu.parentElement);
    const viewport = window.visualViewport;
    viewport?.addEventListener("resize", scheduleUpdate);
    viewport?.addEventListener("scroll", scheduleUpdate);
    window.addEventListener("resize", scheduleUpdate);
    window.addEventListener("scroll", scheduleUpdate, true);

    return () => {
      anchorObserver?.disconnect();
      viewport?.removeEventListener("resize", scheduleUpdate);
      viewport?.removeEventListener("scroll", scheduleUpdate);
      window.removeEventListener("resize", scheduleUpdate);
      window.removeEventListener("scroll", scheduleUpdate, true);
      if (frameId !== null) cancelAnimationFrame(frameId);
    };
  }, [skillMenuOpen]);

  // Build model options: prefer modelList (has provider info), fallback to modelNames
  const modelOptions: ModelOption[] = (() => {
    if (modelList && modelList.length > 0) {
      return modelList.map((m) => ({ provider: m.provider, modelId: m.id, name: m.name })).sort(compareModelOptions);
    }
    return Object.entries(modelNames ?? {}).map(([modelId, name]) => ({
      provider: model?.provider ?? "unknown",
      modelId,
      name,
    })).sort(compareModelOptions);
  })();
  const filteredModelOptions = filterModelOptions(modelOptions, modelFilter);
  const showModelFilter = modelOptions.length > MODEL_FILTER_THRESHOLD;

  // Group options by provider, preserving insertion order
  const modelsByProvider: { provider: string; options: ModelOption[] }[] = [];
  for (const opt of filteredModelOptions) {
    const group = modelsByProvider.find((g) => g.provider === opt.provider);
    if (group) group.options.push(opt);
    else modelsByProvider.push({ provider: opt.provider, options: [opt] });
  }

  const currentName = model
      ? (modelOptions.find((o) => o.modelId === model.modelId && o.provider === model.provider)?.name ?? model.modelId)
      : null;

  const compactSavedTokens = compactResult
    ? Math.max(0, compactResult.tokensBefore - compactResult.estimatedTokensAfter)
    : 0;
  const compactResultText = compactResult
    ? `${compactResult.reason && compactResult.reason !== "manual" ? `${compactResult.reason[0].toUpperCase()}${compactResult.reason.slice(1)} ` : t("chat.compacted")} ${formatTokenCount(compactResult.tokensBefore)} -> ${formatTokenCount(compactResult.estimatedTokensAfter)} tokens (${t("chat.tokensSaved", { saved: formatTokenCount(compactSavedTokens) })})`
    : null;
  const thinkingDisplayLabel = (() => {
    const lvl = thinkingLevel ?? "auto";
    if (lvl === "auto" || !thinkingLevelMap) return lvl;
    return thinkingLevelMap[lvl] ?? lvl;
  })();
  const toolPresetLabel = Object.entries(TOOL_PRESET_MAP).find(([, v]) => v === (toolPreset ?? "default"))?.[0] ?? "default";
  // 普通对话模式下工具集是空的，预设切换没有任何可应用的对象。
  const toolPresetDisabled = isStreaming || chatModeActive;
  const skillMenuSkills = cwd && skillMenuState?.cwd === cwd ? skillMenuState.skills : [];
  const skillMenuGroups = buildSkillMenuGroups(skillMenuSkills, skillFilter);
  const skillMenuMatchCount = skillMenuGroups.reduce((count, group) => count + group.skills.length, 0);
  const skillMenuGroupLabels: Record<SkillMenuGroupId, string> = {
    official: t("chat.officialSkills"),
    project: t("chat.projectSkills"),
    global: t("chat.globalSkills"),
    other: t("chat.otherSkills"),
  };

  // Close dropdowns on outside click
  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (
        dropdownRef.current && !dropdownRef.current.contains(e.target as Node) &&
        modelDropdownPanelRef.current && !modelDropdownPanelRef.current.contains(e.target as Node)
      ) {
        setModelDropdownOpen(false);
        setModelFilter("");
      }
      if (toolDropdownRef.current && !toolDropdownRef.current.contains(e.target as Node)) {
        setToolDropdownOpen(false);
      }
      if (thinkingDropdownRef.current && !thinkingDropdownRef.current.contains(e.target as Node)) {
        setThinkingDropdownOpen(false);
      }
      if (controlsMenuRef.current && !controlsMenuRef.current.contains(e.target as Node)) {
        setControlsMenuOpen(false);
      }
      if (historyMenuRef.current && !historyMenuRef.current.contains(e.target as Node) && !textareaRef.current?.contains(e.target as Node)) {
        setHistoryMenuOpen(false);
      }
      if (skillMenuRef.current && !skillMenuRef.current.contains(e.target as Node)) {
        setSkillMenuOpen(false);
        setSkillFilter("");
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, []);

  useEffect(() => {
    if (!isMobile) setControlsMenuOpen(false);
  }, [isMobile]);

  // The queue menu lives outside the scrolling field, which would otherwise
  // clip it, so it carries its own keyboard handling.
  const handleQueueMenuKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!queueMenuVisible) return;
    const shortcutMode = getQueueShortcutMode(event);
    if (shortcutMode && (shortcutMode === "steer" ? onSteer : onFollowUp)) {
      event.preventDefault();
      event.stopPropagation();
      if (!event.repeat) void sendQueued(shortcutMode);
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      setQueueMenuOpen(false);
      textareaRef.current?.focus();
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      event.stopPropagation();
      selectQueueMode(queueActiveMode === "followup" ? "steer" : "followup");
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      event.stopPropagation();
      if (!event.repeat) void sendQueued(queueActiveMode);
    }
  };

  return (
    <div
      style={{
        flexShrink: 0,
        background: "transparent",
        padding: "0 16px 8px",
        paddingRight: 16,
      }}
    >
      {/* Hidden file input */}
      <input
        ref={fileInputRef}
        type="file"
        accept="image/*"
        multiple
        style={{ display: "none" }}
        onChange={(e) => {
          const files = Array.from(e.target.files ?? []);
          processImageFiles(files);
          e.target.value = "";
        }}
      />
      <div style={{ maxWidth: "var(--chat-content-max-width, 820px)", margin: "0 auto" }}>
        <ModelErrorBanner error={modelError} />
        <ModelScopeWarningBanner warnings={modelScopeWarnings} />
        {/* Queued steering / follow-up messages (delivered by pi on upcoming turns) */}
        {((queuedMessages?.steering.length ?? 0) + (queuedMessages?.followUp.length ?? 0)) > 0 && (
          <div style={{
            marginBottom: 8,
            border: "1px solid var(--border)",
            borderRadius: 6,
            background: "var(--bg-panel)",
            padding: "5px 0",
          }}>
            <div style={{
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              gap: 8,
              padding: "2px 8px 4px 10px",
            }}>
              <span style={{
                fontSize: 10,
                fontFamily: "var(--font-mono)",
                color: "var(--text-dim)",
                textTransform: "uppercase",
                letterSpacing: 0.4,
              }}>
                {t("chat.queued", { count: (queuedMessages?.steering.length ?? 0) + (queuedMessages?.followUp.length ?? 0) })}
              </span>
              {onRecallQueue && (
                <button
                  onClick={onRecallQueue}
                   title={t("chat.recallTitle")}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 6,
                    padding: "4px 12px",
                    fontSize: 12,
                    color: "var(--text)",
                    background: "transparent",
                    border: "1px solid var(--border)",
                    borderRadius: 7,
                    cursor: "pointer",
                    transition: "background 0.12s, border-color 0.12s",
                    whiteSpace: "nowrap",
                  }}
                  onMouseEnter={(e) => {
                    e.currentTarget.style.background = "var(--bg-hover)";
                    e.currentTarget.style.borderColor = "color-mix(in srgb, var(--accent) 45%, var(--border))";
                  }}
                  onMouseLeave={(e) => {
                    e.currentTarget.style.background = "transparent";
                    e.currentTarget.style.borderColor = "var(--border)";
                  }}
                >
                  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <polyline points="9 14 4 9 9 4" />
                    <path d="M20 20v-7a4 4 0 0 0-4-4H4" />
                  </svg>
                   {t("chat.recall")}
                </button>
              )}
            </div>
            {queuedMessages?.steering.map((text, i) => (
              <QueuedMessageRow key={`steer-${i}`} kind="steer" text={text} />
            ))}
            {queuedMessages?.followUp.map((text, i) => (
              <QueuedMessageRow key={`followup-${i}`} kind="follow-up" text={text} />
            ))}
          </div>
        )}
        {/* Retry banner */}
        {retryInfo && (
          <div style={{
            marginBottom: 8, padding: "5px 10px",
            background: "rgba(234,179,8,0.08)", border: "1px solid rgba(234,179,8,0.25)",
            borderRadius: 6, fontSize: 12, color: "rgba(180,130,0,0.9)",
            display: "flex", alignItems: "center", gap: 6,
          }}>
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}>
              <path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" />
              <path d="M3 3v5h5" />
            </svg>
             {t("chat.retrying", { attempt: retryInfo.attempt, max: retryInfo.maxAttempts })}{retryInfo.errorMessage && <span style={{ opacity: 0.7, marginLeft: 4 }}>— {retryInfo.errorMessage}</span>}
          </div>
        )}
        {promptFailure && onRetryPrompt && (
          <div style={{
            marginBottom: 8, padding: "7px 10px",
            background: "rgba(239,68,68,0.08)", border: "1px solid rgba(239,68,68,0.25)",
            borderRadius: 6, fontSize: 12, color: "var(--text-muted)",
            display: "flex", alignItems: "center", justifyContent: "space-between", gap: 10,
          }}>
            <span style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
              {t("chat.promptFailed", { error: promptFailure })}
            </span>
            <button
              type="button"
              onClick={onRetryPrompt}
              disabled={isStreaming}
              title={t("chat.retryPrompt")}
              style={{
                display: "inline-flex", alignItems: "center", gap: 5, flexShrink: 0,
                padding: "4px 8px", border: "1px solid rgba(239,68,68,0.35)", borderRadius: 5,
                background: "transparent", color: "#dc2626", cursor: isStreaming ? "not-allowed" : "pointer",
                fontSize: 12, fontWeight: 600, opacity: isStreaming ? 0.55 : 1,
              }}
            >
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M3 12a9 9 0 1 0 3-6.7" />
                <path d="M3 3v5h5" />
              </svg>
              {t("chat.retryPrompt")}
            </button>
          </div>
        )}
        {compactResultText && (
          <div style={{
            marginBottom: 8, padding: "5px 10px",
            background: "rgba(16,185,129,0.08)", border: "1px solid rgba(16,185,129,0.24)",
            borderRadius: 6, fontSize: 12, color: "rgba(5,150,105,0.95)",
            display: "flex", alignItems: "center", gap: 6,
          }}>
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}>
              <polyline points="20 6 9 17 4 12" />
            </svg>
            {compactResultText}
          </div>
        )}
        {compactError && (
          <div
            role="alert"
            style={{
              marginBottom: 8,
              padding: "7px 10px",
              background: "rgba(239,68,68,0.07)",
              border: "1px solid rgba(239,68,68,0.3)",
              borderRadius: 6,
              color: "#ef4444",
              fontFamily: "var(--font-mono)",
              fontSize: 12,
              lineHeight: 1.5,
              whiteSpace: "pre-wrap",
              overflowWrap: "anywhere",
            }}
          >
            {compactError}
          </div>
        )}
        {/* Image previews */}
        {attachedImages.length > 0 && (
          <div style={{ display: "flex", gap: 6, marginBottom: 6, flexWrap: "wrap" }}>
            {attachedImages.map((img, i) => (
              <div key={i} style={{ position: "relative", flexShrink: 0 }}>
                {/* eslint-disable-next-line @next/next/no-img-element */}
                <img
                  src={img.previewUrl}
                  alt=""
                  style={{ width: 56, height: 56, objectFit: "cover", borderRadius: 6, border: "1px solid var(--border)", display: "block" }}
                />
                <button
                  onClick={() => removeImage(i)}
                  style={{
                    position: "absolute", top: -4, right: -4,
                    width: 16, height: 16, borderRadius: "50%",
                    background: "var(--bg-panel)", border: "1px solid var(--border)",
                    display: "flex", alignItems: "center", justifyContent: "center",
                    cursor: "pointer", padding: 0, color: "var(--text-muted)",
                  }}
                >
                  <svg width="8" height="8" viewBox="0 0 8 8" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
                    <line x1="1" y1="1" x2="7" y2="7" /><line x1="7" y1="1" x2="1" y2="7" />
                  </svg>
                </button>
              </div>
            ))}
          </div>
        )}

        {/* Main input */}
        <div style={{ position: "relative", minWidth: 0 }}>
          {historyMenuOpen && inputHistory.length > 0 && (
            <div
              ref={historyMenuRef}
              style={{
                position: "absolute",
                left: 0,
                right: 0,
                bottom: "calc(100% + 8px)",
                zIndex: 120,
                background: "var(--bg)",
                border: "1px solid var(--border)",
                borderRadius: 8,
                boxShadow: "0 -6px 20px rgba(0,0,0,0.12)",
                overflow: "hidden",
                maxHeight: "min(44vh, 360px)",
              }}
            >
              <div
                title="Input history"
                style={{
                  height: 30,
                  padding: "0 10px",
                  borderBottom: "1px solid var(--border)",
                  display: "flex",
                  alignItems: "center",
                  color: "var(--text-dim)",
                }}
              >
                <svg
                  width="14"
                  height="14"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.8"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  aria-hidden="true"
                >
                  <path d="M3 12a9 9 0 1 0 3-6.7" />
                  <path d="M3 4v5h5" />
                  <path d="M12 7v5l3 2" />
                </svg>
              </div>
              <div style={{ maxHeight: "calc(min(44vh, 360px) - 31px)", overflowY: "auto", padding: 4 }}>
                {inputHistory.map((item, index) => {
                  const active = index === historyActiveIndex;
                  return (
                    <button
                      key={`${index}:${item}`}
                      ref={(node) => {
                        historyItemRefs.current[index] = node;
                      }}
                      type="button"
                      onMouseDown={(e) => {
                        e.preventDefault();
                        applyHistoryInput(item);
                      }}
                      onMouseEnter={() => setHistoryActiveIndex(index)}
                      style={{
                        width: "100%",
                        display: "flex",
                        alignItems: "flex-start",
                        gap: 8,
                        padding: "7px 8px",
                        border: "none",
                        borderRadius: 6,
                        background: active ? "var(--bg-selected)" : "none",
                        color: "var(--text)",
                        cursor: "pointer",
                        textAlign: "left",
                        fontSize: 12.5,
                        lineHeight: 1.45,
                      }}
                    >
                      <span style={{ flexShrink: 0, fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--text-dim)", paddingTop: 1 }}>
                        {index + 1}
                      </span>
                      <span style={{ minWidth: 0, display: "-webkit-box", WebkitBoxOrient: "vertical", WebkitLineClamp: 2, overflow: "hidden", overflowWrap: "anywhere" }}>
                        {item}
                      </span>
                    </button>
                  );
                })}
              </div>
            </div>
          )}
          {slashMenuOpen && slashQuery !== null && (
            <div
              ref={slashMenuRef}
              style={{
                position: "absolute",
                left: 0,
                right: 0,
                bottom: "calc(100% + 8px)",
                zIndex: 120,
                background: "var(--bg)",
                border: "1px solid var(--border)",
                borderRadius: 8,
                boxShadow: "0 -6px 20px rgba(0,0,0,0.12)",
                overflow: "hidden",
                boxSizing: "border-box",
                display: "flex",
                flexDirection: "column",
                maxHeight: slashMenuMaxHeight === null
                  ? "min(72.8vh, 598px)"
                  : `min(72.8vh, 598px, ${slashMenuMaxHeight}px)`,
              }}
            >
              <div
                style={{
                  padding: "8px 10px",
                  borderBottom: "1px solid var(--border)",
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "space-between",
                  gap: 8,
                  fontSize: 11,
                  color: "var(--text-dim)",
                  flexShrink: 0,
                }}
              >
                 <span>{slashCommandsLoading ? t("chat.loadingCommands") : t("chat.slashCommands", { label: slashCommandCountLabel })}</span>
                 <span style={{ fontFamily: "var(--font-mono)" }}>{t("chat.tabEnter")}</span>
              </div>
              <div style={{ flex: "1 1 auto", minHeight: 0, overflowY: "auto", padding: 10 }}>
                {!slashCommandsLoading && filteredSlashCommands.length === 0 ? (
                  <div style={{ padding: "2px 2px 4px", fontSize: 12, color: "var(--text-dim)" }}>
                     {t("chat.noCommands")}
                  </div>
                ) : (
                  groupedSlashCommands.map((group) => (
                    <section key={group.source} style={{ marginBottom: 12 }}>
                      <div
                        style={{
                          position: "sticky",
                          top: -10,
                          zIndex: 1,
                          display: "flex",
                          alignItems: "center",
                          justifyContent: "space-between",
                          gap: 8,
                          padding: "4px 0 6px",
                          background: "var(--bg)",
                          color: "var(--text-dim)",
                          fontSize: 10,
                          fontWeight: 600,
                          textTransform: "uppercase",
                        }}
                      >
                           <span>{t(SLASH_SOURCE_GROUP_LABEL_KEYS[group.source])}</span>
                        <span style={{ fontFamily: "var(--font-mono)", fontWeight: 500 }}>{group.items.length}</span>
                      </div>
                      <div
                        style={{
                          display: "grid",
                          gridTemplateColumns: "repeat(auto-fit, minmax(220px, 1fr))",
                          gap: 8,
                        }}
                      >
                        {group.items.map(({ command, index }) => {
                          const active = index === slashActiveIndex;
                          const dormant = isDormantSkillCommand(command, skillDormancy);
                          return (
                            <button
                              key={`${command.source}:${command.name}`}
                              ref={(node) => {
                                slashItemRefs.current[index] = node;
                              }}
                              type="button"
                              onMouseDown={(e) => {
                                e.preventDefault();
                                applySlashCommand(command);
                              }}
                              onMouseEnter={() => setSlashActiveIndex(index)}
                              style={{
                                width: "100%",
                                minWidth: 0,
                                minHeight: 58,
                                display: "flex",
                                flexDirection: "column",
                                gap: 4,
                                justifyContent: "center",
                                padding: "9px 10px",
                                border: `1px solid ${active ? "var(--accent)" : "var(--border)"}`,
                                borderRadius: 7,
                                background: active ? "var(--bg-selected)" : "var(--bg-panel)",
                                color: "var(--text)",
                                cursor: "pointer",
                                textAlign: "left",
                                boxShadow: active ? "0 0 0 1px color-mix(in srgb, var(--accent) 28%, transparent)" : "none",
                              }}
                            >
                              <span style={{
                                fontSize: 13,
                                fontFamily: "var(--font-mono)",
                                overflowWrap: "anywhere",
                                wordBreak: "break-word",
                                color: dormant ? "var(--text-dim)" : undefined,
                              }}>
                                /{command.name}
                                {dormant && (
                                  <span style={{
                                    marginLeft: 6,
                                    padding: "0 4px",
                                    border: "1px solid var(--border)",
                                    borderRadius: 3,
                                    fontSize: 9,
                                    color: "var(--text-dim)",
                                    whiteSpace: "nowrap",
                                  }}>
                                    {t("chat.dormant")}
                                  </span>
                                )}
                              </span>
                               {command.description && (
                                <span style={{
                                  display: "-webkit-box",
                                  WebkitBoxOrient: "vertical",
                                  WebkitLineClamp: 2,
                                  overflow: "hidden",
                                  fontSize: 11,
                                  lineHeight: 1.35,
                                  color: "var(--text-dim)",
                                }}>
                                   {getSlashDescription(command, t)}
                                </span>
                              )}
                            </button>
                          );
                        })}
                      </div>
                    </section>
                  ))
                )}
              </div>
            </div>
          )}
          {atMenuOpen && atQuery !== null && (() => {
            const indexLoading = fileIndexLoading && (!fileIndex || fileIndex.cwd !== cwd);
             const matchCountLabel = atMatches.length === 1 ? t("chat.match") : t("chat.matches", { count: atMatches.length });
            // With a truncated index, local results are provisional — the
            // debounced server search over the full listing replaces them.
            const truncatedHint = fileIndex?.truncated && !serverResultInUse
               ? (atQuery.query ? t("chat.searchingAll") : t("chat.indexTruncated"))
              : "";
            return (
              <div
                style={{
                  position: "absolute",
                  left: 0,
                  right: 0,
                  bottom: "calc(100% + 8px)",
                  zIndex: 120,
                  background: "var(--bg)",
                  border: "1px solid var(--border)",
                  borderRadius: 8,
                  boxShadow: "0 -6px 20px rgba(0,0,0,0.12)",
                  overflow: "hidden",
                  maxHeight: "min(48vh, 400px)",
                }}
              >
                <div
                  style={{
                    padding: "8px 10px",
                    borderBottom: "1px solid var(--border)",
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "space-between",
                    gap: 8,
                    fontSize: 11,
                    color: "var(--text-dim)",
                  }}
                >
                  <span>
                    {indexLoading
                       ? t("chat.loadingFiles")
                       : t("chat.files", { label: matchCountLabel, hint: truncatedHint })}
                  </span>
                   <span style={{ fontFamily: "var(--font-mono)" }}>{t("chat.tabEnter")}</span>
                </div>
                <div style={{ maxHeight: "calc(min(48vh, 400px) - 34px)", overflowY: "auto", padding: 4 }}>
                  {!indexLoading && atMatches.length === 0 ? (
                    <div style={{ padding: "6px 8px", fontSize: 12, color: "var(--text-dim)" }}>
                       {needsServerSearch && !serverResultInUse ? t("chat.searching") : t("chat.noMatchingFiles")}
                    </div>
                  ) : (
                    atMatches.map((entry, index) => {
                      const active = index === atActiveIndex;
                      const name = entry.path.split("/").pop() ?? entry.path;
                      const dirPrefix = entry.path.slice(0, entry.path.length - name.length);
                      return (
                        <button
                          key={`${entry.isDir ? "d" : "f"}:${entry.path}`}
                          ref={(node) => {
                            atItemRefs.current[index] = node;
                          }}
                          type="button"
                          onMouseDown={(e) => {
                            e.preventDefault();
                            applyAtCompletion(entry);
                          }}
                          onMouseEnter={() => setAtActiveIndex(index)}
                          style={{
                            width: "100%",
                            display: "flex",
                            alignItems: "center",
                            gap: 8,
                            padding: "6px 8px",
                            border: "none",
                            borderRadius: 6,
                            background: active ? "var(--bg-selected)" : "none",
                            color: "var(--text)",
                            cursor: "pointer",
                            textAlign: "left",
                            fontSize: 12.5,
                            fontFamily: "var(--font-mono)",
                          }}
                        >
                          <span style={{ flexShrink: 0, display: "flex", alignItems: "center" }}>
                            {entry.isDir ? <FolderIcon size={14} /> : getFileIcon(name, 14)}
                          </span>
                          <span style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                            {dirPrefix && <span style={{ color: "var(--text-dim)" }}>{dirPrefix}</span>}
                            {name}
                            {entry.isDir && <span style={{ color: "var(--text-dim)" }}>/</span>}
                          </span>
                        </button>
                      );
                    })
                  )}
                </div>
              </div>
            );
          })()}
          {queueMenuVisible && (
            <div
              id={queueMenuId}
              role="menu"
              aria-label={t("chat.send")}
              className="chat-composer-queue-menu"
              onKeyDown={handleQueueMenuKeyDown}
            >
              {onSteer && (
                <button
                  ref={queueSteerOptionRef}
                  role="menuitemradio"
                  aria-checked={queueActiveMode === "steer"}
                  tabIndex={queueActiveMode === "steer" ? 0 : -1}
                  className={queueActiveMode === "steer" ? "chat-composer-queue-selected" : undefined}
                  onFocus={() => setQueueActiveMode("steer")}
                  onMouseEnter={() => setQueueActiveMode("steer")}
                  onClick={() => { void sendQueued("steer"); }}
                >
                  <span>{t("chat.steer")}</span>
                  <kbd className="chat-composer-queue-shortcut">Alt+F</kbd>
                </button>
              )}
              {onFollowUp && (
                <button
                  ref={queueDefaultOptionRef}
                  role="menuitemradio"
                  aria-checked={queueActiveMode === "followup"}
                  tabIndex={queueActiveMode === "followup" ? 0 : -1}
                  className={queueActiveMode === "followup" ? "chat-composer-queue-selected" : undefined}
                  onFocus={() => setQueueActiveMode("followup")}
                  onMouseEnter={() => setQueueActiveMode("followup")}
                  onClick={() => { void sendQueued("followup"); }}
                >
                  <span>{t("chat.followUp")}</span>
                  <span className="chat-composer-queue-meta">
                    {!queueCountdownPaused && (
                      <span className="chat-composer-queue-countdown" aria-label={`${queueCountdown}s`}>{queueCountdown}s</span>
                    )}
                    <kbd className="chat-composer-queue-shortcut">Alt+Q</kbd>
                  </span>
                </button>
              )}
            </div>
          )}
          <div className="chat-composer-shell">
          {showJumpToLatest && onJumpToLatest && (
            // 「回到最新」不是浮在消息区中间，而是从输入框上沿长出来的一个疙瘩：
            // 底部两撇是外撇的切线，矩形 + border 做不出来，所以轮廓交给这个
            // 内联 SVG（装饰性形状，图标仍然只用 lucide）。填充一直铺到输入框
            // 边框的内沿，等于把那条线在疙瘩宽度内顶开，疙瘩与框内连成一体。
            <button
              type="button"
              className="chat-jump-to-latest"
              onClick={onJumpToLatest}
              aria-label={t("chat.jumpToLatest")}
              title={t("chat.jumpToLatest")}
            >
              <svg className="chat-jump-to-latest__shape" viewBox="0 0 84 22" aria-hidden="true" focusable="false">
                <path d="M0.5 22C3.5 16 11 4.2 42 4.2S80.5 16 83.5 22" />
              </svg>
              <ChevronDown size={13} strokeWidth={2} aria-hidden="true" />
            </button>
          )}
          <div
            className="chat-composer-field"
            ref={composerFieldRef}
          >
          <button
            type="button"
            className="chat-composer-mark-button"
            onClick={() => onChatModeChange?.(chatModeActive ? "work" : "chat")}
            disabled={!onChatModeChange}
            aria-pressed={chatModeActive}
            aria-label={chatModeActive ? t("chat.switchToWorkMode") : t("chat.switchToChatMode")}
            title={chatModeActive ? t("chat.switchToWorkMode") : t("chat.switchToChatMode")}
          >
            <span
              aria-hidden="true"
              className="chat-composer-mark"
              style={{
                backgroundColor: chatModeActive ? "var(--accent)" : "var(--text)",
                WebkitMaskImage: "url('/icons/pi-input-mark.png')",
                maskImage: "url('/icons/pi-input-mark.png')",
                WebkitMaskPosition: "center",
                maskPosition: "center",
                WebkitMaskRepeat: "no-repeat",
                maskRepeat: "no-repeat",
                WebkitMaskSize: "contain",
                maskSize: "contain",
                flexShrink: 0,
                width: 28,
                height: 24,
                marginTop: 2,
                pointerEvents: "none",
                userSelect: "none",
                WebkitUserSelect: "none",
              }}
            />
          </button>
          {selectedSlashCommand && (() => {
            const tagStyle = COMMAND_TAG_STYLES[selectedSlashCommand.kind];
            return (
              <span
                style={{
                  display: "inline-flex",
                  alignItems: "center",
                  gap: 4,
                  flexShrink: 1,
                  minWidth: 0,
                  maxWidth: isMobile ? "min(34%, 112px)" : "min(42%, 320px)",
                  padding: "4px 5px 4px 8px",
                  border: `1px solid ${tagStyle.border}`,
                  borderRadius: 6,
                  background: tagStyle.background,
                  color: tagStyle.color,
                  fontFamily: "var(--font-mono)",
                  fontSize: 12,
                  lineHeight: 1.2,
                }}
                title={`/${selectedSlashCommand.name}`}
              >
                <span style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                  /{selectedSlashCommand.name}
                </span>
                <button
                  type="button"
                  onClick={removeSelectedSlashCommand}
                  aria-label={`Remove /${selectedSlashCommand.name}`}
                  title="Remove command"
                  style={{
                    width: 16,
                    height: 16,
                    flexShrink: 0,
                    display: "inline-flex",
                    alignItems: "center",
                    justifyContent: "center",
                    padding: 0,
                    border: "none",
                    borderRadius: 4,
                    background: "transparent",
                    color: "currentColor",
                    cursor: "pointer",
                    opacity: 0.8,
                  }}
                  onMouseDown={(e) => e.stopPropagation()}
                >
                  <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" aria-hidden="true">
                    <line x1="2" y1="2" x2="8" y2="8" />
                    <line x1="8" y1="2" x2="2" y2="8" />
                  </svg>
                </button>
              </span>
            );
          })()}
          {selectedAtMentions.length > 0 && (
            <span style={{ display: "inline-flex", alignItems: "center", gap: 6, minWidth: 0, maxWidth: "min(52%, 440px)", overflowX: "auto", flexShrink: 1 }}>
              {selectedAtMentions.map((mention, index) => (
                <span
                  key={`${mention.text}:${index}`}
                  title={mention.text}
                  style={{
                    display: "inline-flex", alignItems: "center", gap: 4,
                    minWidth: 0, maxWidth: 220, flexShrink: 0,
                    padding: "4px 5px 4px 8px",
                    border: `1px solid ${AT_MENTION_TAG_STYLE.border}`,
                    borderRadius: 6,
                    background: AT_MENTION_TAG_STYLE.background,
                    color: AT_MENTION_TAG_STYLE.color,
                    fontFamily: "var(--font-mono)", fontSize: 12, lineHeight: 1.2,
                  }}
                >
                  <span style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{mention.text}</span>
                  <button
                    type="button"
                    onClick={() => removeSelectedAtMention(index)}
                    aria-label={`Remove ${mention.text}`}
                    title="Remove mention"
                    style={{ width: 16, height: 16, flexShrink: 0, display: "inline-flex", alignItems: "center", justifyContent: "center", padding: 0, border: "none", borderRadius: 4, background: "transparent", color: "currentColor", cursor: "pointer", opacity: 0.8 }}
                    onMouseDown={(event) => event.stopPropagation()}
                  >
                    <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" aria-hidden="true">
                      <line x1="2" y1="2" x2="8" y2="8" />
                      <line x1="8" y1="2" x2="2" y2="8" />
                    </svg>
                  </button>
                </span>
              ))}
            </span>
          )}
          <textarea
            ref={textareaRef}
            value={value}
            onChange={(e) => {
              const rawValue = e.target.value;
              const nextValue = stripMacOSArrowFunctionKeys(rawValue);
              const rawSelectionStart = e.target.selectionStart;
              const rawSelectionEnd = e.target.selectionEnd;
              const selectionDirection = e.target.selectionDirection;
              const nextSelectionStart = rawSelectionStart === null
                ? null
                : stripMacOSArrowFunctionKeys(rawValue.slice(0, rawSelectionStart)).length;
              const nextSelectionEnd = rawSelectionEnd === null
                ? null
                : stripMacOSArrowFunctionKeys(rawValue.slice(0, rawSelectionEnd)).length;

              valueRef.current = nextValue;
              setValue(nextValue);
              setHistoryMenuOpen(false);
              updateAtQuery(nextValue, nextSelectionStart);

              // WKWebView can occasionally submit macOS arrow function-key
              // characters as text. Keep the controlled fallback from moving
              // the caret when it removes those private-use characters.
              if (
                nextValue !== rawValue &&
                nextSelectionStart !== null &&
                nextSelectionEnd !== null
              ) {
                requestAnimationFrame(() => {
                  const ta = textareaRef.current;
                  if (ta?.value === nextValue) {
                    ta.setSelectionRange(nextSelectionStart, nextSelectionEnd, selectionDirection);
                  }
                });
              }
            }}
            onSelect={(e) => {
              const el = e.currentTarget;
              updateAtQuery(el.value, el.selectionStart);
            }}
            onKeyDown={handleKeyDown}
            onCompositionStart={() => {
              isComposingRef.current = true;
            }}
            onCompositionEnd={(e) => {
              isComposingRef.current = false;
              lastCompositionEndAtRef.current = Date.now();
              const el = e.currentTarget;
              updateAtQuery(el.value, el.selectionStart);
            }}
            onBeforeInput={handleBeforeInput}
            onInput={handleInput}
            onPaste={handlePaste}
            placeholder={
              isMobile && selectedSlashCommand
                ? t("chat.commandArgsPlaceholder")
                : isStreaming && (onSteer || onFollowUp)
                ? t("chat.steerPlaceholder")
                : isStreaming ? t("chat.agentPlaceholder")
                : chatModeActive ? t("chat.chatModePlaceholder")
                : t("chat.messagePlaceholder")
            }
            rows={1}
            style={{
              flex: 1,
              alignSelf: "center",
              display: "block",
              minWidth: 0,
              width: "100%",
              margin: 0,
              padding: 0,
              background: "none",
              border: "none",
              outline: "none",
              resize: "none",
              color: "var(--text)",
              fontSize: "var(--chat-content-font-size, 14px)",
              // 默认仍是 24px：只有用户调大字号时行高才跟着长，单行输入框的
              // 垂直居中（24px 行高 + minHeight 24）因此保持不变。
              lineHeight: "calc(24px + var(--chat-font-size-offset, 0px))",
              fontFamily: "inherit",
              boxSizing: "border-box",
              minHeight: 24,
              overflow: "hidden",
            }}
          />

          <span className="chat-composer-actions-spacer" aria-hidden="true" />
          </div>
          <div className="chat-composer-actions" ref={composerActionsRef}>
          {isStreaming ? (
            <div
              ref={queueActionsRef}
              className="chat-composer-queue-actions"
              onKeyDown={handleQueueMenuKeyDown}
            >
              {canQueueStreamingMessage && (onSteer || onFollowUp) && (
                <button
                  onClick={requestQueuedSend}
                  aria-label={t("chat.send")}
                  title={t("chat.send")}
                  aria-haspopup={onSteer && onFollowUp ? "menu" : undefined}
                  aria-expanded={onSteer && onFollowUp ? queueMenuVisible : undefined}
                  aria-controls={queueMenuVisible ? queueMenuId : undefined}
                  className="chat-composer-queue-button chat-composer-queue-primary"
                >
                  <svg width="12" height="12" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M5 1 L9 5 L5 9" /><line x1="1" y1="5" x2="9" y2="5" />
                  </svg>
                </button>
              )}
                <button
                  onClick={onAbort}
                  title={t("chat.stopAgent")}
                  aria-label={t("chat.stopAgent")}
                  className="chat-composer-queue-button chat-composer-stop"
                >
                  <svg width="12" height="12" viewBox="0 0 12 12" fill="currentColor" aria-hidden="true">
                    <rect x="2" y="2" width="8" height="8" rx="1.5" />
                  </svg>
                </button>
            </div>
          ) : (
            <button
              onClick={handleSend}
              disabled={!value.trim() && !attachedImages.length}
              title={t("chat.send")}
              aria-label={t("chat.send")}
              className="chat-composer-send"
              style={{
                flexShrink: 0,
                alignSelf: "flex-end",
                display: "flex", alignItems: "center", justifyContent: "center", gap: isMobile ? 0 : 6,
                width: isMobile ? 36 : undefined,
                height: isMobile ? 36 : undefined,
                padding: isMobile ? 0 : "7px 14px",
                background: (value.trim() || attachedImages.length) ? "var(--accent)" : "var(--bg-panel)",
                border: "none",
                borderRadius: 8,
                color: (value.trim() || attachedImages.length) ? "#fff" : "var(--text-dim)",
                cursor: (value.trim() || attachedImages.length) ? "pointer" : "not-allowed",
                fontSize: 13,
                fontWeight: 600,
                letterSpacing: "-0.01em",
                boxShadow: (value.trim() || attachedImages.length) ? "0 1px 3px rgba(37,99,235,0.25)" : "none",
                transition: "background 0.15s, box-shadow 0.15s",
              }}
            >
              <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <line x1="2" y1="7" x2="11" y2="7" />
                <polyline points="7.5 3 12 7 7.5 11" />
              </svg>
              {!isMobile && t("chat.send")}
            </button>
          )}
          </div>
          </div>
        </div>

        {/* Bash mode status label */}
        {bashMode && (
          <div className="text-xs px-2 py-1" style={{ color: bashExcluded ? "var(--text-muted)" : "var(--accent)", marginTop: 4 }}>
             {t("chat.shell")} · {bashExcluded ? t("chat.outputLocal") : t("chat.outputModel")}
          </div>
        )}

        {/* Bottom bar: left | center (context) | right */}
        <div style={{
          marginTop: 8,
          display: isMobile ? "grid" : "flex",
          gridTemplateColumns: isMobile ? "minmax(0, 1fr) auto" : undefined,
          alignItems: "center",
          gap: 6,
        }}>

          {/* LEFT: attachment and skill controls */}
          <div style={{ flex: isMobile ? "1 1 auto" : undefined, minWidth: 0, display: isMobile ? "flex" : "contents", alignItems: "center", gap: 2 }}>
            <button
              onClick={() => fileInputRef.current?.click()}
             title={t("chat.attachImage")}
              style={{
                flexShrink: 0, display: "flex", alignItems: "center", justifyContent: "center",
                width: 32, height: 32, padding: 0,
                background: "none", border: "none",
                borderRadius: 9,
                color: attachedImages.length ? "var(--accent)" : "var(--text-muted)",
                cursor: "pointer",
                opacity: 1,
                transition: "background 0.12s, color 0.12s",
              }}
              onMouseEnter={(e) => {
                e.currentTarget.style.background = "var(--bg-hover)";
                e.currentTarget.style.color = attachedImages.length ? "var(--accent)" : "var(--text)";
              }}
              onMouseLeave={(e) => {
                e.currentTarget.style.background = "none";
                e.currentTarget.style.color = attachedImages.length ? "var(--accent)" : "var(--text-muted)";
              }}
            >
              <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                <rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
                <circle cx="8.5" cy="8.5" r="1.5" />
                <polyline points="21 15 16 10 5 21" />
              </svg>
            </button>
            {/* 拿到会话 id 才给入口：新建会话还没有 sessionId，这时二维码只能指向
                「这个项目里最近的活动会话」，扫出来是别的会话，反而误导。 */}
            {!isMobile && sessionId && (
              <button
                onClick={() => setPairDialogOpen(true)}
                title={t("mobile.pairTitle")}
                aria-label={t("mobile.pairTitle")}
                style={{
                  flexShrink: 0, display: "flex", alignItems: "center", justifyContent: "center",
                  width: 32, height: 32, padding: 0,
                  background: "none", border: "none",
                  borderRadius: 9,
                  color: pairDialogOpen ? "var(--accent)" : "var(--text-muted)",
                  cursor: "pointer",
                  transition: "background 0.12s, color 0.12s",
                }}
                onMouseEnter={(e) => {
                  e.currentTarget.style.background = "var(--bg-hover)";
                  e.currentTarget.style.color = "var(--text)";
                }}
                onMouseLeave={(e) => {
                  e.currentTarget.style.background = "none";
                  e.currentTarget.style.color = pairDialogOpen ? "var(--accent)" : "var(--text-muted)";
                }}
              >
                <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                  <rect x="3" y="3" width="7" height="7" rx="1" />
                  <rect x="14" y="3" width="7" height="7" rx="1" />
                  <rect x="3" y="14" width="7" height="7" rx="1" />
                  <line x1="14" y1="14" x2="14" y2="14.01" />
                  <line x1="21" y1="14" x2="21" y2="21" />
                  <line x1="14" y1="21" x2="21" y2="21" />
                  <line x1="17.5" y1="14" x2="17.5" y2="17.5" />
                </svg>
              </button>
            )}
            <div ref={skillMenuRef} style={{ position: "relative", flexShrink: 0 }}>
              <button
                type="button"
                disabled={!cwd}
                title={t("chat.chooseSkill")}
                aria-label={t("chat.chooseSkill")}
                aria-expanded={skillMenuOpen}
                onClick={() => {
                  const nextOpen = !skillMenuOpen;
                  setSkillMenuOpen(nextOpen);
                  setSlashMenuOpen(false);
                  setHistoryMenuOpen(false);
                  setModelDropdownOpen(false);
                  setModelFilter("");
                  if (nextOpen) void loadSkillMenu();
                  else setSkillFilter("");
                }}
                style={{
                  height: 32,
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  gap: 6,
                  padding: "0 9px",
                  background: skillMenuOpen ? "var(--bg-hover)" : "none",
                  border: "none",
                  borderRadius: 9,
                  color: skillMenuOpen ? "var(--text)" : "var(--text-muted)",
                  cursor: cwd ? "pointer" : "not-allowed",
                  opacity: cwd ? 1 : 0.45,
                  fontSize: 12,
                  fontWeight: 500,
                  whiteSpace: "nowrap",
                  transition: "background 0.12s, color 0.12s",
                }}
                onMouseEnter={(e) => {
                  if (!cwd) return;
                  e.currentTarget.style.background = "var(--bg-hover)";
                  e.currentTarget.style.color = "var(--text)";
                }}
                onMouseLeave={(e) => {
                  e.currentTarget.style.background = skillMenuOpen ? "var(--bg-hover)" : "none";
                  e.currentTarget.style.color = skillMenuOpen ? "var(--text)" : "var(--text-muted)";
                }}
              >
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M9.5 3 8.2 6.2 5 7.5l3.2 1.3L9.5 12l1.3-3.2L14 7.5l-3.2-1.3L9.5 3Z" />
                  <path d="m17.5 11-1 2.5L14 14.5l2.5 1 1 2.5 1-2.5 2.5-1-2.5-1-1-2.5Z" />
                  <path d="m6 14-.8 2.2L3 17l2.2.8L6 20l.8-2.2L9 17l-2.2-.8L6 14Z" />
                </svg>
              </button>

              {skillMenuOpen && (
                <div
                  role="dialog"
                  aria-label={t("chat.chooseSkill")}
                  style={{
                    position: "absolute",
                    left: isMobile ? -48 : -34,
                    bottom: "calc(100% + 8px)",
                    zIndex: 500,
                    width: "min(560px, calc(100vw - 32px))",
                    maxHeight: skillMenuMaxHeight === null
                      ? "min(58vh, 460px)"
                      : `min(58vh, 460px, ${skillMenuMaxHeight}px)`,
                    display: "flex",
                    flexDirection: "column",
                    overflow: "hidden",
                    background: "var(--bg)",
                    border: "1px solid var(--border)",
                    borderRadius: 8,
                    boxShadow: "0 -6px 20px rgba(0,0,0,0.12)",
                  }}
                >
                  <div style={{ padding: 10, borderBottom: "1px solid var(--border)", flexShrink: 0 }}>
                    <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 10, marginBottom: 8 }}>
                      <span style={{ fontSize: 12, fontWeight: 600, color: "var(--text)" }}>
                        {t("chat.skillsAvailable", { count: skillMenuSkills.length })}
                      </span>
                      {skillMenuLoading && (
                        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" style={{ animation: "spin 0.8s linear infinite", color: "var(--text-dim)" }} aria-hidden="true">
                          <path d="M21 12a9 9 0 1 1-2.64-6.36" />
                        </svg>
                      )}
                    </div>
                    <div style={{ position: "relative" }}>
                      <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true" style={{ position: "absolute", left: 9, top: "50%", transform: "translateY(-50%)", color: "var(--text-dim)", pointerEvents: "none" }}>
                        <circle cx="11" cy="11" r="7" />
                        <path d="m20 20-4-4" />
                      </svg>
                      <input
                        value={skillFilter}
                        onChange={(e) => setSkillFilter(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === "Escape") {
                            setSkillMenuOpen(false);
                            setSkillFilter("");
                            requestAnimationFrame(() => textareaRef.current?.focus());
                          }
                        }}
                        placeholder={t("chat.searchSkills")}
                        aria-label={t("chat.searchSkills")}
                        autoFocus
                        autoComplete="off"
                        spellCheck={false}
                        style={{
                          width: "100%",
                          height: 32,
                          boxSizing: "border-box",
                          padding: "0 10px 0 29px",
                          border: "1px solid var(--border)",
                          borderRadius: 6,
                          outline: "none",
                          background: "var(--bg-panel)",
                          color: "var(--text)",
                          fontSize: 12,
                        }}
                      />
                    </div>
                  </div>

                  <div style={{ minHeight: 0, overflowY: "auto", padding: 8 }}>
                    {skillMenuError && skillMenuSkills.length === 0 ? (
                      <div style={{ padding: "18px 12px", textAlign: "center" }}>
                        <div style={{ color: "var(--text-muted)", fontSize: 12, marginBottom: 8 }}>{t("chat.skillsLoadFailed")}</div>
                        <button
                          type="button"
                          onClick={() => void loadSkillMenu()}
                          style={{ padding: "5px 10px", border: "1px solid var(--border)", borderRadius: 6, background: "var(--bg-panel)", color: "var(--text)", cursor: "pointer", fontSize: 12 }}
                        >
                          {t("chat.retryPrompt")}
                        </button>
                      </div>
                    ) : !skillMenuLoading && skillMenuSkills.length === 0 ? (
                      <div style={{ padding: "18px 12px", textAlign: "center", color: "var(--text-dim)", fontSize: 12 }}>
                        {t("chat.noSkillsAvailable")}
                      </div>
                    ) : !skillMenuLoading && skillMenuMatchCount === 0 ? (
                      <div style={{ padding: "18px 12px", textAlign: "center", color: "var(--text-dim)", fontSize: 12 }}>
                        {t("chat.noMatchingSkills")}
                      </div>
                    ) : (
                      skillMenuGroups.map((group) => (
                        <section key={group.id} style={{ marginBottom: 10 }}>
                          <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8, padding: "4px 6px", color: "var(--text-dim)", fontSize: 10, fontWeight: 600, textTransform: "uppercase" }}>
                            <span>{skillMenuGroupLabels[group.id]}</span>
                            <span style={{ fontFamily: "var(--font-mono)", fontWeight: 500 }}>{group.skills.length}</span>
                          </div>
                          <div>
                            {group.skills.map((skill) => (
                              <button
                                key={skill.filePath}
                                type="button"
                                onClick={() => applySkill(skill)}
                                title={`/skill:${skill.name}`}
                                style={{
                                  width: "100%",
                                  minWidth: 0,
                                  display: "grid",
                                  gridTemplateColumns: "minmax(0, 1fr) auto",
                                  gap: "3px 10px",
                                  padding: "8px 9px",
                                  border: "none",
                                  borderRadius: 6,
                                  background: "transparent",
                                  color: "var(--text)",
                                  cursor: "pointer",
                                  textAlign: "left",
                                }}
                                onMouseEnter={(e) => { e.currentTarget.style.background = "var(--bg-hover)"; }}
                                onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; }}
                              >
                                <span style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontFamily: "var(--font-mono)", fontSize: 12.5, fontWeight: 600, color: skill.disableModelInvocation ? "var(--text-muted)" : "var(--text)" }}>
                                  {skill.name}
                                </span>
                                {skill.disableModelInvocation && (
                                  <span style={{ alignSelf: "start", padding: "1px 5px", borderRadius: 4, background: "var(--bg-selected)", color: "var(--text-dim)", fontSize: 9, fontWeight: 600, textTransform: "uppercase" }}>
                                    {t("chat.dormant")}
                                  </span>
                                )}
                                <span style={{ gridColumn: "1 / -1", minWidth: 0, display: "-webkit-box", WebkitBoxOrient: "vertical", WebkitLineClamp: 2, overflow: "hidden", color: "var(--text-dim)", fontSize: 11.5, lineHeight: 1.4 }}>
                                  {skill.description}
                                </span>
                              </button>
                            ))}
                          </div>
                        </section>
                      ))
                    )}
                  </div>
                </div>
              )}
            </div>
            {onOpenConsole && (
              <button
                type="button"
                onClick={onOpenConsole}
                title={t("console.open")}
                aria-label={t("console.open")}
                aria-pressed={consoleActive}
                style={{
                  flexShrink: 0, display: "flex", alignItems: "center", justifyContent: "center",
                  width: 32, height: 32, padding: 0,
                  background: consoleActive ? "var(--bg-hover)" : "none",
                  border: "none", borderRadius: 9,
                  color: consoleActive ? "var(--text)" : "var(--text-muted)",
                  cursor: "pointer", transition: "background 0.12s, color 0.12s",
                }}
                onMouseEnter={(e) => {
                  e.currentTarget.style.background = "var(--bg-hover)";
                  e.currentTarget.style.color = "var(--text)";
                }}
                onMouseLeave={(e) => {
                  e.currentTarget.style.background = consoleActive ? "var(--bg-hover)" : "none";
                  e.currentTarget.style.color = consoleActive ? "var(--text)" : "var(--text-muted)";
                }}
              >
                <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <polyline points="4 17 10 11 4 5" />
                  <line x1="12" y1="19" x2="20" y2="19" />
                </svg>
              </button>
            )}
            {/* Model selector — visible always, disabled while the session or switch is busy */}
            {(modelOptions.length > 0 || currentName || modelError) && onModelChange && (
                <div ref={dropdownRef} style={{
                  position: "relative",
                  flex: isMobile ? "1 1 auto" : undefined,
                  minWidth: 0,
                  order: isMobile ? undefined : 1,
                }}>
                  <button
                    onClick={(e) => {
                      const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
                      setModelDropdownRect({ top: rect.top, left: rect.left, width: rect.width });
                      setModelDropdownOpen((open) => {
                        if (open) setModelFilter("");
                        return !open;
                      });
                    }}
                    disabled={isStreaming || modelSwitching}
                    aria-busy={modelSwitching || undefined}
                    style={{
                      display: "flex", alignItems: "center", gap: 6,
                      justifyContent: isMobile ? "flex-start" : undefined,
                      padding: isMobile ? "8px 10px" : "8px 12px",
                      height: 32,
                      width: isMobile ? "100%" : undefined,
                      maxWidth: isMobile ? "100%" : 220,
                      overflow: "hidden",
                      background: modelDropdownOpen ? "var(--bg-hover)" : "none",
                      border: "none",
                      borderRadius: 9,
                      color: "var(--text-muted)",
                      cursor: isStreaming || modelSwitching ? "not-allowed" : "pointer",
                      fontSize: 12,
                      opacity: isStreaming ? 0.5 : 1,
                      transition: "background 0.12s, color 0.12s",
                    }}
                    onMouseEnter={(e) => {
                      if (isStreaming || modelSwitching) return;
                      e.currentTarget.style.background = "var(--bg-hover)";
                      e.currentTarget.style.color = "var(--text)";
                    }}
                    onMouseLeave={(e) => {
                      e.currentTarget.style.background = modelDropdownOpen ? "var(--bg-hover)" : "none";
                      e.currentTarget.style.color = "var(--text-muted)";
                    }}
                    title={modelSwitching ? t("chat.switchingModel") : modelOptions.length > 0 ? t("chat.changeModel") : t("chat.noAvailableModels")}
                  >
                    {modelSwitching ? (
                      <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" style={{ animation: "spin 0.8s linear infinite", flexShrink: 0 }} aria-hidden="true">
                        <path d="M21 12a9 9 0 1 1-2.64-6.36" />
                      </svg>
                    ) : (
                      <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                        <rect x="4" y="4" width="16" height="16" rx="2" />
                        <rect x="9" y="9" width="6" height="6" />
                        <line x1="9" y1="1" x2="9" y2="4" /><line x1="15" y1="1" x2="15" y2="4" />
                        <line x1="9" y1="20" x2="9" y2="23" /><line x1="15" y1="20" x2="15" y2="23" />
                        <line x1="20" y1="9" x2="23" y2="9" /><line x1="20" y1="14" x2="23" y2="14" />
                        <line x1="1" y1="9" x2="4" y2="9" /><line x1="1" y1="14" x2="4" y2="14" />
                      </svg>
                    )}
                    <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", minWidth: 0 }}>
                      {currentName ?? (modelOptions.length > 0 ? t("chat.selectModel") : t("chat.noModels"))}
                    </span>
                  </button>
                  {modelDropdownOpen && modelDropdownRect && (() => {
                    const viewportHeight = window.visualViewport?.height ?? window.innerHeight;
                    const bottom = viewportHeight - modelDropdownRect.top + 6;
                    const maxH = Math.max(120, Math.min(modelDropdownRect.top - 8, viewportHeight * 0.6));
                    // On mobile, pin to a small left margin and cap width to the
                    // viewport so long model names never push the panel off-screen.
                    const panelPos: React.CSSProperties = isMobile
                      ? { left: 8, right: 8, maxWidth: "calc(100vw - 16px)" }
                      : { left: modelDropdownRect.left, width: "max-content", minWidth: modelDropdownRect.width };
                    return (
                      <div ref={modelDropdownPanelRef} style={{
                      position: "fixed",
                      bottom,
                      ...panelPos,
                      zIndex: 500, background: "var(--bg)", border: "1px solid var(--border)",
                      borderRadius: 8, boxShadow: "0 -4px 16px rgba(0,0,0,0.10)",
                      overflow: "hidden", maxHeight: maxH, display: "flex", flexDirection: "column",
                      }}>
                      {showModelFilter && (
                        <div style={{ padding: "6px 8px", borderBottom: "1px solid var(--border)", flexShrink: 0 }}>
                          <input
                            value={modelFilter}
                            onChange={(e) => setModelFilter(e.target.value)}
                            onKeyDown={(e) => {
                              if (e.key === "Escape") {
                                setModelFilter("");
                                setModelDropdownOpen(false);
                              }
                            }}
                            placeholder={t("chat.filterModels")}
                            aria-label={t("chat.filterModels")}
                            autoFocus
                            autoComplete="off"
                            spellCheck={false}
                            style={{
                              width: "100%",
                              minWidth: isMobile ? 0 : 220,
                              fontSize: 11,
                              fontFamily: "var(--font-mono)",
                              padding: "5px 8px",
                              border: "1px solid var(--border)",
                              borderRadius: 5,
                              outline: "none",
                              background: "var(--bg)",
                              color: "var(--text)",
                              boxSizing: "border-box",
                            }}
                          />
                        </div>
                      )}
                      <div style={{ minHeight: 0, overflowY: "auto" }}>
                        {modelsByProvider.length === 0 ? (
                          <div style={{ padding: "8px 12px", color: "var(--text-dim)", fontSize: 12, whiteSpace: "nowrap" }}>
                            {modelFilter.trim() ? t("chat.noMatchingModels") : t("chat.noAvailableModels")}
                          </div>
                        ) : modelsByProvider.map((group, gi) => (
                          <div key={group.provider}>
                            {(modelsByProvider.length > 1) && (
                              <div style={{
                                padding: "6px 12px 4px",
                                fontSize: 10, fontWeight: 600, color: "var(--text-dim)",
                                textTransform: "uppercase", letterSpacing: "0.07em",
                                borderTop: gi > 0 ? "1px solid var(--border)" : "none",
                              }}>
                                {group.provider}
                              </div>
                            )}
                            {group.options.map((opt) => {
                              const isActive = opt.modelId === model?.modelId && opt.provider === model?.provider;
                              return (
                                <button
                                  key={`${opt.provider}:${opt.modelId}`}
                                  onClick={() => {
                                    setModelDropdownOpen(false);
                                    setModelFilter("");
                                    if (!isActive || isAutoModelSelection) onModelChange(opt.provider, opt.modelId);
                                  }}
                                  style={{
                                    display: "flex", alignItems: "center", gap: 8,
                                    width: "100%", padding: "7px 12px",
                                    background: isActive ? "var(--bg-selected)" : "none",
                                    border: "none",
                                    color: isActive ? "var(--text)" : "var(--text-muted)",
                                    cursor: "pointer", fontSize: 12, textAlign: "left",
                                    fontWeight: isActive ? 600 : 400,
                                    whiteSpace: "nowrap",
                                  }}
                                  onMouseEnter={(e) => { if (!isActive) e.currentTarget.style.background = "var(--bg-hover)"; }}
                                  onMouseLeave={(e) => { if (!isActive) e.currentTarget.style.background = "none"; }}
                                >
                                  {isActive
                                    ? <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="var(--accent)" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}><polyline points="1.5 5 4 7.5 8.5 2.5" /></svg>
                                    : <span style={{ width: 10, flexShrink: 0 }} />}
                                  {opt.name}
                                </button>
                              );
                            })}
                          </div>
                        ))}
                      </div>
                    </div>
                    );
                  })()}
                </div>
            )}
          </div>

          {/* spacer */}
          {!isMobile && <div style={{ flex: 1 }} />}

          {/* RIGHT: model + thinking + tools preset + sound (idle) | Model + Stop + sound (streaming) */}
          <div ref={controlsMenuRef} style={{
            flex: "0 0 auto",
            display: "flex",
            alignItems: "center",
            justifyContent: "flex-end",
            position: "relative",
            marginLeft: isMobile ? 0 : "auto",
            order: isMobile ? undefined : 2,
          }}>
            {isMobile && (
              <button
                type="button"
                 title={controlsMenuOpen ? undefined : t("chat.moreControls")}
                 aria-label={t("chat.moreControls")}
                aria-expanded={controlsMenuOpen}
                aria-hidden={controlsMenuOpen || undefined}
                tabIndex={controlsMenuOpen ? -1 : undefined}
                onClick={() => {
                  setModelDropdownOpen(false);
                  setModelFilter("");
                  setControlsMenuOpen(true);
                }}
                style={{
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  width: "100%",
                  height: 32,
                  padding: "8px 10px",
                  background: "none",
                  border: "none",
                  borderRadius: 9,
                  color: "var(--text-muted)",
                  cursor: controlsMenuOpen ? "default" : "pointer",
                  fontSize: 12,
                  fontWeight: 500,
                  visibility: controlsMenuOpen ? "hidden" : "visible",
                  pointerEvents: controlsMenuOpen ? "none" : "auto",
                  transition: "background 0.12s, color 0.12s",
                }}
                onMouseEnter={(e) => {
                  if (controlsMenuOpen) return;
                  e.currentTarget.style.background = "var(--bg-hover)";
                  e.currentTarget.style.color = "var(--text)";
                }}
                onMouseLeave={(e) => {
                  if (controlsMenuOpen) return;
                  e.currentTarget.style.background = "none";
                  e.currentTarget.style.color = "var(--text-muted)";
                }}
              >
                {t("chat.moreControls")}
              </button>
            )}
            <div style={{
              display: isMobile ? (controlsMenuOpen ? "flex" : "none") : "flex",
              alignItems: "center",
              gap: isMobile ? 1 : 2,
              ...(isMobile ? {
                position: "absolute",
                right: 0,
                bottom: 0,
                zIndex: 60,
                padding: 1,
                width: "max-content",
                maxWidth: "calc(100vw - 32px)",
                flexWrap: "nowrap",
                justifyContent: "flex-end",
                border: "1px solid color-mix(in srgb, var(--border) 72%, transparent)",
                borderRadius: 10,
                background: "color-mix(in srgb, var(--bg-panel) 92%, var(--bg))",
                boxShadow: "0 8px 24px rgba(0,0,0,0.14)",
                backdropFilter: "blur(10px)",
              } : null),
            }}>
            {!isStreaming && onThinkingLevelChange && (
              <div ref={thinkingDropdownRef} style={{ position: "relative" }}>
                <button
                  onClick={() => !isStreaming && setThinkingDropdownOpen((v) => !v)}
                  disabled={isStreaming}
                   title={t("chat.changeReasoning", { level: thinkingDisplayLabel })}
                   aria-label={t("chat.changeReasoningLabel")}
                  style={{
                    display: "flex", alignItems: "center", justifyContent: "center", gap: 5,
                    padding: isMobile ? "0 6px" : "8px 12px",
                    width: isMobile ? "auto" : undefined,
                    height: 32,
                    background: thinkingDropdownOpen ? "var(--bg-hover)" : "none",
                    border: "none",
                    borderRadius: 9,
                    color: "var(--text-muted)",
                    cursor: isStreaming ? "not-allowed" : "pointer",
                    fontSize: 12,
                    opacity: isStreaming ? 0.5 : 1,
                    transition: "background 0.12s, color 0.12s",
                  }}
                  onMouseEnter={(e) => {
                    if (isStreaming) return;
                    e.currentTarget.style.background = "var(--bg-hover)";
                    e.currentTarget.style.color = "var(--text)";
                  }}
                  onMouseLeave={(e) => {
                    e.currentTarget.style.background = thinkingDropdownOpen ? "var(--bg-hover)" : "none";
                    e.currentTarget.style.color = "var(--text-muted)";
                  }}
                >
                  <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M9.5 2A5.5 5.5 0 0 0 4 7.5c0 1.7.78 3.21 2 4.21V14a1 1 0 0 0 1 1h5a1 1 0 0 0 1-1v-2.29c1.22-1 2-2.51 2-4.21A5.5 5.5 0 0 0 9.5 2z" />
                    <line x1="7" y1="18" x2="12" y2="18" />
                    <line x1="8" y1="21" x2="11" y2="21" />
                  </svg>
                  {(!isMobile || controlsMenuOpen) && <span style={{ whiteSpace: "nowrap" }}>{thinkingDisplayLabel}</span>}
                </button>
                {thinkingDropdownOpen && (
                  <div style={{
                    position: "absolute", bottom: "calc(100% + 6px)",
                    ...(isMobile ? { left: 0 } : { right: 0 }),
                    zIndex: 100, background: "var(--bg)", border: "1px solid var(--border)",
                    borderRadius: 8, boxShadow: "0 -4px 16px rgba(0,0,0,0.10)",
                    overflow: "hidden", minWidth: 180,
                  }}>
                    {THINKING_LEVELS.filter((lvl) => {
                      if (!availableThinkingLevels) return true;
                      if (lvl === "auto") return true;
                      return availableThinkingLevels.includes(lvl);
                    }).map((lvl) => {
                      const isActive = (thinkingLevel ?? "auto") === lvl;
                       const desc = t(THINKING_LEVEL_DESC_KEYS[lvl]);
                      const mappedVal = (lvl !== "auto" && thinkingLevelMap) ? thinkingLevelMap[lvl] : undefined;
                      const displayLabel = (mappedVal != null && mappedVal !== lvl) ? mappedVal : lvl;
                      const showOriginal = mappedVal != null && mappedVal !== lvl;
                      return (
                        <button
                          key={lvl}
                          onClick={() => { setThinkingDropdownOpen(false); if (!isActive) onThinkingLevelChange(lvl); }}
                          style={{
                            display: "flex", alignItems: "center", gap: 8,
                            width: "100%", padding: "7px 12px",
                            background: isActive ? "var(--bg-selected)" : "none",
                            border: "none",
                            color: isActive ? "var(--text)" : "var(--text-muted)",
                            cursor: "pointer", fontSize: 12, textAlign: "left",
                            fontWeight: isActive ? 600 : 400,
                            whiteSpace: "nowrap",
                          }}
                          onMouseEnter={(e) => { if (!isActive) e.currentTarget.style.background = "var(--bg-hover)"; }}
                          onMouseLeave={(e) => { if (!isActive) e.currentTarget.style.background = "none"; }}
                        >
                          {isActive
                            ? <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="var(--accent)" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}><polyline points="1.5 5 4 7.5 8.5 2.5" /></svg>
                            : <span style={{ width: 10, flexShrink: 0 }} />}
                          <span style={{ flex: 1 }}>
                            {displayLabel}
                            {showOriginal && <span style={{ fontSize: 10, color: "var(--text-dim)", fontFamily: "var(--font-mono)", marginLeft: 5 }}>({lvl})</span>}
                          </span>
                          <span style={{ fontSize: 11, color: "var(--text-dim)", marginLeft: 8 }}>{desc}</span>
                        </button>
                      );
                    })}
                  </div>
                )}
              </div>
            )}
            {!isStreaming && onToolPresetChange && (
              <div ref={toolDropdownRef} style={{ position: "relative" }}>
                <button
                  onClick={() => !toolPresetDisabled && setToolDropdownOpen((v) => !v)}
                  disabled={toolPresetDisabled}
                   title={chatModeActive ? t("chat.toolPresetChatMode") : t("chat.changeToolPreset") + `: ${toolPresetLabel}`}
                   aria-label={t("chat.changeToolPreset")}
                  style={{
                    display: "flex", alignItems: "center", justifyContent: "center", gap: 5,
                    padding: isMobile ? "0 6px" : "8px 12px",
                    width: isMobile ? "auto" : undefined,
                    height: 32,
                    background: toolDropdownOpen ? "var(--bg-hover)" : "none",
                    border: "none",
                    borderRadius: 9,
                    color: "var(--text-muted)",
                    cursor: toolPresetDisabled ? "not-allowed" : "pointer",
                    fontSize: 12,
                    opacity: toolPresetDisabled ? 0.5 : 1,
                    transition: "background 0.12s, color 0.12s",
                  }}
                  onMouseEnter={(e) => {
                    if (toolPresetDisabled) return;
                    e.currentTarget.style.background = "var(--bg-hover)";
                    e.currentTarget.style.color = "var(--text)";
                  }}
                  onMouseLeave={(e) => {
                    e.currentTarget.style.background = toolDropdownOpen ? "var(--bg-hover)" : "none";
                    e.currentTarget.style.color = "var(--text-muted)";
                  }}
                >
                  <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" />
                  </svg>
                  {(!isMobile || controlsMenuOpen) && <span style={{ whiteSpace: "nowrap" }}>{toolPresetLabel}</span>}
                </button>
                {toolDropdownOpen && (
                  <div style={{
                    position: "absolute",
                    bottom: "calc(100% + 6px)",
                    right: isMobile ? undefined : 0,
                    left: isMobile ? 0 : undefined,
                    zIndex: 100, background: "var(--bg)", border: "1px solid var(--border)",
                    borderRadius: 8, boxShadow: "0 -4px 16px rgba(0,0,0,0.10)",
                    overflow: "hidden", minWidth: 120,
                  }}>
                    {TOOL_PRESETS.map((lvl) => {
                      const preset = TOOL_PRESET_MAP[lvl];
                      const isActive = (toolPreset ?? "default") === preset;
                      let desc: string;
                      if (lvl === "off") desc = t("chat.noTools");
                      else if (lvl === "read-only") desc = t("chat.readOnlyTools", { count: 4 });
                      else if (lvl === "default") desc = t("chat.builtInTools", { count: 4 });
                      else desc = t("chat.allBuiltInTools");
                      return (
                        <button
                          key={lvl}
                          onClick={() => { setToolDropdownOpen(false); if (!isActive) onToolPresetChange(preset); }}
                          style={{
                            display: "flex", alignItems: "center", gap: 8,
                            width: "100%", padding: "7px 12px",
                            background: isActive ? "var(--bg-selected)" : "none",
                            border: "none",
                            color: isActive ? "var(--text)" : "var(--text-muted)",
                            cursor: "pointer", fontSize: 12, textAlign: "left",
                            fontWeight: isActive ? 600 : 400,
                            whiteSpace: "nowrap",
                          }}
                          onMouseEnter={(e) => { if (!isActive) e.currentTarget.style.background = "var(--bg-hover)"; }}
                          onMouseLeave={(e) => { if (!isActive) e.currentTarget.style.background = "none"; }}
                        >
                          {isActive
                            ? <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="var(--accent)" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}><polyline points="1.5 5 4 7.5 8.5 2.5" /></svg>
                            : <span style={{ width: 10, flexShrink: 0 }} />}
                          <span style={{ flex: 1 }}>{lvl}</span>
                          <span style={{ fontSize: 11, color: "var(--text-dim)", marginLeft: 8 }}>{desc}</span>
                        </button>
                      );
                    })}
                  </div>
                )}
              </div>
            )}

            {isMobile && controlsMenuOpen && (
              <button
                type="button"
                 title={t("chat.collapseControls")}
                 aria-label={t("chat.collapseControls")}
                aria-expanded={true}
                onClick={() => {
                  setToolDropdownOpen(false);
                  setThinkingDropdownOpen(false);
                  setControlsMenuOpen(false);
                }}
                style={{
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  width: 36,
                  height: 32,
                  padding: 0,
                  marginLeft: 0,
                  background: "var(--bg-hover)",
                  border: "none",
                  borderLeft: "1px solid color-mix(in srgb, var(--border) 72%, transparent)",
                  borderRadius: "0 9px 9px 0",
                  color: "var(--text)",
                  cursor: "pointer",
                  transition: "background 0.12s, color 0.12s",
                }}
                onMouseEnter={(e) => {
                  e.currentTarget.style.background = "var(--bg-selected)";
                }}
                onMouseLeave={(e) => {
                  e.currentTarget.style.background = "var(--bg-hover)";
                }}
              >
                <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                  <line x1="18" y1="6" x2="6" y2="18" />
                  <line x1="6" y1="6" x2="18" y2="18" />
                </svg>
              </button>
            )}
            </div>
          </div>

        </div>
      </div>

      {pairDialogOpen && sessionId && (
        <MobilePairDialog
          sessionId={sessionId}
          {...(cwd ? { cwd } : {})}
          onClose={() => setPairDialogOpen(false)}
        />
      )}
    </div>
  );
});
