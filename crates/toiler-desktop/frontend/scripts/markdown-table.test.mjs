import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import postcss from "postcss";

const css = postcss.parse(await readFile(new URL("../app/globals.css", import.meta.url), "utf8"));

function declarations(selector) {
  const values = {};
  css.walkRules((rule) => {
    if (rule.selector !== selector) return;
    rule.walkDecls((decl) => { values[decl.prop] = decl.value; });
  });
  return values;
}

test("桌面和手机表格将圆角外框与滚动区域分层", async () => {
  for (const component of ["MarkdownBody", "MobileMarkdown"]) {
    const source = await readFile(new URL(`../components/${component}.tsx`, import.meta.url), "utf8");
    assert.match(source, /<div className="markdown-table-wrap">\s*<div className="markdown-table-scroll">\s*<table>\{children\}<\/table>\s*<\/div>\s*<\/div>/, `${component} 不能让外框同时承担滚动`);
  }
});

test("表格外框独立裁剪四角，避免 WebKit 滚动层覆盖圆角", () => {
  const frame = declarations(".markdown-table-wrap");
  assert.equal(frame.border, "1px solid var(--border)");
  assert.equal(frame["border-radius"], "var(--radius-lg)");
  assert.equal(frame.overflow, "hidden");
  assert.equal(frame["clip-path"], "inset(0 round var(--radius-lg))");
  assert.equal(frame["overflow-x"], undefined);
  assert.equal(frame["overflow-y"], undefined);
});

test("宽表格仍在内部横向滚动且不改动单元格排版", () => {
  const scroll = declarations(".markdown-table-scroll");
  assert.equal(scroll["overflow-x"], "auto");
  assert.equal(scroll["overflow-y"], "hidden");
  const table = declarations(".markdown-body table");
  assert.equal(table["min-width"], "100%");
  assert.equal(table["border-collapse"], "separate");
  assert.equal(table["border-spacing"], "0");
});
