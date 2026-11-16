import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { samePath } = await jiti.import("../lib/paths.ts");
const { isSystemMessageEvent } = await jiti.import("../lib/agent-event-wire.ts");
const { streamReducer, INITIAL_STREAMING_STATE } = await jiti.import("../lib/streaming-message.ts");

test("文件路径比较不依赖浏览器平台，兼容 Windows 分隔符与大小写", () => {
  assert.equal(samePath("C:\\Repo\\src\\..\\", "c:/repo"), true);
  assert.equal(samePath("\\\\server\\share\\src\\", "//SERVER/share/src"), true);
  assert.equal(samePath("/repo/src/../", "/repo"), true);
  assert.equal(samePath("/repo", "/Repo"), false);
  assert.equal(samePath("/", "/repo/.."), true);
  assert.equal(samePath("/repo", "/repo-other"), false);
  assert.equal(samePath("", "/repo"), false);
});

test("系统消息过滤仅处理消息事件，不过滤普通聊天消息", () => {
  assert.equal(isSystemMessageEvent({ type: "message_start", message: { role: "system" } }), true);
  assert.equal(isSystemMessageEvent({ type: "message_end", message: { role: "user" } }), false);
  assert.equal(isSystemMessageEvent({ type: "message_update" }), false);
  assert.equal(isSystemMessageEvent({ type: "message_start", message: null }), false);
});

test("本地 SSE 类型兼容 Rust 投影的文本、思考和工具增量", () => {
  let state = streamReducer(INITIAL_STREAMING_STATE, {
    type: "snapshot",
    message: { role: "assistant", content: [] },
  });
  const events = [
    { type: "text_start", contentIndex: 0 },
    { type: "text_delta", contentIndex: 0, delta: "正文" },
    { type: "text_end", contentIndex: 0, content: "正文完成" },
    { type: "thinking_start", contentIndex: 1 },
    { type: "thinking_delta", contentIndex: 1, delta: "思考" },
    { type: "thinking_end", contentIndex: 1, content: "思考完成" },
    { type: "toolcall_start", contentIndex: 2 },
    { type: "toolcall_end", contentIndex: 2, toolCall: { id: "tool-1", name: "read", arguments: { path: "README.md" } } },
  ];
  for (const event of events) state = streamReducer(state, { type: "delta", event });
  assert.deepEqual(state.streamingMessage.content, [
    { type: "text", text: "正文完成" },
    { type: "thinking", thinking: "思考完成" },
    { type: "toolCall", toolCallId: "tool-1", toolName: "read", input: { path: "README.md" } },
  ]);
  assert.deepEqual(streamReducer(state, { type: "end" }), INITIAL_STREAMING_STATE);
});
