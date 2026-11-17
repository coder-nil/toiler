import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { isChatRefreshShortcut, createChatRefreshHandler } = await jiti.import("../lib/chat-refresh-shortcut.ts");

function keyEvent(overrides = {}) {
  return {
    key: "r", code: "KeyR", metaKey: false, ctrlKey: false,
    altKey: false, shiftKey: false, isComposing: false, repeat: false,
    defaultPrevented: false,
    preventDefault() { this.defaultPrevented = true; },
    ...overrides,
  };
}

test("消息刷新识别 Cmd+R、F5 和 Ctrl+R，不占用其它组合", () => {
  for (const overrides of [
    { metaKey: true }, { ctrlKey: true }, { key: "F5", code: "F5" },
    { metaKey: true, key: "R" },
  ]) assert.equal(isChatRefreshShortcut(keyEvent(overrides)), true);
  for (const overrides of [
    {}, { metaKey: true, shiftKey: true }, { metaKey: true, altKey: true },
    { metaKey: true, isComposing: true }, { key: "F5", code: "F5", ctrlKey: true },
    { metaKey: true, key: "n", code: "KeyN" },
  ]) assert.equal(isChatRefreshShortcut(keyEvent(overrides)), false);
});

test("刷新阻止窗口重载，长按及请求未完成时不重复刷新", async () => {
  let calls = 0;
  let release;
  const handler = createChatRefreshHandler(() => {
    calls += 1;
    return new Promise((resolve) => { release = resolve; });
  });
  const first = keyEvent({ metaKey: true });
  const pending = handler(first);
  assert.equal(first.defaultPrevented, true);
  for (const overrides of [{ repeat: true }, {}]) {
    const next = keyEvent({ metaKey: true, ...overrides });
    await handler(next);
    assert.equal(next.defaultPrevented, true);
  }
  assert.equal(calls, 1);
  release();
  await pending;
  const next = handler(keyEvent({ key: "F5" }));
  assert.equal(calls, 2);
  release();
  await next;
  const unrelated = keyEvent();
  await handler(unrelated);
  assert.equal(unrelated.defaultPrevented, false);
});

test("刷新失败后仍可再次触发", async () => {
  let calls = 0;
  const handler = createChatRefreshHandler(async () => {
    calls += 1;
    if (calls === 1) throw new Error("刷新失败");
  });
  await assert.rejects(handler(keyEvent({ metaKey: true })), /刷新失败/);
  await handler(keyEvent({ metaKey: true }));
  assert.equal(calls, 2);
});
