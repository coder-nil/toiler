import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const css = await readFile(new URL("../app/globals.css", import.meta.url), "utf8");
const shell = await readFile(new URL("../components/AppShell.tsx", import.meta.url), "utf8");

test("桌面顶栏按钮统一为 Codex 风格的轻量图标控件", () => {
  assert.match(css, /\.app-topbar-actions\s*\{[\s\S]*?gap:\s*4px\s*!important/);
  assert.match(css, /\.app-topbar \.app-toolbar-btn\s*\{[\s\S]*?width:\s*28px[\s\S]*?height:\s*28px/);
  assert.match(css, /\.app-topbar \.app-toolbar-btn:hover:not\(:disabled\)\s*\{[\s\S]*?background:\s*color-mix\(in srgb, currentColor 10%, transparent\)/);
  assert.match(css, /\.app-topbar \.app-toolbar-btn\[data-active="true"\]\s*\{[\s\S]*?background:\s*color-mix\(in srgb, currentColor 12%, transparent\)[\s\S]*?color:\s*var\(--text\)/);
  const activeRule = css.match(/\.app-topbar \.app-toolbar-btn\[data-active="true"\]\s*\{([^}]+)\}/)?.[1];
  assert.ok(activeRule);
  assert.doesNotMatch(activeRule, /var\(--accent\)/);
  assert.match(css, /\.app-topbar-stats\s*\{[\s\S]*?height:\s*28px\s*!important/);
  assert.match(css, /\.app-topbar-stats:hover:not\(:disabled\)\s*\{[\s\S]*?background:\s*color-mix\(in srgb, currentColor 10%, transparent\)/);
  assert.match(css, /\.app-topbar \.app-toolbar-btn:focus-visible,[\s\S]*?outline:\s*2px solid color-mix\(in srgb, var\(--text\) 55%, transparent\)/);
  assert.doesNotMatch(shell, /style=\{\{ color: systemPrompt \? "var\(--accent\)"/);
});

test("历史、分支、系统提示和文件按钮共享顶栏操作组", () => {
  assert.match(shell, /className="app-chrome app-topbar-actions"/);
  assert.match(shell, /className=\{mobile \? "mobile-session-stats" : "app-topbar-stats"\}/);
  assert.match(shell, /renderChatToolbarActions\(false\)/);
  assert.match(shell, /renderSessionStatsButton\(false\)/);
  assert.match(shell, /renderMainFileToggle\(false\)/);
  assert.match(shell, /className="app-toolbar-btn"\s+data-active="true"\s+style=\{\{\s+marginRight: 8/);
  assert.doesNotMatch(shell, /event\.currentTarget\.style\.color = "var\(--accent\)"/);
});
