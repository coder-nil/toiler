import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";

const { encodeFilePathForApi } = await createJiti(import.meta.url).import("../lib/file-paths.ts");

test("文件 API 保留 Windows 盘符和 UNC 路径前缀", () => {
  assert.equal(encodeFilePathForApi(String.raw`C:\项目 100%`), "C%3A/%E9%A1%B9%E7%9B%AE%20100%25");
  assert.equal(encodeFilePathForApi("C:\\"), "C%3A");
  assert.equal(encodeFilePathForApi(String.raw`\\?\D:\project`), "%2F%2F%3F/D%3A/project");
  assert.equal(encodeFilePathForApi(String.raw`\\server\share\project`), "%2F%2Fserver/share/project");
  assert.equal(encodeFilePathForApi("//server/share/project"), "%2F%2Fserver/share/project");
  assert.equal(encodeFilePathForApi(String.raw`\\?\UNC\server\share\project`), "%2F%2F%3F/UNC/server/share/project");
});

test("POSIX 路径仍按段编码，根路径保持空参数", () => {
  assert.equal(encodeFilePathForApi("/home/test/100% done.txt"), "home/test/100%25%20done.txt");
  assert.equal(encodeFilePathForApi("/"), "");
});
