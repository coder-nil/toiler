import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

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

test("Git 打开后复用文件浏览器标题栏，刷新紧邻开关，不再显示关闭按钮", () => {
  assert.ok(explorer);
  assert.match(explorer, /gitPanelOpen \? \(\s*<h2[^>]*gitHeading[^>]*>\s*<GitBranch[^>]*\/>\s*<span>\{t\("common\.git"\)\}<\/span>/);
  assert.match(explorer, /gitRefreshSlot\} ref=\{setGitRefreshTarget\} \/>\}\s*\{showExplorerGitButton &&/);
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
});
