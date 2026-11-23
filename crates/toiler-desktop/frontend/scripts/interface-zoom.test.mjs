import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const read = (path) => readFile(new URL(path, import.meta.url), "utf8");
const script = await read("../public/interface-zoom.js");
const flush = () => new Promise((resolve) => setImmediate(resolve));

function fixture({ desktop = true, mac = true, loading = false, bridge = true, deferred = false } = {}) {
  const calls = [], replies = [], warnings = [];
  const listeners = new Map();
  const rootStyle = new Map();
  const document = {
    readyState: loading ? "loading" : "complete",
    documentElement: { style: { setProperty: (key, value) => rootStyle.set(key, value) } },
    addEventListener(name, listener) { listeners.set(name, listener); },
  };
  const window = {
    __TOILER_DESKTOP__: desktop,
    __TOILER_OVERLAY_TITLEBAR__: mac,
    __TAURI_INTERNALS__: bridge ? {
      invoke(command, { action }) {
        calls.push([command, action]);
        return deferred ? new Promise((resolve, reject) => replies.push({ resolve, reject })) : Promise.resolve(100);
      },
    } : undefined,
    addEventListener(name, listener, capture) { listeners.set(name, listener); if (name === "keydown") assert.equal(capture, true); },
  };
  vm.runInNewContext(script, { window, document, console: { warn: (...args) => warnings.push(args) } });
  function press(key, options = {}) {
    const event = {
      key, metaKey: mac, ctrlKey: !mac,
      preventDefault() { this.defaultPrevented = true; },
      stopPropagation() { this.stopped = true; },
      ...options,
    };
    listeners.get("keydown")?.(event);
    return event.stopped === true && event.defaultPrevented === true;
  }
  return { calls, replies, warnings, listeners, rootStyle, press };
}

const actionNames = (f) => f.calls.map(([, action]) => action);

test("只在桌面壳处理对应平台的缩放快捷键，保留普通键和其它组合键", async () => {
  for (const mac of [true, false]) {
    const f = fixture({ mac });
    assert.equal(f.press("+", { shiftKey: true }), true);
    assert.equal(f.press("="), true);
    assert.equal(f.press("-"), true);
    assert.equal(f.press("0"), true);
    for (const options of [{ altKey: true }, { ctrlKey: mac, metaKey: !mac }, { isComposing: true }, { defaultPrevented: true }]) {
      assert.equal(f.press("+", options), false);
    }
    assert.equal(f.press("0", { shiftKey: true }), false);
    assert.equal(f.press("c"), false);
    await flush();
    assert.deepEqual(actionNames(f), ["restore", "in", "in", "out", "reset"]);
    assert.equal(f.rootStyle.has("--toiler-interface-scale"), mac);
  }
  for (const f of [fixture({ desktop: false }), fixture({ bridge: false })]) {
    assert.equal(f.press("+"), false);
    await flush();
    assert.deepEqual(f.calls, []);
  }
});

test("加载或从页面缓存返回时同步倍率，连续按键与失败后的重置仍按顺序执行", async () => {
  const f = fixture({ loading: true, deferred: true });
  await flush();
  assert.deepEqual(f.calls, []);
  f.listeners.get("DOMContentLoaded")();
  f.press("=");
  f.press("0");
  await flush();
  assert.deepEqual(actionNames(f), ["restore"]);
  f.replies[0].resolve(50);
  await flush();
  assert.equal(f.rootStyle.get("--toiler-interface-scale"), "0.5");
  assert.deepEqual(actionNames(f), ["restore", "in"]);
  f.replies[1].reject(new Error("倍率调整失败"));
  await flush();
  assert.equal(f.warnings.length, 1);
  assert.equal(f.rootStyle.get("--toiler-interface-scale"), "0.5");
  assert.deepEqual(actionNames(f), ["restore", "in", "reset"]);
  f.replies[2].resolve(100);
  await flush();
  assert.equal(f.rootStyle.get("--toiler-interface-scale"), "1");
  f.listeners.get("pageshow")({ persisted: false });
  f.listeners.get("pageshow")({ persisted: true });
  await flush();
  assert.deepEqual(actionNames(f), ["restore", "in", "reset", "restore"]);
});

test("缩放桥仅授权主窗口，macOS 保持原生按钮避让宽度", async () => {
  const native = await read("../../src/lib.rs");
  const build = await read("../../build.rs");
  const capability = JSON.parse(await read("../../capabilities/default.json"));
  const css = await read("../app/globals.css");
  assert.match(native, /include_str!\("\.\.\/frontend\/public\/interface-zoom\.js"\)/);
  assert.match(native, /interface_zoom::adjust_interface_zoom,/);
  assert.match(native, /\.zoom_hotkeys_enabled\(false\)/);
  assert.match(build, /"adjust_interface_zoom"/);
  assert.deepEqual(capability.windows, ["main"]);
  assert.ok(capability.permissions.includes("allow-adjust-interface-zoom"));
  assert.match(css, /--chrome-inset: calc\(92px \/ var\(--toiler-interface-scale, 1\)\)/);
  assert.match(css, /padding-left: calc\(86px \/ var\(--toiler-interface-scale, 1\)\)/);
});
