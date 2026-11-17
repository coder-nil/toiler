import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { sortTagsDescending } = await jiti.import("../lib/git-tags.ts");

test("本地版本标签按数值倒序，而非字符串顺序", () => {
  assert.deepEqual(sortTagsDescending(["v1.0.2", "v1.0.10", "v2.0.0", "v1.1.0"]), [
    "v2.0.0", "v1.1.0", "v1.0.10", "v1.0.2",
  ]);
});

test("同版本正式标签在预发布前，兼容无 v 前缀和预发布数字", () => {
  assert.deepEqual(sortTagsDescending(["v1.2.3-beta.2", "1.2.4", "v1.2.3-beta.10", "v1.2.3"]), [
    "1.2.4", "v1.2.3", "v1.2.3-beta.10", "v1.2.3-beta.2",
  ]);
});

test("非版本标签保留并按名称自然倒序，不改写输入数组", () => {
  const tags = Object.freeze(["release-2", "v1.0.0", "release-10", "backup"]);
  assert.deepEqual(sortTagsDescending(tags), ["v1.0.0", "release-10", "release-2", "backup"]);
  assert.deepEqual(tags, ["release-2", "v1.0.0", "release-10", "backup"]);
  assert.deepEqual(sortTagsDescending([]), []);
  assert.deepEqual(sortTagsDescending(["v1.0.0"]), ["v1.0.0"]);
});

test("标签列表使用倒序展示与专属限高滚动区域，键盘可聚焦", async () => {
  const panel = await readFile(new URL("../components/GitPanel.tsx", import.meta.url), "utf8");
  const css = await readFile(new URL("../app/globals.css", import.meta.url), "utf8");
  assert.match(panel, /sortTagsDescending\(summary\?\.tags \?\? \[\]\)/);
  assert.match(panel, /className="git-file-list git-tag-list"[^>]*tabIndex=\{0\}>\{sortedTags\.map/);
  const block = css.match(/\.git-tag-list\s*\{([^}]+)\}/)?.[1] ?? "";
  assert.match(block, /max-height:\s*min\(30vh, 240px\)/);
  assert.match(block, /overflow-y:\s*scroll/);
  assert.match(block, /scrollbar-gutter:\s*stable/);
  assert.match(css, /\.git-tag-list::-webkit-scrollbar-thumb\s*\{/);
});
