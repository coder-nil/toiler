import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { createJiti } from "jiti";

const { subscribeToWindowForeground } = await createJiti(import.meta.url).import("../lib/window-foreground.ts");

const read = (path) => readFile(new URL(`../${path}`, import.meta.url), "utf8");
const sidebar = await read("components/SessionSidebar.tsx");
const shell = await read("components/AppShell.tsx");
const panel = await read("components/GitPanel.tsx");
const explorer = sidebar.match(/<section className=\{styles\.explorer\}[\s\S]*?<\/section>/)?.[0];

test("Git 开关移到文件浏览器，底部和文件浏览器不再显示旧入口", () => {
  assert.ok(explorer);
  assert.match(explorer, /onClick=\{toggleGitPanel\}\s+title=\{t\("common\.git"\)\}\s+pressed=\{gitPanelOpen\}/);
  assert.doesNotMatch(explorer, /openUploadPicker|uploadFilesTitle/);
  assert.doesNotMatch(shell, /<GitPanel\b|translate\("common\.git"\)/);
});

test("Git 开关与文件或 Git 刷新共用同一组显隐工具栏，不再显示关闭按钮", () => {
  assert.ok(explorer);
  assert.match(explorer, /gitPanelOpen \? \(\s*<h2[^>]*gitHeading[^>]*>\s*<GitBranch[^>]*\/>\s*<span>\{t\("common\.git"\)\}<\/span>/);
  assert.match(explorer, /<div className=\{styles\.sectionActions\}>\s*\{gitPanelOpen \? \(\s*<div className=\{styles\.gitRefreshSlot\} ref=\{setGitRefreshTarget\} \/>\s*\) : \([\s\S]*?t\("sidebar\.refreshExplorer"\)[\s\S]*?<\/>\s*\)\}\s*\{showExplorerGitButton && \(\s*<ToolbarIconButton\s+onClick=\{toggleGitPanel\}/);
  assert.match(explorer, /refreshTarget=\{gitRefreshTarget\}/);
  assert.match(panel, /refreshTarget && createPortal\([\s\S]*?t\("git\.refreshStatus"\)[\s\S]*?refreshTarget,/);
  assert.doesNotMatch(panel, /<header|git\.close/);
});

test("Git 面板仅替换文件树的显示，并保留子仓库和差异预览入口", () => {
  assert.ok(explorer);
  assert.match(explorer, /hidden=\{gitPanelOpen\}>\s*<FileExplorer/);
  assert.match(explorer, /onOpenRepositoryGitPanel=\{openRepositoryGitPanel\}/);
  assert.match(explorer, /\{gitPanelCwd && \(\s*<GitPanel\s+key=\{gitPanelCwd\}\s+cwd=\{gitPanelCwd\}/);
  assert.match(explorer, /onClose=\{closeGitPanelAndRestoreFocus\}\s+onChanged=\{refreshAfterGitChange\}\s+onOpenFile=\{onOpenFile/);
  assert.match(sidebar, /gitTarget\.explorerCwd === explorerCwd \? gitTarget\.cwd : null/);
});

test("分支选择框显示当前工作树的同步数字，Git 面板不重复显示地址或分支摘要", () => {
  assert.doesNotMatch(panel, /git-panel-path|className="git-summary"|\{summary\.remote\}<\/div>/);
  assert.match(panel, /t\("git\.editRemote"\)/);
  assert.match(sidebar, /worktreeState\.forCwd === selectedCwd \? worktreeState\.branchSync : null/);
  const selector = sidebar.match(/<button\s+type="button"\s+aria-expanded=\{wtDropdownOpen\}[\s\S]*?<\/button>/)?.[0];
  assert.ok(selector);
  assert.match(selector, /branchSync &&[\s\S]*?branchSync\.ahead[\s\S]*?branchSync\.behind/);
  assert.match(sidebar, /const refreshAfterGitChange = useCallback\(\(\) => \{\s*refreshExplorer\(\);\s*setWtRefreshKey/);
  assert.match(sidebar, /subscribeToWindowForeground\(\(\) => \{ void loadWorktrees\(true\); \}\)/);
});

function foregroundWindow() {
  const target = new EventTarget();
  target.document = Object.assign(new EventTarget(), { visibilityState: "visible" });
  const frames = new Map();
  let nextFrame = 0;
  target.requestAnimationFrame = (callback) => {
    frames.set(++nextFrame, callback);
    return nextFrame;
  };
  target.cancelAnimationFrame = (frame) => frames.delete(frame);
  target.paint = () => {
    for (const [frame, callback] of frames) {
      frames.delete(frame);
      callback();
    }
  };
  target.setVisibility = (visibilityState) => {
    target.document.visibilityState = visibilityState;
    target.document.dispatchEvent(new Event("visibilitychange"));
  };
  return target;
}

test("回到前台时合并可见性和焦点事件，窗口未隐藏时重新聚焦也会刷新", () => {
  const target = foregroundWindow();
  let refreshes = 0;
  const stop = subscribeToWindowForeground(() => refreshes++, target);

  target.setVisibility("hidden");
  target.dispatchEvent(new Event("focus"));
  target.paint();
  assert.equal(refreshes, 0);

  target.setVisibility("visible");
  target.dispatchEvent(new Event("focus"));
  target.paint();
  assert.equal(refreshes, 1);

  target.dispatchEvent(new Event("focus"));
  target.paint();
  assert.equal(refreshes, 2);
  stop();
});

test("刷新前再次隐藏或切换项目卸载订阅时，不再触发旧刷新", () => {
  const target = foregroundWindow();
  const stop = subscribeToWindowForeground(() => assert.fail("不应触发刷新"), target);

  target.dispatchEvent(new Event("focus"));
  target.setVisibility("hidden");
  target.paint();

  target.setVisibility("visible");
  stop();
  target.paint();
  target.dispatchEvent(new Event("focus"));
  target.setVisibility("visible");
  target.paint();
});

test("桌面主窗口的原生焦点事件不依赖 DOM 焦点、可见性或绘制", async () => {
  const target = foregroundWindow();
  target.__PI_WEB_DESKTOP__ = true;
  let refreshes = 0;
  let nativeFocus;
  let unlistened = 0;
  const stop = subscribeToWindowForeground(() => refreshes++, target, async (event, callback, options) => {
    assert.equal(event, "tauri://focus");
    assert.deepEqual(options.target, { kind: "Window", label: "main" });
    nativeFocus = callback;
    return () => { unlistened++; };
  });
  await Promise.resolve();
  target.setVisibility("hidden");
  nativeFocus();
  assert.equal(refreshes, 1);
  target.setVisibility("visible");
  target.dispatchEvent(new Event("focus"));
  target.paint();
  assert.equal(refreshes, 1);

  nativeFocus();
  assert.equal(refreshes, 2);
  stop();
  nativeFocus();
  assert.equal(refreshes, 2);
  assert.equal(unlistened, 1);
});

test("桌面原生监听失败时仍能通过网页事件刷新", async () => {
  const target = foregroundWindow();
  target.__PI_WEB_DESKTOP__ = true;
  let refreshes = 0;
  const stop = subscribeToWindowForeground(() => refreshes++, target, async () => {
    throw new Error("无法监听原生事件");
  });
  await Promise.resolve();
  target.dispatchEvent(new Event("focus"));
  target.paint();
  assert.equal(refreshes, 1);
  stop();
});

test("切换项目时清理尚未完成注册的原生监听", async () => {
  const target = foregroundWindow();
  target.__PI_WEB_DESKTOP__ = true;
  let unlistened = false;
  const stop = subscribeToWindowForeground(() => assert.fail("不应触发旧项目刷新"), target, async () => {
    return () => { unlistened = true; };
  });
  stop();
  await Promise.resolve();
  assert.equal(unlistened, true);
});
