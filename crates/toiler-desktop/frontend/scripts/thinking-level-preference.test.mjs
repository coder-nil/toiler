import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";
const jiti = createJiti(import.meta.url);
const { THINKING_LEVEL_OPTIONS, getPreferredThinkingLevel, setPreferredThinkingLevel, resolveThinkingLevel } = await jiti.import("../lib/thinking-level-preference.ts");

test("记住最近显式选择，包含关闭与自动", () => {
  const values = new Map();
  const storage = { getItem: (key) => values.get(key) ?? null, setItem: (key, value) => values.set(key, value) };
  for (const level of THINKING_LEVEL_OPTIONS) {
    setPreferredThinkingLevel(level, storage);
    assert.equal(getPreferredThinkingLevel(storage), level);
  }
});

test("最近选择优先于服务端默认和固定级别，未选择时保持原有回退", () => {
  const supported = ["off", "low", "high"];
  assert.equal(resolveThinkingLevel("high", "low", "off", supported), "high");
  assert.equal(resolveThinkingLevel(null, "low", "off", supported), "low");
  assert.equal(resolveThinkingLevel(null, null, "high", supported), "high");
  assert.equal(resolveThinkingLevel("auto", "high", "high", supported), "auto");
  assert.equal(resolveThinkingLevel("off", "high", null, supported), "off");
  assert.equal(resolveThinkingLevel(null, null, null, supported), "auto");
});

test("不支持的级别安全回退，切回支持的模型仍可恢复偏好", () => {
  assert.equal(resolveThinkingLevel("high", null, null, []), "auto");
  assert.equal(resolveThinkingLevel("max", null, null, ["off", "high"]), "auto");
  assert.equal(resolveThinkingLevel("high", null, null, ["off", "high"]), "high");
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
