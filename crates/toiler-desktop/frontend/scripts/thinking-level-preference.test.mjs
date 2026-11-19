import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";
const jiti = createJiti(import.meta.url);
const { THINKING_LEVEL_OPTIONS, getPreferredThinkingLevel, setPreferredThinkingLevel, resolveThinkingLevel, resolveDisplayThinkingLevel } = await jiti.import("../lib/thinking-level-preference.ts");

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

test("选自动时生效级别 off 回显为自动，不跳成关闭", () => {
  // 后端只回报实际生效级别（auto 在 rpi 里没有对应档位，一律落成 off）。
  assert.equal(resolveDisplayThinkingLevel("off", "auto"), "auto");
  assert.equal(resolveDisplayThinkingLevel(null, "auto"), "auto");
  // 关闭是用户自己选的，不能被改写成自动。
  assert.equal(resolveDisplayThinkingLevel("off", "off"), "off");
  assert.equal(resolveDisplayThinkingLevel("off", null), "off");
  // 其它级别原样回显。
  assert.equal(resolveDisplayThinkingLevel("high", "high"), "high");
  assert.equal(resolveDisplayThinkingLevel("high", "auto"), "high");
  // 偏好无法解析（旧数据、未选过、被清掉）时按实际生效级别显示。
  assert.equal(resolveDisplayThinkingLevel("low", "invalid"), "low");
  assert.equal(resolveDisplayThinkingLevel(null, null), "auto");
  assert.equal(resolveDisplayThinkingLevel("medium", undefined), "medium");
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
