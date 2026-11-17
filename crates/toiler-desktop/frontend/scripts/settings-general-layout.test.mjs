import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const styles = readFileSync(new URL("../app/globals.css", import.meta.url), "utf8");
const panel = readFileSync(new URL("../components/SettingsPanel.tsx", import.meta.url), "utf8");
const rule = (selector) => {
  const start = styles.indexOf(`${selector} {`);
  assert.notEqual(start, -1, `缺少样式规则：${selector}`);
  return styles.slice(start, styles.indexOf("}", start) + 1);
};

test("常规页撑满内容区，不再限制为固定宽度", () => {
  const general = rule(".settings-general");
  assert.match(general, /width: 100%/);
  assert.match(general, /flex: 1 0 auto/);
  assert.doesNotMatch(general, /max-width|min\(720px/);
  assert.match(rule(".settings-general-about"), /margin-top: auto/);
  assert.match(panel, /className="settings-general-about"/);
});

test("常规页保留滚动能力，弹性布局不覆盖隐藏页签", () => {
  assert.match(panel, /hidden=\{activeSection !== "general"\}[^>]*className="settings-general-pane"/);
  assert.match(rule(".settings-general-pane"), /overflow-y: auto/);
  assert.doesNotMatch(rule(".settings-general-pane"), /display: flex/);
  assert.match(rule(".settings-general-pane:not([hidden])"), /display: flex/);
});

test("窄屏为文字和右侧控件保留收缩空间", () => {
  assert.match(styles, /@media \(max-width: 600px\)/);
  assert.match(styles, /max-width: min\(140px, 40%\)/);
  assert.match(rule(".settings-row-text"), /overflow-wrap: anywhere/);
});
