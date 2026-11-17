import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { measureMinimapTurnPositions, resolveMinimapViewport } = await jiti.import("../lib/chat-minimap-viewport.ts");
const { buildChatRenderUnits } = await jiti.import("../lib/chat-render-units.ts");
const { getVisibleRenderWindow, VISIBLE_PAGE_SIZE } = await jiti.import("../lib/chat-lazy-load.ts");

const user = () => ({ role: "user", content: "继续" });
const assistant = () => ({ role: "assistant", content: [{ type: "text", text: "回复" }] });
const toolCall = (id) => ({
  role: "assistant",
  content: [{ type: "toolCall", toolCallId: id, toolName: "read", input: {} }],
});
const toolResult = (id) => ({ role: "toolResult", toolCallId: id, content: [] });

function measureRenderedWindow(messages, liveTail) {
  const units = buildChatRenderUnits(messages, { liveTail });
  const { startIndex } = getVisibleRenderWindow(units.length, VISIBLE_PAGE_SIZE);
  const firstMessage = units[startIndex].startIdx;
  const tops = messages.flatMap((message, index) => (
    message.role === "user" || message.role === "assistant"
      ? [index < firstMessage ? null : (index - firstMessage) * 100]
      : []
  ));
  return measureMinimapTurnPositions(messages, tops);
}

test("长轮次工具调用超过可见窗口后，视口仍定位在最新轮而非第一轮", () => {
  const messages = [user(), assistant(), user()];
  for (let index = 0; index < 40; index += 1) {
    messages.push(toolCall(String(index)), toolResult(String(index)));
  }
  const positions = measureRenderedWindow(messages, true);
  assert.equal(positions[0].viewportTop, null);
  assert.equal(positions[1].scrollTop, null, "用户消息已被分页卸载，跳转时仍应补齐历史");
  assert.notEqual(positions[1].viewportTop, null, "工具调用没有回答正文，也能提供视口锚点");
  assert.deepEqual(resolveMinimapViewport(positions, 1800, 600), { first: 1, last: 1, active: 1 });
});

test("单轮运行结束折叠后重新使用用户消息的真实锚点", () => {
  const messages = [user()];
  for (let index = 0; index < 40; index += 1) {
    messages.push(toolCall(String(index)), toolResult(String(index)));
  }
  messages.push(assistant());
  assert.equal(measureRenderedWindow(messages, true)[0].scrollTop, null);
  const completed = measureRenderedWindow(messages, false);
  assert.deepEqual(completed, [{ scrollTop: 0, viewportTop: 0 }]);
  assert.deepEqual(resolveMinimapViewport(completed, 0, 600), { first: 0, last: 0, active: 0 });
});

test("只包含一轮的长运行也能定位，不因用户消息不可见而丢失高亮", () => {
  const positions = measureMinimapTurnPositions([user(), toolCall("1"), toolResult("1"), assistant()], [null, 0, 500]);
  assert.deepEqual(positions, [{ scrollTop: null, viewportTop: 0 }]);
  assert.deepEqual(resolveMinimapViewport(positions, 300, 600), { first: 0, last: 0, active: 0 });
});

test("工具结果和自定义消息不占用 user / assistant 的引用槽位", () => {
  const messages = [assistant(), toolResult("0"), user(), toolCall("1"), toolResult("1"), { role: "custom" }, assistant(), user()];
  assert.deepEqual(measureMinimapTurnPositions(messages, [10, null, 100, 200, 400]), [
    { scrollTop: null, viewportTop: 100 },
    { scrollTop: 400, viewportTop: 400 },
  ]);
});

test("用户消息仍在渲染时不能被助手消息覆盖锚点", () => {
  const positions = measureMinimapTurnPositions([user(), assistant(), assistant()], [50, 100, 300]);
  assert.deepEqual(positions, [{ scrollTop: 50, viewportTop: 50 }]);
});

test("历史节点未渲染时从首个实测轮次开始，而不是默认高亮顶部", () => {
  const positions = [null, null, 50, 450, 1000].map((top) => ({ scrollTop: top, viewportTop: top }));
  assert.deepEqual(resolveMinimapViewport(positions, 0, 600), { first: 2, last: 3, active: 2 });
  assert.deepEqual(resolveMinimapViewport(positions, 500, 600), { first: 3, last: 4, active: 3 });
  assert.deepEqual(resolveMinimapViewport(positions, 1100, 600), { first: 4, last: 4, active: 4 });
});

test("未获得任何实测位置时隐藏视口带，不伪造第一轮的位置", () => {
  assert.equal(resolveMinimapViewport([], 0, 600), null);
  assert.equal(resolveMinimapViewport([{ scrollTop: null, viewportTop: null }], 1000, 600), null);
});

test("同轮高度变化后重新测量，历史补齐后不保留旧的回退坐标", () => {
  const messages = [user(), assistant(), user(), assistant()];
  const before = measureMinimapTurnPositions(messages, [null, null, null, 800]);
  const after = measureMinimapTurnPositions(messages, [0, 100, 200, 400]);
  assert.deepEqual(resolveMinimapViewport(before, 800, 600), { first: 1, last: 1, active: 1 });
  assert.deepEqual(after, [{ scrollTop: 0, viewportTop: 0 }, { scrollTop: 200, viewportTop: 200 }]);
  assert.deepEqual(resolveMinimapViewport(after, 200, 600), { first: 1, last: 1, active: 1 });
});

test("组件在流式内容变化时限频测量，并用新测量结果同步视口带", async () => {
  const source = await readFile(new URL("../components/ChatMinimap.tsx", import.meta.url), "utf8");
  assert.match(source, /measureNodes\(\);\s*\}, \[allMessages, measureNodes\]\)/);
  assert.match(source, /const positions = measureMinimapTurnPositions\(allMessagesRef\.current/);
  assert.match(source, /resolveMinimapViewport\(\s*nextNodes\.map\(\(node\) => node\.targetTurn\)/);
  assert.match(source, /node\.targetTurn\.scrollTop === null[\s\S]*?onRevealHistory\(\)/);
});
