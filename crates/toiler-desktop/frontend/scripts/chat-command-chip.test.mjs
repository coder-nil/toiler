import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { createJiti } from "jiti";

const source = await readFile(new URL("../components/ChatInput.tsx", import.meta.url), "utf8");
const draftStore = await createJiti(import.meta.url).import("../lib/draft-store.ts");

// 发送后 chip 又挂回输入框：新会话首条消息走 promoteNewSession → rekeyDraft，
// 它读的是 ref 快照而不是 React state。若 chip 的写入只 setState，ref 里留着
// 刚提交的 /skill:xxx，rekeyDraft 就把它当成残留草稿搬进新 draftKey，随后
// draftKey 变化的 effect 又把它恢复出来。这里守住「写入必须同时刷新 ref」。
const stateSetters = ["SlashCommand", "AtMentions"];

for (const name of stateSetters) {
  test(`selected${name} 的写入同时更新 ref 快照`, () => {
    assert.match(
      source,
      new RegExp(`const \\[selected${name}, setSelected${name}State\\] = useState`),
      `selected${name} 的 useState setter 必须改名，避免绕过下面的包装函数`,
    );
    assert.match(
      source,
      new RegExp(`const setSelected${name} = useCallback\\(`),
      `selected${name} 需要一层同时写 ref 的包装 setter`,
    );
  });
}

test("包装 setter 先写 ref 再 setState，且 ref 先于 setState 生效", () => {
  const commandSetter = source.slice(
    source.indexOf("const setSelectedSlashCommand = useCallback("),
  ).split("}, []);")[0];
  assert.match(commandSetter, /selectedSlashCommandRef\.current = next;\s*setSelectedSlashCommandState\(next\);/);

  const mentionSetter = source.slice(
    source.indexOf("const setSelectedAtMentions = useCallback("),
  ).split("}, []);")[0];
  // 函数式更新必须基于 ref 的最新值，否则连续两次 append 会互相覆盖。
  assert.match(mentionSetter, /typeof update === "function"/);
  assert.match(mentionSetter, /update\(selectedAtMentionsRef\.current\)/);
  assert.match(mentionSetter, /selectedAtMentionsRef\.current = next;\s*setSelectedAtMentionsState\(next\);/);
});

test("原始 setState 只在包装 setter 内出现", () => {
  for (const name of stateSetters) {
    const occurrences = source.match(new RegExp(`setSelected${name}State`, "g")) ?? [];
    // 声明 + 包装 setter 内的唯一一次调用。
    assert.equal(occurrences.length, 2, `setSelected${name}State 只允许在包装 setter 内调用`);
  }
});

test("清空输入与搬家草稿都经由包装 setter，不会留下过期 chip", () => {
  const clearInput = source.slice(
    source.indexOf("const clearInput = useCallback("),
  ).split("}, [clearImages, draftKey]);")[0];
  assert.match(clearInput, /setSelectedSlashCommand\(null\);/);
  assert.match(clearInput, /setSelectedAtMentions\(\[\]\);/);
});

// rekeyDraft 不做语义判断：它把调用方给的 currentDraft 原样搬到新 key。所以
// 「发送后 chip 复活」的开关就是 ref 快照是否及时清空，这条守住它的语义边界。
test("草稿搬家以调用方快照为准：快照干净则新会话不出现 chip", () => {
  const previous = "new:abc:/repo";
  const next = "sess-1";
  draftStore.clearDraft(previous);
  draftStore.clearDraft(next);

  const stale = draftStore.rekeyDraft(previous, next, {
    value: "",
    images: [],
    selectedCommand: { name: "skill:k8s-deploy", kind: "skill" },
  });
  assert.equal(stale?.selectedCommand?.name, "skill:k8s-deploy", "过期快照会被如实搬走（旧行为的来源）");
  draftStore.clearDraft(next);

  const clean = draftStore.rekeyDraft(previous, next, { value: "", images: [] });
  assert.equal(clean?.selectedCommand, undefined, "快照干净时新会话草稿不该带命令 chip");
  draftStore.clearDraft(next);
});
