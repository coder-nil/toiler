import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { createJiti } from "jiti";

const { subscribeToWindowForeground } = await createJiti(import.meta.url).import("../lib/window-foreground.ts");

const read = (path) => readFile(new URL(`../${path}`, import.meta.url), "utf8");
const sidebar = await read("components/SessionSidebar.tsx");
const shell = await read("components/AppShell.tsx");
const panel = await read("components/GitPanel.tsx");
const workspace = await read("components/GitWorkspace.tsx");
const history = await read("components/GitHistoryPanel.tsx");
const explorer = sidebar.match(/<section className=\{styles\.explorer\}[\s\S]*?<\/section>/)?.[0];

test("侧栏项目和子仓库入口打开同一个 Git 工作区，不替换文件树", () => {
  assert.ok(explorer);
  assert.match(explorer, /onClick=\{toggleGitPanel\}\s+title=\{t\("gitWorkspace\.title"\)\}\s+pressed=\{gitPanelOpen\}/);
  assert.match(explorer, /onOpenRepositoryGitPanel=\{openRepositoryGitPanel\}/);
  assert.match(sidebar, /if \(explorerCwd\) onToggleGitWorkspace\(cwd, explorerCwd\)/);
  assert.doesNotMatch(explorer, /<GitPanel\b|hidden=\{gitPanelOpen\}|openUploadPicker/);
});

test("Git 工作区隐藏但不卸载聊天，保留返回入口并刷新侧栏", () => {
  assert.match(shell, /inert=\{Boolean\(gitWorkspace\)\}[\s\S]*?<ChatWindow[\s\S]*?\{gitWorkspace && <GitWorkspace/);
  assert.match(shell, /const filePanelVisible = rightPanelOpen && !gitWorkspace/);
  assert.match(workspace, /onClick=\{onClose\}[\s\S]*?gitWorkspace\.back/);
  assert.match(workspace, /refreshTarget=\{refreshTarget\}/);
  assert.match(panel, /refreshTarget && createPortal\([\s\S]*?t\("git\.refreshStatus"\)[\s\S]*?refreshTarget,/);
  assert.match(shell, /const refreshAfterGitChange = useCallback\(\(\) => \{\s*handleExplorerRefresh\(\);\s*setGitWorkspaceRefreshKey/);
});

test("当前更改与历史文件都送到共用差异区，保留各自的比较基准", () => {
  assert.match(panel, /onSelectFile\(\{ scope: "unstaged", path: file\.filePath \}\)/);
  assert.match(panel, /onSelectFile\(\{ scope: "staged", path: file\.filePath \}\)/);
  assert.match(history, /onSelectFile\(\{ scope: "commit", hash: commit\.hash, path: file\.path, oldPath: file\.oldPath \}\)/);
  assert.match(workspace, /<WorkspaceDiff cwd=\{cwd\} selection=\{selection\}/);
  assert.doesNotMatch(history, /showModal\(|<dialog\b/);
});

test("Git 面板在远程区显示地址，分支同步数字仍由工作树选择框展示", () => {
  assert.doesNotMatch(panel, /git-panel-path|className="git-summary"/);
  assert.match(panel, /\{summary\.remote && !editingRemote && <div className="git-actions-row"[^\n]*flexWrap: "nowrap"[^\n]*>\s*<div className="git-remote-url" title=\{summary\.remote\}[^\n]*textOverflow: "ellipsis"[^\n]*>\{summary\.remote\}<\/div>\s*<div style=\{\{ flexShrink: 0 \}\}><ActionButton label=\{t\("git\.editRemote"\)\}/);
  assert.match(sidebar, /worktreeState\.forCwd === selectedCwd \? worktreeState\.branchSync : null/);
  const selector = sidebar.match(/<button\b[^>]*\baria-expanded=\{wtDropdownOpen\}[\s\S]*?<\/button>/)?.[0];
  assert.ok(selector);
  assert.match(selector, /branchSync &&[\s\S]*?branchSync\.ahead[\s\S]*?branchSync\.behind/);
  assert.match(sidebar, /\[selectedCwd, wtRefreshKey, refreshKey, gitWorkspaceRefreshKey\]/);
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
