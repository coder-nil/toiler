import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { subscribeToWindowForeground } = await jiti.import("../lib/window-foreground.ts");
const { getGitWorkspaceSplitBounds } = await jiti.import("../lib/panel-layout.ts");

const read = (path) => readFile(new URL(`../${path}`, import.meta.url), "utf8");
const sidebar = await read("components/SessionSidebar.tsx");
const shell = await read("components/AppShell.tsx");
const panel = await read("components/GitPanel.tsx");
const workspace = await read("components/GitWorkspace.tsx");
const history = await read("components/GitHistoryPanel.tsx");
const conflictEditor = await read("components/GitConflictEditor.tsx");
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

test("进入 Git 工作区自动收起侧栏，返回聊天时重新展开", () => {
  assert.match(shell, /const toggleGitWorkspace = useCallback[\s\S]*?setGitWorkspace\(\{ cwd, contextCwd \}\);\s*setSidebarOpen\(false\);/);
  assert.match(shell, /const closeGitWorkspace = useCallback\(\(\) => guardGitDraft\(\(\) => \{\s*setGitWorkspace\(null\);\s*setSidebarOpen\(true\);/);
});

test("当前更改与历史文件都送到共用差异区，保留各自的比较基准", () => {
  assert.match(panel, /onSelectFile\(\{ scope: "unstaged", path: file\.filePath \}\)/);
  assert.match(panel, /onSelectFile\(\{ scope: "staged", path: file\.filePath \}\)/);
  assert.match(history, /onSelectFile\(\{ scope: "commit", hash: commit\.hash, path: file\.path, oldPath: file\.oldPath \}\)/);
  assert.match(workspace, /<WorkspaceDiff cwd=\{cwd\} selection=\{selection\}/);
  assert.doesNotMatch(history, /showModal\(|<dialog\b/);
});

test("冲突编辑器位于文件对比区域，浏览其他差异时不卸载草稿", () => {
  assert.match(workspace, /id=\{diffId\}[\s\S]*?ref=\{setConflictTarget\} hidden=\{!showingConflict\}/);
  assert.match(panel, /conflictTarget && conflict\?\.cwd === cwd && createPortal\([\s\S]*?<GitConflictEditor[\s\S]*?conflictTarget,/);
  assert.doesNotMatch(conflictEditor, /showModal\(|<dialog\b|document\.body/);
  assert.match(shell, /conflictEditorRef\.current\.requestLeave\(leave\)/);
});

test("右侧上下分隔条在短窗口中仍为两个区域保留空间", () => {
  assert.deepEqual(getGitWorkspaceSplitBounds(680), { min: 230, max: 540, initial: 356 });
  assert.deepEqual(getGitWorkspaceSplitBounds(360), { min: 183, max: 233, initial: 187 });
  assert.deepEqual(getGitWorkspaceSplitBounds(0), { min: 0, max: 0, initial: 0 });
  assert.match(workspace, /growthDirection: "down"/);
  assert.match(workspace, /sizing\.separatorProps[\s\S]*?aria-controls=\{diffId\}/);
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

test("远程在 Git 操作区顶部，提交与提交并推送相邻，不再显示独立合并区", () => {
  const headings = [...panel.matchAll(/<SectionTitle title=\{t\("(git\.\w+)"/g)].map((match) => match[1]);
  assert.deepEqual(headings, ["git.remote", "git.changesSummary", "git.stagedSummary", "git.commit", "git.tags"]);
  assert.doesNotMatch(panel, /MergeBranchPicker|git\.mergeBranch/);
  assert.match(panel, /label=\{t\("git\.commit"\)\} action="commit"[^\n]*run\("commit", \{ message \}\)[^\n]*\n\s*<ActionButton label=\{t\("git\.commitAndPush"\)\} action="commit_and_push"[^\n]*run\("commit_and_push", \{ message \}\)/);
});

test("提交并推送先完成本地提交，普通推送和组合操作都只在确认后发起推送", () => {
  assert.match(panel, /const firstAction = action === "commit_and_push" \? "commit" : action;[\s\S]*?await request\(firstAction, extra\);[\s\S]*?setPushPrompt\(\{ cwd, committed: true \}\)/);
  assert.match(panel, /action="push" busy=\{busy\} onClick=\{\(\) => setPushPrompt\(\{ cwd, committed: false \}\)\}/);
  assert.equal([...panel.matchAll(/run\("push"/g)].length, 1);
  assert.match(panel, /run\("push", \{ previewToken: preview\.token \}, preview\.usesPanelCredential \? undefined : null\)/);
  assert.match(panel, /onCancel=\{\(\) => setPushPrompt\(null\)\} onPush=\{confirmPush\}/);
});

test("推送预览的分支只读，提交选择驱动右侧文件和详情", async () => {
  const dialog = await read("components/GitPushDialog.tsx");
  assert.doesNotMatch(dialog, /<select\b|BranchPicker/);
  assert.match(dialog, /preview\.remote\}\/\{preview\.targetBranch/);
  assert.match(dialog, /onClick=\{\(\) => setHash\(item\.hash\)\}/);
  assert.match(dialog, /load<GitHistoryCommit>\("commit", cwd, \{ hash \}/);
  assert.match(dialog, /commit\.files\.map/);
  assert.match(dialog, /commit\.message/);
  assert.match(dialog, /await onPush\(preview\)/);
});

test("Git 操作区的拉取入口统一在没有已存 HTTPS 或 SSH 凭据时弹出确认框", () => {
  assert.equal([...panel.matchAll(/action="pull" busy=\{busy\} onClick=\{requestPull\}/g)].length, 2);
  assert.match(panel, /if \(!hasSavedCredential && \(kind === "https" \|\| kind === "ssh"\)\) \{\s*setPullPrompt\(\{ cwd, remote, kind, rebase: pullRebase \}\);\s*return;\s*\}\s*pull\(pullRebase\);/);
});

test("取消凭据确认不拉取，确认保留原拉取模式，留空不会发送凭据", () => {
  assert.match(panel, /pullPrompt\.remote === summary\.remote && pullPrompt\.kind === summary\.credentialKind/);
  assert.match(panel, /onCancel=\{\(\) => setPullPrompt\(null\)\}/);
  assert.match(panel, /onConfirm=\{\(draft\) => \{ setPullPrompt\(null\); pull\(pullPrompt\.rebase, draft\); \}\}/);
  assert.match(panel, /run\("pull", \{ rebase \}, draft\)/);
  assert.match(panel, /isRemoteAction\(gitAction\) && draft\?\.secret && draft\.secret !== SAVED_CREDENTIAL_MASK/);
  assert.match(panel, /\.\.\.\(credential && draft \? \{ credential, rememberCredential: draft\.rememberCredential \} : \{\}\)/);
  assert.match(panel, /onCancel=\{\(event\) => \{ event\.preventDefault\(\); onCancel\(\); \}\}/);
});

test("侧栏和 Git 历史按完整引用删除分支，不连带移除工作树", async () => {
  const actions = await read("components/GitRefActions.tsx");
  assert.match(sidebar, /sidebar\.deleteLocalBranch[^\n]*[\s\S]*?reference: `refs\/heads\/\$\{branch\}`/);
  assert.match(sidebar, /sidebar\.deleteRemoteBranch[^\n]*[\s\S]*?reference: `refs\/remotes\/\$\{branch\}`/);
  const handler = sidebar.split("const handleDeleteBranch =")[1]?.split("const handleRemoveWorktree =")[0] ?? "";
  assert.match(handler, /requestGitRefAction\(target\.cwd, target\.reference,/);
  assert.doesNotMatch(handler, /\/api\/worktrees|delete_remote_branch|delete_branch/);
  assert.match(actions, /const canForceDelete = isDelete && kind === "local";/);
  assert.match(actions, /const \[forceDelete, setForceDelete\] = useState\(false\)/);
  assert.match(actions, /canForceDelete && forceDelete \? \{ action: "delete", force: true \} : \{ action: "delete" \}/);
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
