import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const read = (path) => readFile(new URL(path, import.meta.url), "utf8");
const css = await read("../app/globals.css");
const shell = await read("../components/AppShell.tsx");
const native = await read("../../src/lib.rs");
const material = await read("../../src/native_titlebar.rs");
const windows = await read("../../src/windows_titlebar.rs");
const script = await read("../public/native-titlebar.js");
const flush = () => new Promise((resolve) => setImmediate(resolve));

function fixture({ platform = "macos", loading = false, dark = false, result = "vibrancy", deferred = false, failure = false } = {}) {
  const calls = [], pending = [], warnings = [], events = {}, attributes = {};
  const classes = new Set(dark ? ["dark"] : []);
  let observer;
  const root = {
    classList: {
      add: (name) => classes.add(name),
      contains: (name) => classes.has(name),
      toggle(name, value) { if (value) classes.add(name); else classes.delete(name); },
    },
    setAttribute(name, value) { attributes[name] = value; },
  };
  const document = {
    documentElement: root,
    readyState: loading ? "loading" : "complete",
    addEventListener(name, fn) { events[name] = fn; },
  };
  const window = {
    __TOILER_OVERLAY_TITLEBAR__: platform === "macos",
    __TOILER_CUSTOM_TITLEBAR__: platform === "windows",
    __TAURI_INTERNALS__: {
      invoke(command, args) {
        calls.push({ command, args });
        if (deferred) return new Promise((resolve) => pending.push(resolve));
        return failure ? Promise.reject(new Error("权限未授予")) : Promise.resolve(result);
      },
    },
    addEventListener(name, fn) { events[name] = fn; },
  };
  class MutationObserver {
    constructor(callback) { this.callback = callback; observer = this; }
    observe(target, options) { this.target = target; this.options = options; }
  }
  vm.runInNewContext(script, { document, window, MutationObserver, console: { warn(...args) { warnings.push(args); } } });
  return { calls, pending, warnings, events, classes, root, attributes, observer: () => observer };
}

test("macOS 材质位于 WebView 下方，遵守系统降低透明度设置", () => {
  assert.match(material, /native\.setOpaque\(false\)/);
  assert.match(material, /native\.setBackgroundColor\(Some\(&NSColor::clearColor\(\)\)\)/);
  assert.match(material, /NSVisualEffectView::new\(main\)/);
  assert.match(material, /setIdentifier\(Some\(ns_string!\("toiler-live-titlebar"\)\)\)/);
  assert.match(material, /addSubview_positioned_relativeTo\(&effect, NSWindowOrderingMode::Below, None\)/);
  assert.match(material, /setBlendingMode\(NSVisualEffectBlendingMode::BehindWindow\)/);
  assert.match(material, /setState\(NSVisualEffectState::Active\)/);
  assert.match(material, /effect\.setHidden\(reduce_transparency\)/);
  assert.match(material, /Ok\(!reduce_transparency\)/);
  assert.doesNotMatch(material, /setAlphaValue|CGWindowListCreateImage|CGDisplayCreateImage/);
});

