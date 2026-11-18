import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const read = (path) => readFile(new URL(path, import.meta.url), "utf8");
const css = await read("../app/globals.css");
const shell = await read("../components/AppShell.tsx");
const native = await read("../../src/lib.rs");
const material = await read("../../src/native_titlebar.rs");
const script = await read("../public/native-titlebar.js");

function fixture({ enabled = true, loading = false, dark = false } = {}) {
  const calls = [];
  const events = {};
  const classes = new Set(dark ? ["dark"] : []);
  let observer;
  const root = { classList: { add: (name) => classes.add(name), contains: (name) => classes.has(name) } };
  const document = {
    documentElement: root,
    readyState: loading ? "loading" : "complete",
    addEventListener(name, fn) { events[name] = fn; },
  };
  const window = {
    __TOILER_OVERLAY_TITLEBAR__: enabled,
    __TAURI_INTERNALS__: { invoke(command, args) { calls.push({ command, args }); return Promise.resolve(); } },
    addEventListener(name, fn) { events[name] = fn; },
  };
  class MutationObserver {
    constructor(callback) { this.callback = callback; observer = this; }
    observe(target, options) { this.target = target; this.options = options; }
  }
  vm.runInNewContext(script, { document, window, MutationObserver, console });
  return { calls, events, classes, root, observer: () => observer };
}

test("原生标题栏使用具名实时材质，放在 WebView 下方而非叠在文字上", () => {
  assert.match(native, /\.transparent\(true\)/);
  assert.match(material, /native\.setOpaque\(false\)/);
  assert.match(material, /native\.setBackgroundColor\(Some\(&NSColor::clearColor\(\)\)\)/);
  assert.match(material, /NSVisualEffectView::new\(main\)/);
  assert.match(material, /setIdentifier\(Some\(ns_string!\("toiler-live-titlebar"\)\)\)/);
  assert.match(material, /addSubview_positioned_relativeTo\(&effect, NSWindowOrderingMode::Below, None\)/);
  assert.match(material, /setBlendingMode\(NSVisualEffectBlendingMode::BehindWindow\)/);
  assert.match(material, /setState\(NSVisualEffectState::Active\)/);
  assert.doesNotMatch(material, /setAlphaValue|CGWindowListCreateImage|CGDisplayCreateImage/);
  assert.match(material, /accessibilityDisplayShouldReduceTransparency/);
});

test("布局和页面就绪时复用原生材质，主题同步不依赖屏幕轮询", () => {
  const lookup = material.indexOf("for child in content.subviews()");
  const creation = material.indexOf("NSVisualEffectView::new(main)");
  assert.ok(lookup >= 0 && lookup < creation);
  assert.match(material, /effect\.setFrame\(frame\)/);
  assert.match(material, /setAutoresizingMask/);
  assert.match(material, /ViewWidthSizable/);
  assert.match(native, /WindowEvent::Resized\(_\)/);
  assert.match(native, /WindowEvent::ScaleFactorChanged/);
  assert.match(native, /include_str!\("\.\.\/frontend\/public\/native-titlebar\.js"\)/);
  assert.match(native, /sync_titlebar_appearance,/);
  assert.doesNotMatch(script, /setInterval|setTimeout|\/\//);
});

test("标题栏主题同步命令具备处理函数、生成权限和主窗口能力授权", async () => {
  const build = await read("../../build.rs");
  const capability = JSON.parse(await read("../../capabilities/default.json"));
  const commands = [...script.matchAll(/invoke\("([^"]+)"/g)].map((match) => match[1]);
  assert.deepEqual(commands, ["sync_titlebar_appearance"]);
  for (const command of commands) {
    assert.ok(native.includes(`${command},`));
    assert.ok(build.includes(`"${command}"`), "必须生成命令权限，不能只注册处理函数");
    assert.ok(capability.permissions.includes(`allow-${command.replaceAll("_", "-")}`), "主窗口必须显式授权主题同步");
  }
  assert.deepEqual(capability.windows, ["main"]);
  assert.ok(capability.remote.urls.includes("http://127.0.0.1:*"));
  assert.match(material, /effectiveAppearance\(\)\.name\(\)/);
});

test("DOM 就绪后同步主题，切换深浅色只发一次必要更新", () => {
  const f = fixture({ loading: true });
  assert.equal(f.calls.length, 0);
  f.events.DOMContentLoaded();
  assert.ok(f.classes.has("pi-overlay-chrome"));
  assert.equal(f.observer().target, f.root);
  assert.equal(f.observer().options.attributeFilter[0], "class");
  assert.equal(f.calls[0].command, "sync_titlebar_appearance");
  assert.equal(f.calls[0].args.dark, false);
  f.classes.add("dark");
  f.observer().callback();
  assert.equal(f.calls.length, 2);
  assert.equal(f.calls[1].args.dark, true);
  f.observer().callback();
  assert.equal(f.calls.length, 2);
  f.events["pi-app-ready"]();
  assert.equal(f.calls.length, 3);
});

test("页面已为深色时直接同步，非 macOS 页面不调用原生材质", () => {
  const dark = fixture({ dark: true });
  assert.equal(dark.calls[0].args.dark, true);
  const web = fixture({ enabled: false });
  assert.equal(web.calls.length, 0);
  assert.equal(web.observer(), undefined);
});

test("原生透明所需的 Tauri 功能与配置保持一致", async () => {
  const cargo = await read("../../Cargo.toml");
  assert.match(cargo, /"macos-private-api"/);
  assert.match(cargo, /"NSVisualEffectView"/);
  const config = JSON.parse(await read("../../tauri.conf.json"));
  assert.equal(config.app.macOSPrivateApi, true);
});

test("网页标题区完全透出原生材质，不再使用 PNG 或高不透明度模拟底色", async () => {
  const block = css.match(/\.pi-overlay-chrome \.app-workspace\s*\{([^}]+)\}/)?.[1];
  assert.ok(block);
  assert.doesNotMatch(css, /titlebar-frost|--titlebar-frost-tint/);
  assert.doesNotMatch(block, /url\(|backdrop-filter|opacity:/);
  assert.match(css, /html\.pi-overlay-chrome,\s*html\.pi-overlay-chrome body\s*\{\s*background: transparent;/);
  for (const [surface, color] of [["workspace", "bg"], ["sidebar", "bg-chrome"], ["file-panel", "bg"]]) {
    assert.ok(block.includes(`--${surface}-surface: linear-gradient(to bottom,\n    transparent var(--native-titlebar-height),\n    var(--${color}) var(--native-titlebar-height))`));
  }
  const layout = await read("../lib/panel-layout.ts");
  const height = layout.match(/CHAT_TOP_BAR_HEIGHT = (\d+)/)?.[1];
  assert.ok(height);
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
