import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const sidebar = await readFile(new URL("../components/SessionSidebar.tsx", import.meta.url), "utf8");
const css = await readFile(new URL("../app/globals.css", import.meta.url), "utf8");

/**
 * 校验某个 svg 同时带上了“重挂载用的 key”和“一次性转圈类”，
 * 且两者都在同一个开始标签里（否则 key 换个元素就没意义了）。
 */
function assertSvgReplaysSpin(source, tick) {
  const keyAt = source.indexOf(`key={${tick}}`);
  assert.ok(keyAt > 0, `${tick} 必须挂在 svg 的 key 上`);
  const svgAt = source.lastIndexOf("<svg", keyAt);
  const spinAt = source.indexOf("REFRESH_SPIN_CLASS", keyAt);
  const closeAt = source.indexOf("</svg", svgAt);
  assert.ok(svgAt > 0, `${tick} 必须属于一个 svg 图标`);
  assert.ok(spinAt > keyAt, `${tick} 对应的 svg 要带 REFRESH_SPIN_CLASS`);
  assert.ok(closeAt < 0 || spinAt < closeAt, "转圈类必须写在 svg 开始标签内");
  assert.match(source.slice(keyAt), new RegExp(`^key=\\{${tick}\\}\\s*className=\\{${tick} > 0 \\? REFRESH_SPIN_CLASS : undefined\\}`));
}

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

test("两处刷新按钮点击后给图标换 key 并加上一次性转圈类", () => {
  for (const tick of ["explorerRefreshTick", "sessionRefreshTick"]) {
    assert.match(sidebar, new RegExp(`const \\[${tick}, set${tick[0].toUpperCase()}${tick.slice(1)}\\] = useState\\(0\\)`));
    assert.match(sidebar, new RegExp(`set${tick[0].toUpperCase()}${tick.slice(1)}\\(\\(tick\\) => tick \\+ 1\\)`));
  }
  // key 必须挂在 svg 上：只有重挂载才能让同一个动画从头再跑一遍。
  assertSvgReplaysSpin(sidebar, "explorerRefreshTick");
  assertSvgReplaysSpin(sidebar, "sessionRefreshTick");
  // 首次渲染不加类，打开应用时不该白白转一圈。
  assert.match(sidebar, /const REFRESH_SPIN_CLASS = "refresh-icon-spin"/);
});

test("转圈动画只跑一次且起止角度一致，尊重减少动效偏好", () => {
  const rule = css.match(/\n\.refresh-icon-spin\s*\{([^}]+)\}/)?.[1];
  assert.ok(rule, "缺少 .refresh-icon-spin 规则");
  assert.match(rule, /animation:\s*spin\s+[\d.]+m?s\s+linear;/);
  assert.doesNotMatch(rule, /infinite/, "点击反馈只转一圈，不能一直转");
  const keyframesStart = css.indexOf("@keyframes spin {");
  assert.ok(keyframesStart >= 0, "缺少 @keyframes spin");
  const keyframes = css.slice(keyframesStart, css.indexOf("}", css.indexOf("to ", keyframesStart)));
  assert.match(keyframes, /from\s*\{\s*transform:\s*rotate\(0deg\)/);
  assert.match(keyframes, /to\s*\{\s*transform:\s*rotate\(360deg\)/);
  // 0°→360° 视觉上是同一姿态，动画结束不会把图标停歪。
  const reducedMotionStart = css.indexOf("@media (prefers-reduced-motion: reduce)", css.indexOf(".refresh-icon-spin"));
  assert.ok(reducedMotionStart > 0, ".refresh-icon-spin 之后要跟着减少动效的覆盖规则");
  const reducedMotion = css.slice(reducedMotionStart, css.indexOf("\n}", reducedMotionStart));
  assert.match(reducedMotion, /\.refresh-icon-spin\s*\{\s*animation:\s*none;/);
});