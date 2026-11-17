import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { classifyAgentEvent, blockingUiRequestFromEvent } = await jiti.import("../lib/mobile-events.ts");
const { streamReducer, INITIAL_STREAMING_STATE } = await jiti.import("../lib/streaming-message.ts");
const { buildMobileTimeline, reconcilePendingUserMessages } = await jiti.import("../lib/mobile-timeline.ts");

test("手机静态入口接入现有组件，不依赖 force-dynamic 和 Next 服务端", async () => {
  const page = await readFile(new URL("../app/m/page.tsx", import.meta.url), "utf8");
  assert.match(page, /<I18nProvider>/);
  assert.match(page, /<MobileRemoteView\s*\/>/);
  assert.doesNotMatch(page, /force-dynamic/);
});

test("手机消费 Rust SSE 的完整生命周期并支持重连对账", () => {
  assert.equal(classifyAgentEvent({ type: "connected", isStreaming: false }), "sync");
  assert.equal(classifyAgentEvent({ type: "connected", isStreaming: true }), "start");
  assert.equal(classifyAgentEvent({ type: "agent_start" }), "start");
  const start = { type: "message_start", message: { role: "assistant", content: [] } };
  assert.equal(classifyAgentEvent(start), "snapshot");
  let state = streamReducer(INITIAL_STREAMING_STATE, { type: "snapshot", message: start.message });
  const delta = { type: "text_delta", contentIndex: 0, delta: "手机上的实时正文" };
  assert.equal(classifyAgentEvent({ type: "message_update", assistantMessageEvent: delta }), "delta");
  state = streamReducer(state, { type: "delta", event: { type: "text_start", contentIndex: 0 } });
  state = streamReducer(state, { type: "delta", event: delta });
  assert.equal(state.streamingMessage.content[0].text, "手机上的实时正文");
  for (const type of ["agent_end", "prompt_done", "agent_settled"]) {
    assert.equal(classifyAgentEvent({ type }), "settle");
  }
  assert.deepEqual(streamReducer(state, { type: "end" }), INITIAL_STREAMING_STATE);
  assert.equal(classifyAgentEvent({ type: "message_start", message: { role: "system" } }), "ignore");
});

test("手机 ask_user 请求和另一端回答都能正确对账", () => {
  const request = { type: "extension_ui_request", id: "ui-1", method: "confirm", title: "确认？" };
  assert.equal(classifyAgentEvent(request), "prompt");
  assert.equal(blockingUiRequestFromEvent(request).id, "ui-1");
  assert.equal(classifyAgentEvent({ type: "extension_ui_resolved", id: "ui-1" }), "sync");
  assert.equal(blockingUiRequestFromEvent({ ...request, method: "setStatus" }), null);
  assert.equal(blockingUiRequestFromEvent({ ...request, id: "" }), null);
});

test("手机快照接管本地用户气泡且保留轮次时间线", () => {
  const pending = [{ id: "pending-1", role: "user", text: "你好" }];
  const snapshot = [{ id: "u-1", role: "user", text: "你好" }, { id: "a-1", role: "assistant", text: "收到" }];
  assert.deepEqual(reconcilePendingUserMessages(pending, snapshot, new Set()), []);
  assert.ok(buildMobileTimeline(snapshot).length > 0);
});
