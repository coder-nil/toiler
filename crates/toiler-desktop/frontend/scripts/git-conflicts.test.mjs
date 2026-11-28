import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";

const { alignMergeRows, changedMergeColumns, mergeBlockText, mergeMarkerOffsets, mergeTextForSave, normalizeMergeText, parseMergeDraft } = await createJiti(import.meta.url).import("../lib/git-conflicts.ts");

test("冲突定位支持自定义标记宽度，并跳过普通文本", () => {
  const text = "文档\n<<<<<<<<< HEAD\n本地\n||||||||| base\n=========\n远程\n>>>>>>>>> origin\n  <<<<<<<<< 缩进\n<<<<<<<<<不是标记";
  assert.deepEqual(mergeMarkerOffsets(text, 9).map((offset) => text.slice(offset).split("\n")[0]), [
    "<<<<<<<<< HEAD", "||||||||| base", "=========", ">>>>>>>>> origin",
  ]);
  assert.deepEqual(mergeMarkerOffsets(text, 7), []);
});

test("合并结果保留原有 CRLF，未改动时逐字返回原文", () => {
  assert.equal(mergeTextForSave("改动\n两行\n", "原文\r\n"), "改动\r\n两行\r\n");
  const mixed = "a\nb\r\nc\r";
  assert.equal(mergeTextForSave(normalizeMergeText(mixed), mixed), mixed);
  assert.equal(mergeTextForSave("\n新文件\n", null), "\n新文件\n");
});

test("逐块或批量应用保持自动合并内容、先前手工编辑及 diff3 空侧", () => {
  const draft = "手工修改\n<<<<<<< HEAD\n左一\n||||||| base\n旧内容\n=======\n右一\n>>>>>>> origin/main\n自动合并\n<<<<<<< HEAD\n=======\n右二\n>>>>>>> origin/main\n结尾\n";
  const { blocks, looseMarkers } = parseMergeDraft(draft, 7);
  assert.equal(blocks.length, 2);
  assert.equal(looseMarkers.length, 0);
  assert.equal(blocks[0].incomingLabel, "origin/main");
  assert.equal(blocks[0].startLine, 2);
  assert.equal(blocks[1].local, "");
  const apply = (choices) => blocks.reduceRight((text, block, index) => (
    text.slice(0, block.from) + mergeBlockText(block, choices[index], draft, "") + text.slice(block.to)
  ), draft);
  assert.equal(apply(["incoming", "local"]), "手工修改\n右一\n自动合并\n结尾\n");
  assert.equal(apply(["local", "local"]), "手工修改\n左一\n自动合并\n结尾\n");
});

test("不自动替换嵌套、孤立和不完整标记", () => {
  const draft = "=======\n<<<<<<< HEAD\n<<<<<<< nested\na\n=======\nb\n>>>>>>> nested\n=======\n>>>>>>> source\n<<<<<<< HEAD\nc";
  const { blocks, looseMarkers } = parseMergeDraft(draft, 7);
  assert.deepEqual(blocks, []);
  assert.equal(looseMarkers.length, 8);
});

test("采纳 EOF 冲突保留对应版本的末行换行状态", () => {
  const draft = "开头\n<<<<<<< HEAD\n左侧\n=======\n右侧\n>>>>>>> source\n";
  const [block] = parseMergeDraft(draft, 7).blocks;
  assert.equal(mergeBlockText(block, "local", draft, "开头\n左侧"), "左侧");
  assert.equal(mergeBlockText(block, "incoming", draft, "开头\n右侧\n"), "右侧\n");
});

test("三栏按冲突起点对齐，多出来的行不改变各栏真实行号", () => {
  const draft = "开头\n左侧非冲突\n<<<<<<< HEAD\n左\n=======\n右\n右侧多一行\n>>>>>>> source\n结尾\n";
  const { blocks } = parseMergeDraft(draft, 7);
  const layout = alignMergeRows(draft.split("\n"), "开头\n左侧非冲突\n左\n结尾\n".split("\n"), "开头\n右\n右侧多一行\n结尾\n".split("\n"), blocks);
  assert.deepEqual(layout.rows[1], [2, 2, null]);
  assert.deepEqual(layout.rows[layout.conflictRows.get(3)], [3, 3, 2]);
  assert.deepEqual(layout.rows[layout.conflictRows.get(3) + 1], [null, 4, 3]);
  assert.deepEqual(layout.rows.at(-2), [4, 9, 4]);
  assert.deepEqual(layout.rows.map((row) => row[2]).filter(Boolean), [1, 2, 3, 4, 5]);
});

test("三栏对齐保留只出现在只读栏的行，并高亮行内有差异的区间", () => {
  const layout = alignMergeRows(["a", "b"], ["之前", "a", "中间", "b", "之后"], ["a", "b"], []);
  assert.deepEqual(layout.rows, [[1, null, null], [2, 1, 1], [3, null, null], [4, 2, 2], [5, null, null]]);
  assert.deepEqual(changedMergeColumns("NAME=left;", "NAME=right;"), [6, 9]);
});
