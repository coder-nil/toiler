import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";

const { historyFilterParams, layoutGitHistory, shortRef } = await createJiti(import.meta.url).import("../lib/git-history.ts");
const commit = (hash, ...parents) => ({ hash, parents });

test("合并的两条线在共同祖先汇合，翻页不移动已有节点", () => {
  const history = [commit("merge", "main", "feature"), commit("feature", "base"), commit("main", "base"), commit("base")];
  const first = layoutGitHistory(history.slice(0, 2));
  const all = layoutGitHistory(history);
  assert.deepEqual(all.rows.slice(0, 2), first.rows);
  assert.equal(all.width, 2);
  assert.deepEqual(all.rows, [
    { lane: 0, enters: false, through: [], parents: [0, 1] },
    { lane: 1, enters: true, through: [0], parents: [1] },
    { lane: 0, enters: true, through: [1], parents: [1] },
    { lane: 1, enters: true, through: [], parents: [] },
  ]);
});

test("多父合并保留各父节点的线槽，单链只占一槽", () => {
  const all = layoutGitHistory([commit("merge", "a", "b", "c"), commit("c"), commit("b"), commit("a")]);
  assert.deepEqual(all.rows[0].parents, [0, 1, 2]);
  assert.deepEqual(all.rows.slice(1).map((row) => row.lane), [2, 1, 0]);
  assert.equal(layoutGitHistory([commit("a", "b"), commit("b", "c"), commit("c")]).width, 1);
  assert.equal(shortRef("refs/remotes/origin/feature/a"), "origin/feature/a");
  assert.equal(shortRef("HEAD"), "HEAD");
});

test("提交查询按本地日期发送半开区间，保留摘要中的字面字符", () => {
  const base = { commitId: "A1B2C3D4", subject: " [UI] ", author: " 陈 ", fromDate: "2024-12-31", toDate: "2024-12-31" };
  assert.deepEqual(historyFilterParams(base), {
    commitId: "a1b2c3d4", subject: "[UI]", author: "陈",
    from: String(new Date(2024, 11, 31).getTime() / 1000),
    before: String(new Date(2025, 0, 1).getTime() / 1000),
  });
  assert.deepEqual(historyFilterParams({ commitId: "", subject: " ", author: "", fromDate: "", toDate: "" }), {});
});
