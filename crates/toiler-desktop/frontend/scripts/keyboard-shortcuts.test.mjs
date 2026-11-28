import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { KEYBOARD_SHORTCUT_GROUPS, KEYBOARD_SHORTCUTS } = await jiti.import("../lib/keyboard-shortcuts.ts");
const { enLocale } = await jiti.import("../lib/i18n/messages/en.ts");
const { zhCNLocale } = await jiti.import("../lib/i18n/messages/zh-CN.ts");

const byId = new Map(KEYBOARD_SHORTCUTS.map((shortcut) => [shortcut.id, shortcut]));

test("快捷键目录中的每条说明都归入有效分组，且有中英文文案", () => {
  const groups = new Set(KEYBOARD_SHORTCUT_GROUPS.map(({ id }) => id));
  assert.equal(groups.size, KEYBOARD_SHORTCUT_GROUPS.length);
  assert.equal(byId.size, KEYBOARD_SHORTCUTS.length);
  for (const group of KEYBOARD_SHORTCUT_GROUPS) {
    assert.ok(KEYBOARD_SHORTCUTS.some((shortcut) => shortcut.group === group.id), group.id);
  }
  for (const shortcut of KEYBOARD_SHORTCUTS) {
    assert.ok(groups.has(shortcut.group), shortcut.id);
    assert.ok(shortcut.combos.length > 0 && shortcut.combos.every((combo) => combo.length > 0), shortcut.id);
  }
  for (const locale of [zhCNLocale, enLocale]) {
    for (const { labelKey, noteKey } of [...KEYBOARD_SHORTCUT_GROUPS, ...KEYBOARD_SHORTCUTS]) {
      for (const key of [labelKey, noteKey].filter(Boolean)) {
        assert.ok(locale.messages[key]?.trim(), `${locale.id}: ${key}`);
      }
    }
  }
});

test("设置收录消息刷新、原生窗口缩放和各工作区的常用按键", () => {
  const expected = {
    "refresh-chat": [["Ctrl/Cmd", "R"], ["F5"]],
    "zoom-in": [["Ctrl/Cmd", "+"], ["Ctrl/Cmd", "="]],
    "zoom-out": [["Ctrl/Cmd", "-"]],
    "reset-zoom": [["Ctrl/Cmd", "0"]],
    "find-in-file": [["Ctrl/Cmd", "F"]],
    "previous-file-match": [["Shift", "Enter"]],
    "save-file": [["Ctrl/Cmd", "S"]],
    "redo-edit": [["Ctrl/Cmd", "Shift", "Z"], ["Ctrl", "Y"]],
    "git-history-context-menu": [["Shift", "F10"], ["ContextMenu"]],
    "resize-panel-limits": [["Home"], ["End"]],
    "exit-export-view": [["Escape"]],
  };
  for (const [id, combos] of Object.entries(expected)) {
    assert.deepEqual(byId.get(id)?.combos, combos, id);
  }
});
