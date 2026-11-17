import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { scrollChatToBottom, getPromptAnchorSpacerHeight } = await jiti.import("../lib/chat-lazy-load.ts");

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

// 模拟两轮消息从置顶到首个流式增量的几何变化，固定留白必须来自组件实值。
const chatWindowSource = await readFile(new URL("../components/ChatWindow.tsx", import.meta.url), "utf8");
const bottomClearance = Number(chatWindowSource.match(/const CHAT_JUMP_CLEARANCE = (\d+);/)[1]);

for (const userTop of [16, 900]) {
  test(`用户消息位于 ${userTop}px 时，置顶后的自动贴底不再将气泡推过顶部`, () => {
    const clientHeight = 600;
    const targetTop = Math.max(0, userTop - 16);
    for (const replyHeight of [0, 20, 120, 300]) {
      const contentEnd = userTop + 70 + replyHeight;
      const spacer = getPromptAnchorSpacerHeight(targetTop, contentEnd, clientHeight, bottomClearance);
      const container = {
        scrollHeight: Math.max(clientHeight, contentEnd + spacer + bottomClearance),
        clientHeight,
        scrollTo({ top }) { this.scrollTop = top; },
      };
      scrollChatToBottom(container);
      assert.equal(userTop - container.scrollTop, 16, `回复增长到 ${replyHeight}px 后仍保留顶部间距`);
    }
  });
}

test("回复长过视口后占位归零，正常跟随最新输出", () => {
  assert.equal(getPromptAnchorSpacerHeight(884, 1800, 600, bottomClearance), 0);
});

test("仅靠固定底部留白已能置顶时，不额外制造占位", () => {
  assert.equal(getPromptAnchorSpacerHeight(884, 1460, 600, bottomClearance), 0);
});

test("组件计算尾部占位时必须计入实际渲染的固定留白", () => {
  assert.match(chatWindowSource, /getPromptAnchorSpacerHeight\(\s*targetTop,\s*contentEnd,\s*container.clientHeight,\s*CHAT_JUMP_CLEARANCE,/);
});
