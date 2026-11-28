import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";

const load = createJiti(import.meta.url);
const { parseMergeDraft } = await load.import("../lib/git-conflicts.ts");
const { planSimpleMerge, simpleMergeEdits } = await load.import("../lib/git-simple-merge.ts");
const size = 9;

function conflict(local, incoming, base = null, labels = ["HEAD", "incoming"]) {
  return `${"<".repeat(size)} ${labels[0]}\n${local}${base === null ? "" : `${"|".repeat(size)} base\n${base}`}${"=".repeat(size)}\n${incoming}${">".repeat(size)} ${labels[1]}\n`;
}
function plans(original, replay, canMergeEof = true) {
  return planSimpleMerge(original, replay, size, canMergeEof);
}
function apply(text, plan) {
  return simpleMergeEdits(text, parseMergeDraft(text, size).blocks, plan)
    .reduceRight((draft, edit) => draft.slice(0, edit.from) + edit.text + draft.slice(edit.to), text);
}

const left = "A=local\nB=base\n", right = "A=base\nB=incoming\n", base = "A=base\nB=base\n";
const replay = conflict(left, right, base, ["local", "incoming"]);
const block = conflict(left, right);

test("同一冲突块中相邻的独立修改可同时采纳，保留块外手工和自动合并内容", () => {
  const original = `手工修改\n${block}其他自动合并内容\n`;
  const plan = plans(original, `祖先版本\n${replay}其他自动合并内容\n`);
  assert.equal(plan.size, 1);
  const shifted = `新增的手工编辑\n${original}`;
  assert.equal(apply(shifted, plan), "新增的手工编辑\n手工修改\nA=local\nB=incoming\n其他自动合并内容\n");
  assert.equal(apply(shifted.replace("A=local", "A=用户修改"), plan), shifted.replace("A=local", "A=用户修改"));
  assert.equal(apply(`${shifted}${block}`, plan), `${shifted}${block}`);
});

test("只移出混合块中的独立行，保留同一行上的真实冲突及 diff3 祖先标记", () => {
  const ours = "A=local\nB=local\nC=base\n", theirs = "A=base\nB=incoming\nC=incoming\n";
  const common = "A=base\nB=base\nC=base\n";
  const draft = conflict(ours, theirs, common);
  const result = apply(draft, plans(draft, draft));
  const expected = `A=local\n${conflict("B=local\n", "B=incoming\n", "B=base\n")}C=incoming\n`;
  assert.equal(result, expected);
  assert.equal(parseMergeDraft(result, size).blocks.length, 1);
});

test("有重排、重复行、不同边界或末尾换行不确定时不猜测", () => {
  const moved = conflict("B\nA\n", "A\nC\n");
  assert.equal(plans(moved, conflict("B\nA\n", "A\nC\n", "A\nB\n")).size, 0);
  assert.equal(plans(block, conflict(left, right, "A=base\nA=base\n")).size, 0);
  assert.equal(plans(block, replay.replace("A=local", "A=不匹配")).size, 0);
  assert.equal(plans(block, replay, false).size, 0);
  assert.equal(plans(`${block}${block}`, `${replay}${replay}`).size, 0);
  const overlap = conflict("A=local\n", "A=incoming\n");
  assert.equal(plans(overlap, conflict("A=local\n", "A=incoming\n", "A=base\n")).size, 0);
  assert.equal(plans(block, conflict(left, right, "A=base\n")).size, 0);
});
