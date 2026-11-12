export type KeyboardShortcutGroupId = "global" | "composer" | "streaming" | "menus" | "files";

export interface KeyboardShortcutDefinition {
  id: string;
  group: KeyboardShortcutGroupId;
  labelKey: string;
  combos: readonly (readonly string[])[];
  noteKey?: string;
}

export const PRIMARY_SHORTCUT_BINDINGS = {
  newSession: { key: "n", code: "KeyN" },
  showSidebar: { key: "b", code: "KeyB" },
  toggleConsole: { key: "`", code: "Backquote" },
  openDirectory: { key: "o", code: "KeyO" },
} as const;

export type PrimaryShortcutId = keyof typeof PRIMARY_SHORTCUT_BINDINGS;

export function matchesPrimaryShortcut(
  event: Pick<KeyboardEvent, "altKey" | "code" | "ctrlKey" | "key" | "metaKey" | "shiftKey">,
  shortcut: PrimaryShortcutId,
): boolean {
  const binding = PRIMARY_SHORTCUT_BINDINGS[shortcut];
  const keyMatches = event.code === binding.code || event.key.toLowerCase() === binding.key;
  return keyMatches && (event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey;
}

export const KEYBOARD_SHORTCUT_GROUPS = [
  { id: "global", labelKey: "settings.shortcutsGroupGlobal" },
  { id: "composer", labelKey: "settings.shortcutsGroupComposer" },
  { id: "streaming", labelKey: "settings.shortcutsGroupStreaming" },
  { id: "menus", labelKey: "settings.shortcutsGroupMenus" },
  { id: "files", labelKey: "settings.shortcutsGroupFiles" },
] as const satisfies readonly { id: KeyboardShortcutGroupId; labelKey: string }[];

export const KEYBOARD_SHORTCUTS: readonly KeyboardShortcutDefinition[] = [
  { id: "new-session", group: "global", labelKey: "settings.shortcutNewSession", combos: [["Ctrl/Cmd", "N"]] },
  { id: "show-sidebar", group: "global", labelKey: "settings.shortcutShowSidebar", combos: [["Ctrl/Cmd", "B"]] },
  { id: "toggle-console", group: "global", labelKey: "settings.shortcutToggleConsole", combos: [["Ctrl/Cmd", "`"]] },
  { id: "open-directory", group: "global", labelKey: "settings.shortcutOpenDirectory", combos: [["Ctrl/Cmd", "O"]] },
  {
    id: "send-message",
    group: "composer",
    labelKey: "settings.shortcutSendMessage",
    combos: [["Enter"], ["Ctrl/Cmd", "Enter"]],
    noteKey: "settings.shortcutSendMessageNote",
  },
  { id: "new-line", group: "composer", labelKey: "settings.shortcutNewLine", combos: [["Shift", "Enter"]] },
  { id: "input-history", group: "composer", labelKey: "settings.shortcutInputHistory", combos: [["ArrowUp"]] },
  { id: "remove-tag", group: "composer", labelKey: "settings.shortcutRemoveTag", combos: [["Backspace"]] },
  { id: "stop-response", group: "streaming", labelKey: "settings.shortcutStopResponse", combos: [["Escape"]] },
  { id: "steer-response", group: "streaming", labelKey: "settings.shortcutSteerResponse", combos: [["Alt", "F"]] },
  { id: "queue-followup", group: "streaming", labelKey: "settings.shortcutQueueFollowup", combos: [["Alt", "Q"]] },
  { id: "move-selection", group: "menus", labelKey: "settings.shortcutMoveSelection", combos: [["ArrowUp"], ["ArrowDown"]] },
  { id: "move-slash-grid", group: "menus", labelKey: "settings.shortcutMoveSlashGrid", combos: [["ArrowLeft"], ["ArrowRight"]] },
  { id: "choose-suggestion", group: "menus", labelKey: "settings.shortcutChooseSuggestion", combos: [["Tab"], ["Enter"]] },
  { id: "close-menu", group: "menus", labelKey: "settings.shortcutCloseMenu", combos: [["Escape"]] },
  { id: "mention-lines", group: "files", labelKey: "settings.shortcutMentionLines", combos: [["Ctrl/Cmd", "I"]] },
];
