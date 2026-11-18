import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { measureMinimapTurnPositions, resolveMinimapViewport, resolveMinimapViewportBand } = await jiti.import("../lib/chat-minimap-viewport.ts");
const { layoutNodes, MINIMAP_PADDING, MAX_NODE_GAP } = await jiti.import("../lib/chat-minimap-layout.ts");
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
  // 胶囊几何必须走纯函数：内联的 ±gap/2 外扩会在首尾节点上被容器裁掉，
  // 表现为胶囊贴着窗口顶端、横条偏在胶囊上半截。
  assert.match(source, /resolveMinimapViewportBand\(\{/);
  assert.doesNotMatch(source, /topRatio \* height - gap \* 0\.5/);
});

test("第一个用户消息胶囊整体下移，不与轨道顶端贴边", () => {
  // 真实链路：节点布局 → 视口胶囊。首轮对话只有一条节点，位于 MINIMAP_PADDING。
  const layout = layoutNodes([{ topRatio: 0 }], 600);
  const firstCenter = layout.nodes[0].topRatio * 600;
  assert.equal(firstCenter, MINIMAP_PADDING, "首轮节点应落在顶部内边距处");

  const band = resolveMinimapViewportBand({
    firstCenter,
    lastCenter: firstCenter,
    trackHeight: 600,
    gap: layout.gap,
    nodeThickness: layout.nodeHeight,
  });
  assert.ok(band);
  // 胶囊上下都与窗口边留出静止空气：既不能 top=0，也不能贴着节点上沿。
  assert.ok(band.top >= 8, `胶囊顶部应明显下移（实际 ${band.top}px）`);
  assert.ok(band.top + band.height / 2 === firstCenter, "胶囊仍需与节点同心");
  assert.ok(band.top > 0, "胶囊不能与轨道顶端相切");
});

test("首轮胶囊与顶部之间的余量单轮、多轮保持一致", () => {
  // 多轮时首轮节点未在上边缘之外，胶囊仍按同样内边距落位。
  const single = layoutNodes([{ topRatio: 0 }], 600);
  const many = layoutNodes(Array.from({ length: 5 }, () => ({ topRatio: 0 })), 600);
  assert.equal(many.nodes[0].topRatio, single.nodes[0].topRatio);

  const band = resolveMinimapViewportBand({
    firstCenter: many.nodes[0].topRatio * 600,
    lastCenter: many.nodes[0].topRatio * 600,
    trackHeight: 600,
    gap: many.gap,
    nodeThickness: many.nodeHeight,
  });
  assert.ok(band);
  assert.ok(band.top >= 8, "多轮时首轮胶囊也必须有同样的顶部余量");
});

test("高度被填满时末轮节点停在底部内边距，与顶部对称", () => {
  // 轮次够多时自然间距不再封顶（fillsHeight），节点铺满可用高度。
  const many = layoutNodes(Array.from({ length: 30 }, () => ({ topRatio: 0 })), 600);
  assert.equal(many.fillsHeight, true);
  assert.equal(Math.round(many.nodes[0].topRatio * 600), MINIMAP_PADDING);
  assert.equal(
    Math.round(many.nodes[many.nodes.length - 1].topRatio * 600),
    600 - MINIMAP_PADDING,
  );

  // 稀疏时（间距封顶）首轮就在内边距，后续节点不再往下铺。
  const few = layoutNodes(Array.from({ length: 5 }, () => ({ topRatio: 0 })), 600);
  assert.equal(few.fillsHeight, false);
  assert.ok(few.gap <= MAX_NODE_GAP);
  assert.equal(Math.round(few.nodes[0].topRatio * 600), MINIMAP_PADDING);
});

test("末轮节点的视口胶囊同样与轨道底端留出空隙", () => {
  const band = resolveMinimapViewportBand({
    firstCenter: 588,
    lastCenter: 588,
    trackHeight: 600,
    gap: 50,
    nodeThickness: 4,
  });
  assert.ok(band);
  assert.equal(band.top + band.height / 2, 588);
  assert.ok(600 - (band.top + band.height) > 0, "胶囊不能贴住轨道底端");
});

test("多轮区间的胶囊以首尾节点中心为轴，不被容器裁成偏心", () => {
  const band = resolveMinimapViewportBand({
    firstCenter: 12,
    lastCenter: 162,
    trackHeight: 600,
    gap: 50,
    nodeThickness: 4,
  });
  assert.ok(band);
  assert.equal(band.top + band.height / 2, 87);
  assert.ok(band.top > 0, "跨越首节点时胶囊仍不能贴边");
});

test("极端距离下胶囊不溢出轨道", () => {
  for (const gap of [2, 50]) {
    const band = resolveMinimapViewportBand({
      firstCenter: 0,
      lastCenter: 600,
      trackHeight: 600,
      gap,
      nodeThickness: 4,
    });
    assert.ok(band);
    assert.ok(band.top >= 0);
    assert.ok(band.top + band.height <= 600);
  }
  assert.equal(resolveMinimapViewportBand({
    firstCenter: 10, lastCenter: 10, trackHeight: 0, gap: 10, nodeThickness: 4,
  }), null);
});
