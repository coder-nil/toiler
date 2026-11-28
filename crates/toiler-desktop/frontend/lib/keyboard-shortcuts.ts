export type KeyboardShortcutGroupId =
  | "global" | "interface" | "composer" | "streaming" | "menus"
  | "files" | "editor" | "git" | "panels";

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
  toggleGitWorkspace: { key: "g", code: "KeyG" },
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
  { id: "interface", labelKey: "settings.shortcutsGroupInterface" },
  { id: "composer", labelKey: "settings.shortcutsGroupComposer" },
  { id: "streaming", labelKey: "settings.shortcutsGroupStreaming" },
  { id: "menus", labelKey: "settings.shortcutsGroupMenus" },
  { id: "files", labelKey: "settings.shortcutsGroupFiles" },
  { id: "editor", labelKey: "settings.shortcutsGroupEditor" },
  { id: "git", labelKey: "settings.shortcutsGroupGit" },
  { id: "panels", labelKey: "settings.shortcutsGroupPanels" },
] as const satisfies readonly { id: KeyboardShortcutGroupId; labelKey: string }[];

export const KEYBOARD_SHORTCUTS: readonly KeyboardShortcutDefinition[] = [
  { id: "new-session", group: "global", labelKey: "settings.shortcutNewSession", combos: [["Ctrl/Cmd", "N"]] },
  { id: "show-sidebar", group: "global", labelKey: "settings.shortcutShowSidebar", combos: [["Ctrl/Cmd", "B"]] },
  { id: "toggle-console", group: "global", labelKey: "settings.shortcutToggleConsole", combos: [["Ctrl/Cmd", "`"]] },
  { id: "open-directory", group: "global", labelKey: "settings.shortcutOpenDirectory", combos: [["Ctrl/Cmd", "O"]] },
  { id: "toggle-git-workspace", group: "global", labelKey: "settings.shortcutToggleGitWorkspace", combos: [["Ctrl/Cmd", "G"]] },
  { id: "refresh-chat", group: "global", labelKey: "settings.shortcutRefreshChat", combos: [["Ctrl/Cmd", "R"], ["F5"]], noteKey: "settings.shortcutRefreshChatNote" },
  { id: "close-dialog", group: "global", labelKey: "settings.shortcutCloseDialog", combos: [["Escape"]], noteKey: "settings.shortcutCloseDialogNote" },
  { id: "exit-export-view", group: "global", labelKey: "settings.shortcutExitExportView", combos: [["Escape"]] },
  { id: "confirm-session-rename", group: "global", labelKey: "settings.shortcutConfirmSessionRename", combos: [["Enter"]] },
  { id: "cancel-session-rename", group: "global", labelKey: "settings.shortcutCancelSessionRename", combos: [["Escape"]] },
  { id: "zoom-in", group: "interface", labelKey: "settings.shortcutZoomIn", combos: [["Ctrl/Cmd", "+"], ["Ctrl/Cmd", "="]] },
  { id: "zoom-out", group: "interface", labelKey: "settings.shortcutZoomOut", combos: [["Ctrl/Cmd", "-"]] },
  { id: "reset-zoom", group: "interface", labelKey: "settings.shortcutResetZoom", combos: [["Ctrl/Cmd", "0"]] },
  {
    id: "send-message",
    group: "composer",
    labelKey: "settings.shortcutSendMessage",
    combos: [["Enter"], ["Ctrl/Cmd", "Enter"]],
    noteKey: "settings.shortcutSendMessageNote",
  },
  { id: "new-line", group: "composer", labelKey: "settings.shortcutNewLine", combos: [["Shift", "Enter"]] },
  { id: "input-history", group: "composer", labelKey: "settings.shortcutInputHistory", combos: [["ArrowUp"]] },
  { id: "remove-tag", group: "composer", labelKey: "settings.shortcutRemoveTag", combos: [["Backspace"]], noteKey: "settings.shortcutRemoveTagNote" },
  { id: "remote-send", group: "composer", labelKey: "settings.shortcutRemoteSend", combos: [["Enter"]] },
  { id: "submit-prompt", group: "composer", labelKey: "settings.shortcutSubmitPrompt", combos: [["Enter"]], noteKey: "settings.shortcutSubmitPromptNote" },
  { id: "submit-multiline-prompt", group: "composer", labelKey: "settings.shortcutSubmitMultilinePrompt", combos: [["Ctrl/Cmd", "Enter"]] },
  { id: "cancel-extension-panel", group: "composer", labelKey: "settings.shortcutCancelExtensionPanel", combos: [["Escape"]] },
  { id: "stop-response", group: "streaming", labelKey: "settings.shortcutStopResponse", combos: [["Escape"]], noteKey: "settings.shortcutStopResponseNote" },
  { id: "open-queue-menu", group: "streaming", labelKey: "settings.shortcutOpenQueueMenu", combos: [["Enter"], ["Ctrl/Cmd", "Enter"]], noteKey: "settings.shortcutSendMessageNote" },
  { id: "steer-response", group: "streaming", labelKey: "settings.shortcutSteerResponse", combos: [["Alt", "F"]] },
  { id: "queue-followup", group: "streaming", labelKey: "settings.shortcutQueueFollowup", combos: [["Alt", "Q"], ["Alt", "Enter"]], noteKey: "settings.shortcutQueueFollowupNote" },
  { id: "move-selection", group: "menus", labelKey: "settings.shortcutMoveSelection", combos: [["ArrowUp"], ["ArrowDown"]] },
  { id: "move-slash-grid", group: "menus", labelKey: "settings.shortcutMoveSlashGrid", combos: [["ArrowLeft"], ["ArrowRight"]] },
  { id: "choose-suggestion", group: "menus", labelKey: "settings.shortcutChooseSuggestion", combos: [["Tab"], ["Enter"]], noteKey: "settings.shortcutChooseSuggestionNote" },
  { id: "close-menu", group: "menus", labelKey: "settings.shortcutCloseMenu", combos: [["Escape"]] },
  { id: "find-in-file", group: "files", labelKey: "settings.shortcutFindInFile", combos: [["Ctrl/Cmd", "F"]], noteKey: "settings.shortcutFindInFileNote" },
  { id: "next-file-match", group: "files", labelKey: "settings.shortcutNextFileMatch", combos: [["Enter"]], noteKey: "settings.shortcutFileSearchFocusNote" },
  { id: "previous-file-match", group: "files", labelKey: "settings.shortcutPreviousFileMatch", combos: [["Shift", "Enter"]], noteKey: "settings.shortcutFileSearchFocusNote" },
  { id: "close-file-search", group: "files", labelKey: "settings.shortcutCloseFileSearch", combos: [["Escape"]], noteKey: "settings.shortcutFileSearchFocusNote" },
  { id: "mention-lines", group: "files", labelKey: "settings.shortcutMentionLines", combos: [["Ctrl/Cmd", "I"]], noteKey: "settings.shortcutMentionLinesNote" },
  { id: "confirm-file-rename", group: "files", labelKey: "settings.shortcutConfirmFileRename", combos: [["Enter"]] },
  { id: "cancel-file-rename", group: "files", labelKey: "settings.shortcutCancelFileRename", combos: [["Escape"]] },
  { id: "save-file", group: "editor", labelKey: "settings.shortcutSaveFile", combos: [["Ctrl/Cmd", "S"]] },
  { id: "toggle-line-comment", group: "editor", labelKey: "settings.shortcutToggleLineComment", combos: [["Ctrl/Cmd", "/"]] },
  { id: "format-file", group: "editor", labelKey: "settings.shortcutFormatFile", combos: [["Alt", "F"]], noteKey: "settings.shortcutFormatFileNote" },
  { id: "select-next-occurrence", group: "editor", labelKey: "settings.shortcutSelectNextOccurrence", combos: [["Ctrl/Cmd", "D"]] },
  { id: "editor-suggestions", group: "editor", labelKey: "settings.shortcutEditorSuggestions", combos: [["Ctrl/Cmd", "Space"]] },
  { id: "undo-edit", group: "editor", labelKey: "settings.shortcutUndoEdit", combos: [["Ctrl/Cmd", "Z"]], noteKey: "settings.shortcutUndoEditNote" },
  { id: "redo-edit", group: "editor", labelKey: "settings.shortcutRedoEdit", combos: [["Ctrl/Cmd", "Shift", "Z"], ["Ctrl", "Y"]], noteKey: "settings.shortcutRedoEditNote" },
  { id: "redo-file-edit", group: "editor", labelKey: "settings.shortcutRedoFileEdit", combos: [["Ctrl/Cmd", "U"], ["Cmd", "Y"]] },
  { id: "exit-git-workspace", group: "git", labelKey: "settings.shortcutExitGitWorkspace", combos: [["Escape"]], noteKey: "settings.shortcutExitGitWorkspaceNote" },
  { id: "git-history-context-menu", group: "git", labelKey: "settings.shortcutGitHistoryContextMenu", combos: [["Shift", "F10"], ["ContextMenu"]] },
  { id: "git-menu-navigation", group: "git", labelKey: "settings.shortcutGitMenuNavigation", combos: [["ArrowUp"], ["ArrowDown"]] },
  { id: "git-menu-jump", group: "git", labelKey: "settings.shortcutGitMenuJump", combos: [["Home"], ["End"]] },
  { id: "close-git-menu", group: "git", labelKey: "settings.shortcutCloseGitMenu", combos: [["Escape"], ["Tab"]] },
  { id: "confirm-git-ref", group: "git", labelKey: "settings.shortcutConfirmGitRef", combos: [["Enter"]] },
  { id: "resize-panel-width", group: "panels", labelKey: "settings.shortcutResizePanelWidth", combos: [["ArrowLeft"], ["ArrowRight"]], noteKey: "settings.shortcutResizePanelWidthNote" },
  { id: "resize-panel-height", group: "panels", labelKey: "settings.shortcutResizePanelHeight", combos: [["ArrowUp"], ["ArrowDown"]], noteKey: "settings.shortcutResizePanelHeightNote" },
  { id: "resize-panel-limits", group: "panels", labelKey: "settings.shortcutResizePanelLimits", combos: [["Home"], ["End"]], noteKey: "settings.shortcutResizePanelLimitsNote" },
  { id: "reset-panel-size", group: "panels", labelKey: "settings.shortcutResetPanelSize", combos: [["Enter"]], noteKey: "settings.shortcutResizePanelLimitsNote" },
];
