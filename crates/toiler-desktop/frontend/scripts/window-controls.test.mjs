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

test("右侧栏开关时由窗口最右侧顶栏避让控制按钮", async () => {
  const { document } = fixture();
  const css = document.head.children[0].textContent;
  assert.ok(css.includes(".toiler-custom-titlebar:not(:has(.right-panel-open)) .app-topbar"));
  assert.ok(css.includes(".toiler-custom-titlebar .right-panel-open .app-file-panel-titlebar"));
  assert.ok(css.includes("box-sizing:border-box;padding-right:138px"));
  const shell = await readFile(new URL("../components/AppShell.tsx", import.meta.url), "utf8");
  assert.match(shell, /<div className="app-chrome app-file-panel-titlebar"/);
  const header = shell.slice(shell.indexOf('{/* Right panel tab bar */}'), shell.indexOf('{/* Only the active panel body'));
  assert.ok(header.includes('onClick={() => setRightPanelOpen(false)}'), "收起键必须与标签条一起避让");
});

test("文件预览标题栏空白可拖动，文件标签与操作按钮保持可点击", async () => {
  const shell = await readFile(new URL("../components/AppShell.tsx", import.meta.url), "utf8");
  const header = shell.slice(shell.indexOf('{/* Right panel tab bar */}'), shell.indexOf('{/* Only the active panel body'));
  assert.match(header, /className="app-chrome app-file-panel-titlebar"[^>]*data-tauri-drag-region="deep"/);
  assert.ok(header.includes('onClick={() => setRightPanelOpen(false)}'));
  assert.match(header, /<button[\s\S]*?onClick=\{\(\) => setRightPanelOpen\(false\)\}/, "收起面板仍使用原生按钮，自动排除窗口拖拽");

  const tabs = await readFile(new URL("../components/TabBar.tsx", import.meta.url), "utf8");
  assert.match(tabs, /key=\{tab.id\}[\s\S]*?data-tauri-drag-region="false"\s+onClick=\{\(\) => onSelectTab\(tab.id\)\}/);
  assert.ok(tabs.includes("onAuxClick={(e) => {"), "保留中键关闭文件标签");
  assert.match(tabs, /<button\s+onClick=\{\(e\) => \{ e.stopPropagation\(\); onCloseTab\(tab.id\); \}\}/, "关闭标签仍使用原生按钮");

  const styles = await readFile(new URL("../app/globals.css", import.meta.url), "utf8");
  assert.match(styles, /\.app-chrome,\s*\.app-chrome \*\s*\{[^}]*user-select:\s*none/);
});

test("Git 面板刷新和关闭按钮与顶栏一起避让窗口控制区", async () => {
  const { document } = fixture();
  const css = document.head.children[0].textContent;
  assert.ok(css.includes(".toiler-custom-titlebar .git-panel-header{box-sizing:border-box;padding-right:138px}"));
  assert.ok(css.includes("--toiler-control-color:#1f1f1f"));
  assert.ok(css.includes(".dark .toiler-window-controls{--toiler-control-color:#fff}"));
  assert.ok(css.includes("background:color-mix(in srgb,currentColor 10%,transparent)"));
  assert.ok(css.includes("background:#c42b1c;color:#fff"));
  assert.ok(css.includes(".toiler-window-controls .close:active{background:#a82016;color:#fff}"));
  assert.ok(!css.includes("color:var(--text,#eee)"));
  assert.ok(css.includes("transition:background-color 120ms ease,color 120ms ease"));
  const panel = await readFile(new URL("../components/GitPanel.tsx", import.meta.url), "utf8");
  const header = panel.slice(panel.indexOf("<header"), panel.indexOf("</header>"));
  assert.ok(header.includes('className="git-panel-header"'));
  assert.ok(header.includes('t("git.refreshStatus")'), "刷新按钮必须位于避让顶栏内");
  assert.ok(header.includes('onClick={onClose}'), "关闭按钮必须位于避让顶栏内");
});

test("设置弹窗不继承主窗口控制区的右侧留白", async () => {
  const { document } = fixture();
  const css = document.head.children[0].textContent;
  assert.ok(!css.includes(".settings-titlebar"), "居中弹窗的关闭按钮应保持靠右，不为窗口控制按钮预留空间");
  const styles = await readFile(new URL("../app/globals.css", import.meta.url), "utf8");
  const titlebar = styles.match(/\.settings-titlebar\s*\{([^}]+)\}/)?.[1];
  assert.ok(titlebar);
  assert.match(titlebar, /justify-content:\s*space-between/);
  assert.match(titlebar, /padding:\s*0 8px 0 10px/);
});

test("窗口控制按钮高于普通界面，但低于对话框蒙版", async () => {
  const { document } = fixture();
  const css = document.head.children[0].textContent;
  const controlsRule = css.match(/\.toiler-window-controls\{([^}]+)\}/)?.[1];
  assert.ok(controlsRule);
  const controlsZIndex = Number(controlsRule.match(/z-index:(\d+)/)?.[1]);
  assert.ok(controlsZIndex > 500, "未打开对话框时，窗口按钮应高于普通界面浮层");
  for (const name of ["DirectoryPicker", "ProjectTrustDialog", "MobilePairDialog", "AboutDialog", "SettingsPanel"]) {
    const source = await readFile(new URL(`../components/${name}.tsx`, import.meta.url), "utf8");
    const backdropZIndex = Number(source.match(/zIndex:\s*(\d+)/)?.[1]);
    assert.ok(controlsZIndex < backdropZIndex, `${name} 的蒙版必须遮挡窗口按钮`);
  }
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
