import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { scrollChatToBottom } = await jiti.import("../lib/chat-lazy-load.ts");

test("滚到底部只操作消息容器，不触发会滚动外层标题栏的 scrollIntoView", () => {
  const calls = [];
  const container = {
    scrollHeight: 1600,
    clientHeight: 600,
    scrollTo: (options) => calls.push(options),
    scrollIntoView: () => assert.fail("不应滚动消息区以外的祖先容器"),
  };
  scrollChatToBottom(container, "instant");
  assert.deepEqual(calls, [{ top: 1000, behavior: "instant" }]);
});

test("内容不足一屏时停在顶部，并保留平滑滚动参数", () => {
  const calls = [];
  scrollChatToBottom({
    scrollHeight: 200,
    clientHeight: 600,
    scrollTo: (options) => calls.push(options),
  }, "smooth");
  assert.deepEqual(calls, [{ top: 0, behavior: "smooth" }]);
});

test("流式跟随和会话恢复共用的滚动入口必须限制在消息区内", async () => {
  const source = await readFile(new URL("../hooks/useAgentSession.ts", import.meta.url), "utf8");
  assert.match(source, /scrollChatToBottom\(container, behavior\)/);
  assert.doesNotMatch(source, /\.scrollIntoView\(/);
});