test("布局更新复用 macOS 材质，原生命令返回实际执行结果而非排队确认", () => {
  assert.ok(material.indexOf("for child in content.subviews()") < material.indexOf("NSVisualEffectView::new(main)"));
  assert.match(material, /effect\.setFrame\(frame\)/);
  assert.match(material, /ViewWidthSizable/);
  assert.match(native, /WindowEvent::Resized\(_\)/);
  assert.match(native, /WindowEvent::ScaleFactorChanged/);
  assert.match(native, /receiver\.await/);
  assert.match(native, /sender\.send\(titlebar::apply\(&target, dark\)\)/);
  assert.match(native, /if overlay_titlebar \|\| custom_titlebar/);
  assert.match(native, /decorations\(false\)\.transparent\(true\)/);
  assert.doesNotMatch(script, /setInterval|setTimeout|\/\//);
});

test("Windows 使用官方 Acrylic 并检查合成、透明与高对比度，失败可清理", () => {
  for (const api of ["DwmIsCompositionEnabled", "RegGetValueW", "SystemParametersInfoW", "DWMSBT_TRANSIENTWINDOW", "DWMWA_USE_IMMERSIVE_DARK_MODE", "DwmExtendFrameIntoClientArea", "DWMSBT_NONE"]) {
    assert.ok(windows.includes(api));
  }
  assert.match(windows, /if result < 0/);
  assert.match(windows, /windows_block_reason/);
  assert.doesNotMatch(windows, /SetWindowCompositionAttribute\(|Command::new|RegSetValue/);
});

test("标题栏命令具备处理函数、命令清单和主窗口最小权限", async () => {
  const build = await read("../../build.rs");
  const capability = JSON.parse(await read("../../capabilities/default.json"));
  const commands = [...script.matchAll(/invoke\("([^"]+)"/g)].map((match) => match[1]);
  assert.deepEqual(commands, ["sync_titlebar_appearance"]);
  for (const command of commands) {
    assert.ok(native.includes(`${command},`));
    assert.ok(build.includes(`"${command}"`));
    assert.ok(capability.permissions.includes(`allow-${command.replaceAll("_", "-")}`));
  }
  assert.deepEqual(capability.windows, ["main"]);
  assert.ok(capability.remote.urls.includes("http://127.0.0.1:*"));
});

test("DOM 就绪后请求材质，必须收到成功回复才能透明", async () => {
  const f = fixture({ loading: true, deferred: true });
  assert.equal(f.calls.length, 0);
  f.events.DOMContentLoaded();
  assert.ok(f.classes.has("pi-overlay-chrome"));
  assert.equal(f.classes.has("toiler-native-titlebar"), false);
  assert.equal(f.observer().target, f.root);
  assert.equal(f.observer().options.attributeFilter[0], "class");
  f.pending[0]("vibrancy");
  await flush();
  assert.ok(f.classes.has("toiler-native-titlebar"));
  assert.equal(f.attributes["data-titlebar-material"], "vibrancy");
});

test("Windows Acrylic 成功后透明，但不继承 macOS 红绿灯避让", async () => {
  const f = fixture({ platform: "windows", result: "acrylic" });
  await flush();
  assert.ok(f.classes.has("toiler-native-titlebar"));
  assert.equal(f.classes.has("pi-overlay-chrome"), false);
  assert.equal(f.attributes["data-titlebar-material"], "acrylic");
});

test("不支持、未知状态或 IPC 失败时保持实色", async () => {
  for (const result of ["solid", undefined, "unknown"]) {
    const f = fixture({ platform: "windows", result: result ?? null });
    await flush();
    assert.equal(f.classes.has("toiler-native-titlebar"), false);
    assert.equal(f.attributes["data-titlebar-material"], "solid");
  }
  const denied = fixture({ failure: true });
  await flush();
  assert.equal(denied.classes.has("toiler-native-titlebar"), false);
  assert.equal(denied.warnings.length, 1);
});

test("Linux 与网页默认实色，不申请原生材质", () => {
  for (const platform of ["linux", "web"]) {
    const f = fixture({ platform });
    assert.equal(f.calls.length, 0);
    assert.equal(f.observer(), undefined);
    assert.equal(f.classes.has("toiler-native-titlebar"), false);
  }
});

test("主题变化仅请求必要更新，页面就绪和重新聚焦会重查系统能力", async () => {
  const f = fixture({ dark: true });
  await flush();
  assert.equal(f.calls[0].args.dark, true);
  f.observer().callback();
  assert.equal(f.calls.length, 1);
  f.classes.delete("dark");
  f.observer().callback();
  assert.equal(f.calls[1].args.dark, false);
  f.events["pi-app-ready"]();
  f.events.focus();
  assert.equal(f.calls.length, 4);
});

test("快速切换主题时忽略过期材质回复", async () => {
  const f = fixture({ deferred: true });
  f.classes.add("dark");
  f.observer().callback();
  f.pending[1]("solid");
  await flush();
  f.pending[0]("vibrancy");
  await flush();
  assert.equal(f.classes.has("toiler-native-titlebar"), false);
  assert.equal(f.attributes["data-titlebar-material"], "solid");
});

test("从系统设置切回时可撤销已启用的 Acrylic", async () => {
  const f = fixture({ platform: "windows", deferred: true });
  f.pending[0]("acrylic");
  await flush();
  assert.ok(f.classes.has("toiler-native-titlebar"));
  f.events.focus();
  f.pending[1]("solid");
  await flush();
  assert.equal(f.classes.has("toiler-native-titlebar"), false);
});

test("平台原生依赖与 Tauri 透明配置保持一致", async () => {
  const cargo = await read("../../Cargo.toml");
  assert.match(cargo, /"macos-private-api"/);
  assert.match(cargo, /"NSVisualEffectView"/);
  assert.match(cargo, /target\.'cfg\(target_os = "windows"\)'\.dependencies/);
  assert.match(cargo, /"Win32_Graphics_Dwm"/);
  const config = JSON.parse(await read("../../tauri.conf.json"));
  assert.equal(config.app.macOSPrivateApi, true);
});

test("透明 CSS 只由原生成功能力类开启，Windows 控制区不留实色块", async () => {
  const block = css.match(/\.toiler-native-titlebar \.app-workspace\s*\{([^}]+)\}/)?.[1];
  assert.ok(block);
  assert.doesNotMatch(css, /titlebar-frost|--titlebar-frost-tint/);
  assert.doesNotMatch(block, /url\(|backdrop-filter|opacity:/);
  assert.match(css, /html\.toiler-native-titlebar,\s*html\.toiler-native-titlebar body\s*\{\s*background: transparent;/);
  assert.match(css, /\.toiler-native-titlebar \.toiler-window-controls\s*\{\s*background: transparent;/);
  assert.doesNotMatch(css, /html\.pi-overlay-chrome body\s*\{\s*background: transparent;/);
  /* 侧栏有自己的一档浅色底（--sidebar-bg），渐变必须引它而不是 --bg-chrome，
     否则桌面窗口的侧栏会比浏览器里深一档。 */
  for (const [surface, color] of [["workspace", "bg"], ["sidebar", "sidebar-bg"], ["file-panel", "bg"]]) {
    assert.ok(block.includes(`--${surface}-surface: linear-gradient(to bottom,\n    transparent var(--native-titlebar-height),\n    var(--${color}) var(--native-titlebar-height))`));
  }
  const layout = await read("../lib/panel-layout.ts");
  const height = layout.match(/CHAT_TOP_BAR_HEIGHT = (\d+)/)?.[1];
  assert.ok(css.includes(`--native-titlebar-height: calc(${height}px + env(safe-area-inset-top))`));
  assert.match(native, new RegExp(`TITLEBAR_HEIGHT: f64 = ${height}\\.0`));
});

test("三列标题区保留背景回退、窗口拖动与按钮交互入口", () => {
  assert.match(shell, /className="app-workspace"/);
  for (const surface of ["workspace", "sidebar", "file-panel", "titlebar"]) {
    assert.ok(shell.includes(`var(--${surface}-surface, var(--bg`));
  }
  assert.equal(shell.match(/background: "var\(--titlebar-surface, var\(--bg-chrome\)\)"/g)?.length, 2);
  assert.match(shell, /className="app-chrome app-topbar"[\s\S]*?data-tauri-drag-region="deep"/);
  assert.match(shell, /className="app-chrome app-file-panel-titlebar" data-tauri-drag-region="deep"/);
});

test("嵌入侧栏的 Git 面板不继承原生标题栏材质或拖动区", async () => {
  assert.doesNotMatch(css, /--git-panel-surface/);
  assert.match(css, /\.git-panel\s*\{[^}]*background: var\(--sidebar-bg, var\(--bg\)\)/);
  const panel = await read("../components/GitPanel.tsx");
  assert.doesNotMatch(panel, /CHAT_TOP_BAR_HEIGHT|data-tauri-drag-region/);
});
