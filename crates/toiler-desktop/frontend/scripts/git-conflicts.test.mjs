import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";

const { mergeMarkerOffsets, mergeTextForSave, normalizeMergeText } = await createJiti(import.meta.url).import("../lib/git-conflicts.ts");

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
