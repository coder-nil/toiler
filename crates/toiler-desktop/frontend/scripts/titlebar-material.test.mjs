import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const read = (path) => readFile(new URL(path, import.meta.url), "utf8");
const css = await read("../app/globals.css");
const shell = await read("../components/AppShell.tsx");
const native = await read("../../src/lib.rs");

test("macOS 覆盖式标题栏在窗口失焦时仍保持原生磨砂材质", () => {
  const mac = native.slice(native.indexOf('// macOS：把系统标题栏'), native.indexOf('// Windows 使用页内顶栏'));
  assert.match(mac, /#\[cfg\(target_os = "macos"\)\]/);
  assert.match(mac, /\.transparent\(true\)/);
  assert.match(mac, /Effect::UnderWindowBackground/);
  assert.match(mac, /EffectState::Active/);
  assert.match(native, /__TOILER_OVERLAY_TITLEBAR__ = \{overlay_titlebar\};/);
  assert.match(native, /document\.documentElement\.classList\.add\('pi-overlay-chrome'\)/);
});

test("磨砂层保持完整强度并模糊窗口后方，不能通过降低 alpha 冒充磨砂", () => {
  assert.match(native, /native\.setOpaque\(false\)/);
  assert.match(native, /native\.setBackgroundColor\(Some\(&NSColor::clearColor\(\)\)\)/);
  assert.match(native, /view\.downcast_ref::<NSVisualEffectView>\(\)/);
  assert.match(native, /effect\.setAlphaValue\(1\.0\)/);
  assert.match(native, /effect\.setBlendingMode\(NSVisualEffectBlendingMode::BehindWindow\)/);
  assert.match(native, /effect\.setState\(NSVisualEffectState::Active\)/);
  assert.match(native, /effect\.setMaterial\(NSVisualEffectMaterial::UnderWindowBackground\)/);
  assert.doesNotMatch(native, /setAlphaValue\(0\./);
  assert.doesNotMatch(native, /native\.setAlphaValue/);
  assert.match(native, /if count == 0/);
});

test("原生透明所需的 Tauri 功能与配置保持一致", async () => {
  assert.match(await read("../../Cargo.toml"), /"macos-private-api"/);
  const config = JSON.parse(await read("../../tauri.conf.json"));
  assert.equal(config.app.macOSPrivateApi, true);
});

test("透明仅作用于 macOS 主工作台，正文从标题带下沿恢复实色", async () => {
  assert.match(css, /html\.pi-overlay-chrome,\s*html\.pi-overlay-chrome body\s*\{\s*background: transparent;/);
  assert.match(css, /\.pi-overlay-chrome \.app-workspace\s*\{/);
  const layout = await read("../lib/panel-layout.ts");
  const height = layout.match(/CHAT_TOP_BAR_HEIGHT = (\d+)/)?.[1];
  assert.ok(height);
  assert.ok(css.includes(`--native-titlebar-height: calc(${height}px + env(safe-area-inset-top))`));
  assert.match(css, /--workspace-surface: linear-gradient\(to bottom,\s*transparent var\(--native-titlebar-height\),\s*var\(--bg\) var\(--native-titlebar-height\)\)/);
  assert.match(css, /--sidebar-surface: linear-gradient\(to bottom,\s*transparent var\(--native-titlebar-height\),\s*var\(--bg-chrome\) var\(--native-titlebar-height\)\)/);
});

test("工作台及三列标题区域均打通背景，非 macOS 保留原背景回退", () => {
  assert.match(shell, /className="app-workspace"/);
  for (const surface of ["workspace", "sidebar", "file-panel", "titlebar"]) {
    assert.ok(shell.includes(`var(--${surface}-surface, var(--bg`));
  }
  assert.equal(shell.match(/background: "var\(--titlebar-surface, var\(--bg-chrome\)\)"/g)?.length, 2);
  assert.match(shell, /className="app-chrome app-topbar"[\s\S]*?data-tauri-drag-region="deep"/);
  assert.match(shell, /className="app-chrome app-file-panel-titlebar" data-tauri-drag-region="deep"/);
});
