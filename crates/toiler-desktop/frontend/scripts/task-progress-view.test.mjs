import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { resolveTaskProgress, summarizeTaskProgress } = await jiti.import("../lib/task-progress-view.ts");

function result(status) {
  return {
    role: "toolResult",
    toolCallId: `task-${status}`,
    toolName: "task_progress",
    content: [],
    details: { tasks: [{ id: "fix", title: "修复问题", status }] },
  };
}

test("从这里编辑退回未完成快照时，不重新恢复已收起的任务", () => {
  const history = [result("in_progress")];
  const latest = [...history, result("completed")];
  assert.equal(summarizeTaskProgress(resolveTaskProgress([], latest)).finished, true);
  assert.deepEqual(resolveTaskProgress([], history, true), []);
});

test("正常打开当前叶子仍从消息恢复未完成任务", () => {
  const tasks = resolveTaskProgress([], [result("pending")]);
  assert.equal(tasks.length, 1);
  assert.equal(tasks[0].status, "in_progress");
});

test("编辑后新产生的实时任务优先于历史视图限制", () => {
  const widgets = [{
    key: "任务进度",
    lines: ["已完成 0/1", "● 新的任务"],
    placement: "aboveEditor",
  }];
  const tasks = resolveTaskProgress(widgets, [result("completed")], true);
  assert.equal(tasks.length, 1);
  assert.equal(tasks[0].title, "新的任务");
});

test("编辑第一条消息后的空上下文不恢复任务", () => {
  assert.deepEqual(resolveTaskProgress([], [], true), []);
});

test("显式清空优先于更早的未完成记录", () => {
  const cleared = { ...result("completed"), details: { tasks: [] } };
  assert.deepEqual(resolveTaskProgress([], [result("pending"), cleared]), []);
});
