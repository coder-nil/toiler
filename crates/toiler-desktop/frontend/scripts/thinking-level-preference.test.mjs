import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";
const jiti = createJiti(import.meta.url);
const { DEFAULT_THINKING_LEVEL, THINKING_LEVEL_OPTIONS, availableThinkingLevels, isThinkingLevelAdjustable, getPreferredThinkingLevel, setPreferredThinkingLevel, resolveThinkingLevel, resolveDisplayThinkingLevel } = await jiti.import("../lib/thinking-level-preference.ts");

test("菜单里不再有 auto，默认档位是真实存在的 medium", () => {
  assert.ok(!THINKING_LEVEL_OPTIONS.includes("auto"), "auto 已从菜单删除");
  assert.deepEqual(THINKING_LEVEL_OPTIONS, ["off", "minimal", "low", "medium", "high", "xhigh", "max"]);
  assert.equal(DEFAULT_THINKING_LEVEL, "medium");
});

test("记住最近显式选择，包含关闭", () => {
  const values = new Map();
  const storage = { getItem: (key) => values.get(key) ?? null, setItem: (key, value) => values.set(key, value) };
  for (const level of THINKING_LEVEL_OPTIONS) {
    setPreferredThinkingLevel(level, storage);
    assert.equal(getPreferredThinkingLevel(storage), level);
  }
});

test("菜单与 i18n 里都不再有 auto 档位", async () => {
  const { readFile } = await import("node:fs/promises");
  const input = await readFile(new URL("../components/ChatInput.tsx", import.meta.url), "utf8");
  // 档位清单只有一份，菜单不能自己再写一份字面量（漏改会让 auto 又回到菜单里）。
  assert.ok(!/THINKING_LEVELS = \[/.test(input), "ChatInput 不应再有自己的档位字面量");
  // 只看推理级别菜单那一段：文件里还有大量 CSS 的 `"auto"`（overflowY 等）。
  const thinkingMenu = input.match(/const THINKING_LEVEL_DESC_KEYS[\s\S]*?\n\};/);
  assert.ok(thinkingMenu, "应该能找到推理级别的文案 key 表");
  assert.ok(!thinkingMenu[0].includes("auto"), `推理级别文案表仍含 auto：${thinkingMenu[0]}`);
  assert.ok(!input.includes("thinkingUseDefault"), "「默认推理」文案应已删除");
  for (const locale of ["zh-CN", "en"]) {
    const messages = await readFile(new URL(`../lib/i18n/messages/${locale}.ts`, import.meta.url), "utf8");
    assert.ok(!messages.includes("chat.thinkingUseDefault"), `${locale} 仍留着「默认推理」文案`);
  }
});

// 回归：删掉 auto 后，非推理模型的菜单只剩 off 一项，曾经表现为「推理级别里只有 off」。
test("只支持 off 的模型不展示推理级别入口", () => {
  // qoder 的 GLM / Kimi / MiniMax 系列是 reasoning: false，服务端只给 ["off"]。
  assert.deepEqual(availableThinkingLevels(["off"]), ["off"]);
  assert.equal(isThinkingLevelAdjustable(["off"]), false);
  // 推理模型照旧可用。
  assert.equal(isThinkingLevelAdjustable(["off", "minimal", "low", "medium", "high"]), true);
  assert.deepEqual(availableThinkingLevels(["off", "low", "high"]), ["off", "low", "high"]);
  // 档位数据还没加载到（null/undefined）：不过滤、默认展示，避免一会儿有一会儿没有。
  assert.deepEqual(availableThinkingLevels(null), [...THINKING_LEVEL_OPTIONS]);
  assert.deepEqual(availableThinkingLevels(undefined), [...THINKING_LEVEL_OPTIONS]);
  assert.equal(isThinkingLevelAdjustable(null), true);
});

test("旧数据里的 auto 不再被当成有效偏好", () => {
  assert.equal(getPreferredThinkingLevel({ getItem: () => "auto" }), null);
});

test("最近选择优先于服务端默认和固定级别，未选择时回退到默认档位", () => {
  const supported = ["off", "low", "medium", "high"];
  assert.equal(resolveThinkingLevel("high", "low", "off", supported), "high");
  assert.equal(resolveThinkingLevel(null, "low", "off", supported), "low");
  assert.equal(resolveThinkingLevel(null, null, "high", supported), "high");
  assert.equal(resolveThinkingLevel("off", "high", "high", supported), "off");
  // 什么都没选过：给默认档位，而不是过去那个运行时等价于 off 的 auto。
  assert.equal(resolveThinkingLevel(null, null, null, supported), "medium");
});

test("不支持的级别安全回退，切回支持的模型仍可恢复偏好", () => {
  // 非推理模型只有 off，回退到它支持的那一档。
  assert.equal(resolveThinkingLevel("high", null, null, []), "medium");
  assert.equal(resolveThinkingLevel("high", null, null, ["off"]), "off");
  // 默认档位不在支持列表里时取列表首项。
  assert.equal(resolveThinkingLevel("max", null, null, ["low", "high"]), "low");
  assert.equal(resolveThinkingLevel("high", null, null, ["off", "high"]), "high");
});

test("显示级别以服务端生效值为准，不会显示 auto", () => {
  assert.equal(resolveDisplayThinkingLevel("off", "medium"), "off");
  assert.equal(resolveDisplayThinkingLevel(null, "medium"), "medium");
  assert.equal(resolveDisplayThinkingLevel("medium", "medium"), "medium");
  assert.equal(resolveDisplayThinkingLevel("high", "high"), "high");
  assert.equal(resolveDisplayThinkingLevel("high", "low"), "high");
  // 偏好无法解析（旧数据、未选过、被清掉）时按实际生效级别显示。
  assert.equal(resolveDisplayThinkingLevel("low", "invalid"), "low");
  assert.equal(resolveDisplayThinkingLevel("medium", undefined), "medium");
  // 两者都取不到时用默认档位，界面上绝不会出现 auto 字样。
  assert.equal(resolveDisplayThinkingLevel(null, null), "medium");
  assert.equal(resolveDisplayThinkingLevel(null, "auto"), "medium");
});

test("损坏数据与不可用存储不会影响会话初始化", () => {
  assert.equal(getPreferredThinkingLevel(null), null);
  for (const value of [null, "", "invalid", "HIGH"]) {
    assert.equal(getPreferredThinkingLevel({ getItem: () => value }), null);
  }
  const broken = { getItem() { throw new Error("受限"); }, setItem() { throw new Error("受限"); } };
  assert.equal(getPreferredThinkingLevel(broken), null);
  assert.doesNotThrow(() => setPreferredThinkingLevel("high", broken));
});
