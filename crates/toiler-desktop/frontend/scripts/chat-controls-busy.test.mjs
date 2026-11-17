import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const source = await readFile(new URL("../components/ChatInput.tsx", import.meta.url), "utf8");

for (const [callback, ref, disabled] of [
  ["onThinkingLevelChange", "thinkingDropdownRef", "isStreaming"],
  ["onToolPresetChange", "toolDropdownRef", "toolPresetDisabled"],
]) {
  test(`${callback} 在处理期间仍渲染，按钮使用原生禁用和灰显样式`, () => {
    assert.match(source, new RegExp(`\\{${callback} && \\(\\s*<div ref=\\{${ref}\\}`));
    assert.doesNotMatch(source, new RegExp(`!isStreaming && ${callback}`));
    const button = source.slice(source.indexOf(`<div ref={${ref}}`)).split("</button>")[0];
    assert.match(button, new RegExp(`disabled=\\{${disabled}\\}`));
    assert.match(button, new RegExp(`opacity: ${disabled} \\? 0\\.5 : 1`));
    assert.match(button, new RegExp(`cursor: ${disabled} \\? "not-allowed" : "pointer"`));
  });
}

test("忙碌时不显示已打开的配置菜单，并清除打开状态，结束后不会自行弹出", () => {
  assert.match(source, /thinkingDropdownOpen && !isStreaming &&/);
  assert.match(source, /toolDropdownOpen && !toolPresetDisabled &&/);
  assert.match(source, /if \(isStreaming\) setThinkingDropdownOpen\(false\)/);
  assert.match(source, /if \(toolPresetDisabled\) setToolDropdownOpen\(false\)/);
  assert.match(source, /\[isStreaming, toolPresetDisabled\]/);
});

test("保留普通对话模式的工具预设禁用规则，并使用完整会话忙碌状态", async () => {
  assert.match(source, /const toolPresetDisabled = isStreaming \|\| chatModeActive/);
  const chatWindow = await readFile(new URL("../components/ChatWindow.tsx", import.meta.url), "utf8");
  assert.match(chatWindow, /isStreaming=\{sessionBusy\}/);
});
