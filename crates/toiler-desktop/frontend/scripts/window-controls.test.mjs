import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const script = await readFile(new URL("../public/window-controls.js", import.meta.url), "utf8");

function fixture({ enabled = true, loading = false } = {}) {
  const calls = [];
  const events = {};
  let maximized = false;
  function element() {
    const path = { setAttribute(name, value) { this[name] = value; } };
    return {
      children: [], attributes: {}, events: {},
      appendChild(child) { this.children.push(child); },
      setAttribute(name, value) { this.attributes[name] = value; },
      addEventListener(name, callback) { this.events[name] = callback; },
      querySelector() { return path; },
    };
  }
  const document = {
    readyState: loading ? "loading" : "complete",
    documentElement: { classList: { add() {} } },
    head: element(), body: element(), createElement: element,
    addEventListener(name, callback) { events[name] = callback; },
  };
  const window = {
    __TOILER_CUSTOM_TITLEBAR__: enabled,
    __TAURI_INTERNALS__: {
      async invoke(command, args) {
        calls.push({ command, args });
        if (command.endsWith("toggle_maximize")) maximized = !maximized;
        if (command.endsWith("is_maximized")) return maximized;
      },
    },
    addEventListener(name, callback) { events[name] = callback; },
  };
  vm.runInNewContext(script, { window, document, console });
  return { calls, events, document };
}

const flush = () => new Promise((resolve) => setImmediate(resolve));

test("非 Windows 页面不添加控制按钮，也不调用窗口 API", () => {
  const { calls, document } = fixture({ enabled: false });
  assert.equal(document.body.children.length, 0);
  assert.equal(calls.length, 0);
});

test("独立 HTML 等待 DOM 就绪后仍有完整窗口控制", async () => {
  const { document, events, calls } = fixture({ loading: true });
  assert.equal(document.body.children.length, 0);
  events.DOMContentLoaded();
  await flush();
  const controls = document.body.children[0];
  assert.equal(controls.attributes["data-tauri-drag-region"], "false");
  const [minimize, maximize, close] = controls.children;
  assert.equal(maximize.title, "最大化");
  minimize.events.click();
  maximize.events.click();
  await flush();
  assert.equal(maximize.title, "还原");
  assert.equal(maximize.attributes["aria-label"], "还原");
  maximize.events.click();
  await flush();
  assert.equal(maximize.title, "最大化");
  close.events.click();
  events.resize();
  await flush();
  for (const command of ["minimize", "toggle_maximize", "close", "is_maximized"]) {
    assert.ok(calls.some((call) => call.command === `plugin:window|${command}`));
  }
  assert.ok(calls.every((call) => call.args.label === "main"));
});

test("无边框配置、注入脚本和最小权限保持配套", async () => {
  const rust = await readFile(new URL("../../src/lib.rs", import.meta.url), "utf8");
  assert.match(rust, /#\[cfg\(target_os = "windows"\)\]\s*\{\s*builder = builder.decorations\(false\)/);
  assert.ok(rust.includes('include_str!("../frontend/public/window-controls.js")'));
  assert.ok(!script.includes("//"), "注入脚本压平时不能被单行注释截断");
  const capability = JSON.parse(await readFile(new URL("../../capabilities/default.json", import.meta.url), "utf8"));
  for (const command of ["minimize", "toggle-maximize", "is-maximized", "close"]) {
    assert.ok(capability.permissions.includes(`core:window:allow-${command}`));
  }
});
