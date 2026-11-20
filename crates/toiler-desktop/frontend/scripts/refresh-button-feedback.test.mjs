import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const sidebar = await readFile(new URL("../components/SessionSidebar.tsx", import.meta.url), "utf8");
const css = await readFile(new URL("../app/globals.css", import.meta.url), "utf8");

test("文件浏览器与会话列表的刷新按钮只用图标转一圈反馈，不换颜色也不换图标", () => {
  // 曾经的实现是「变绿 + 换成对勾」，两处按钮都不该再有完成态的绿色。
  assert.doesNotMatch(sidebar, /setExplorerRefreshDone|setSessionRefreshDone/);
  assert.doesNotMatch(sidebar, /explorerRefreshDone|sessionRefreshDone/);
  // 这两处按钮所在文件里已经不该再出现 74,222,128 系的绿色状态。
  const refreshButtonArea = sidebar.slice(
    sidebar.indexOf('t("sidebar.refreshExplorer")'),
    sidebar.indexOf('t("sidebar.refresh")'),
  );
  assert.ok(refreshButtonArea.length > 0);
  assert.doesNotMatch(refreshButtonArea, /#4ade80|rgba\(74,222,128/);
  assert.doesNotMatch(sidebar.slice(sidebar.indexOf('t("sidebar.refresh")')), /#4ade80/);
});

test("两处刷新按钮都用顺时针箭头，点击后重挂载图标来重播动画", () => {
  for (const tick of ["explorerRefreshTick", "sessionRefreshTick"]) {
    assert.match(sidebar, new RegExp(`const \\[${tick}, set${tick[0].toUpperCase()}${tick.slice(1)}\\] = useState\\(0\\)`));
    assert.match(sidebar, new RegExp(`set${tick[0].toUpperCase()}${tick.slice(1)}\\(\\(tick\\) => tick \\+ 1\\)`));
    // key 与动画类必须在同一个图标上；初次渲染不加动画类。
    assert.match(sidebar, new RegExp(`<RotateCw\\s+key=\\{${tick}\\}\\s+className=\\{${tick} > 0 \\? REFRESH_SPIN_CLASS : undefined\\}`));
  }
  assert.match(sidebar, /const REFRESH_SPIN_CLASS = "refresh-icon-spin"/);
});

test("转圈动画沿刷新箭头顺时针转一圈，尊重减少动效偏好", () => {
  const rule = css.match(/\n\.refresh-icon-spin\s*\{([^}]+)\}/)?.[1];
  assert.ok(rule, "缺少 .refresh-icon-spin 规则");
  assert.match(rule, /animation:\s*spin\s+[\d.]+m?s\s+linear;/);
  assert.doesNotMatch(rule, /infinite|animation-direction/, "点击反馈只顺时针转一圈");
  const keyframesStart = css.indexOf("@keyframes spin {");
  assert.ok(keyframesStart >= 0, "缺少 @keyframes spin");
  const keyframes = css.slice(keyframesStart, css.indexOf("}", css.indexOf("to ", keyframesStart)));
  assert.match(keyframes, /from\s*\{\s*transform:\s*rotate\(0deg\)/);
  assert.match(keyframes, /to\s*\{\s*transform:\s*rotate\(360deg\)/);
  // 0° 与 360° 是同一姿态，图标不会停歪。
  const reducedMotionStart = css.indexOf("@media (prefers-reduced-motion: reduce)", css.indexOf(".refresh-icon-spin"));
  assert.ok(reducedMotionStart > 0, ".refresh-icon-spin 之后要跟着减少动效的覆盖规则");
  const reducedMotion = css.slice(reducedMotionStart, css.indexOf("\n}", reducedMotionStart));
  assert.match(reducedMotion, /\.refresh-icon-spin\s*\{\s*animation:\s*none;/);
});