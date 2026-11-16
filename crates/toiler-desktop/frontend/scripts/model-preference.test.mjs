import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { getPreferredModel, setPreferredModel, resolvePreferredModel } = await jiti.import("../lib/model-preference.ts");
const models = [{ provider: "first", id: "a" }, { provider: "second", id: "b" }];

test("草稿最近选择优先于默认模型，并可从存储恢复", () => {
  const data = new Map();
  const storage = { getItem: (key) => data.get(key) ?? null, setItem: (key, value) => data.set(key, value) };
  setPreferredModel({ provider: "first", modelId: "a" }, storage);
  setPreferredModel({ provider: "second", modelId: "b" }, storage);
  const preferred = getPreferredModel(storage);
  assert.deepEqual(preferred, { provider: "second", modelId: "b" });
  assert.equal(resolvePreferredModel(models, { provider: "first", modelId: "a" }, preferred), models[1]);
});

test("偏好已删除或禁用时退回默认模型，再退回首个可见模型", () => {
  const hidden = { provider: "hidden", modelId: "b" };
  assert.equal(resolvePreferredModel(models, { provider: "second", modelId: "b" }, hidden), models[1]);
  assert.equal(resolvePreferredModel(models, hidden, hidden), models[0]);
  assert.equal(resolvePreferredModel([], hidden, hidden), undefined);
});

test("存储不可用、损坏或字段不合法不影响模型选择", () => {
  assert.equal(getPreferredModel(null), null);
  for (const value of ["{", "null", "{}", '{"provider":"p","modelId":1}', '{"provider":"","modelId":"m"}']) {
    assert.equal(getPreferredModel({ getItem: () => value }), null);
  }
  const broken = { getItem() { throw new Error("受限"); }, setItem() { throw new Error("受限"); } };
  assert.equal(getPreferredModel(broken), null);
  assert.doesNotThrow(() => setPreferredModel({ provider: "p", modelId: "m" }, broken));
});
