import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url, {
  alias: { "@": fileURLToPath(new URL("../", import.meta.url)) },
  jsx: { runtime: "automatic" },
});
const { ExternalLink } = await jiti.import("../components/ExternalLink.tsx");
const url = "https://skills.sh/example?from=settings#readme";

function clickEvent(overrides = {}) {
  return {
    currentTarget: { href: url },
    defaultPrevented: false,
    button: 0,
    metaKey: false,
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    preventDefault() { this.defaultPrevented = true; },
    ...overrides,
  };
}

function setWindow(t, mockWindow) {
  globalThis.window = mockWindow;
  t.after(() => { delete globalThis.window; });
}

test("桌面设置的链接交给系统浏览器，而不是在 WebView 里开新窗口", { timeout: 3000 }, async (t) => {
  const { promise, resolve } = Promise.withResolvers();
  setWindow(t, {
    __PI_WEB_API_ORIGIN__: "http://127.0.0.1:12345",
    __TAURI_INTERNALS__: { invoke: async (command, args) => resolve({ command, args }) },
  });

  const link = ExternalLink({ href: url, children: "技能详情" });
  const event = clickEvent();
  link.props.onClick(event);
  assert.equal(event.defaultPrevented, true);
  assert.deepEqual(await promise, { command: "open_external_url", args: { url } });
});

test("纯浏览器保留原生链接及新标签行为", (t) => {
  setWindow(t, {});
  const link = ExternalLink({ href: url, children: "技能详情" });
  assert.equal(link.type, "a");
  assert.equal(link.props.href, url);
  assert.equal(link.props.target, "_blank");
  assert.equal(link.props.rel, "noopener noreferrer");

  for (const event of [clickEvent(), clickEvent({ metaKey: true }), clickEvent({ button: 1 })]) {
    link.props.onClick(event);
    assert.equal(event.defaultPrevented, false);
  }
});

test("非 HTTP(S) 地址只显示文字，不暴露可点击的链接", () => {
  for (const href of ["javascript:alert(1)", "file:///tmp/example", "//example.com", ""]) {
    const link = ExternalLink({ href, children: "来源" });
    assert.equal(link.props.children, "来源");
    assert.equal(link.props.href, undefined);
    assert.equal(link.props.onClick, undefined);
  }
});

test("模型、技能、插件设置使用共用外链入口，不绕回 WebView 弹窗", async () => {
  for (const component of ["ModelsConfig", "SkillsConfig", "PluginsConfig"]) {
    const source = await readFile(new URL(`../components/${component}.tsx`, import.meta.url), "utf8");
    assert.match(source, /<ExternalLink\s+href=/, component);
    assert.doesNotMatch(source, /<a\s[^>]*\btarget=["']_blank["']/, component);
  }
});
