import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import test from "node:test";

const root = fileURLToPath(new URL("../", import.meta.url));

const read = (relative) => readFile(join(root, relative), "utf8");

/** 缺少 Git 时，文件浏览器仍要保留安装提示的入口。 */
test("没装 Git 时文件浏览器仍有 Git 入口", async () => {
  const sidebar = await read("components/SessionSidebar.tsx");
  assert.match(sidebar, /const showExplorerGitButton = gitPanelOpen \|\| explorerGit\?\.isGit === true \|\| explorerGit\?\.gitAvailable === false/);
  assert.match(sidebar, /showExplorerGitButton && \(\s*<ToolbarIconButton\s+onClick=\{toggleGitPanel\}/);
});

test("Git 面板对两种「不是仓库」给出不同提示", async () => {
  const panel = await read("components/GitPanel.tsx");
  assert.match(panel, /gitAvailable\?: boolean/, "GitSummary 需要 gitAvailable 字段");
  assert.match(panel, /git\.missingTitle/, "缺 git 时要显示安装标题");
  assert.match(panel, /git\.missingBody/, "缺 git 时要显示安装说明");
  // 只有在没装 git 的分支之外，才显示「此目录不是 Git 仓库」
  assert.match(
    panel,
    /summary\?\.gitAvailable !== false && summary && !summary\.isGitRepository/,
    "「不是仓库」的提示不能盖住「缺 git」的提示"
  );
});

test("侧边栏读取 gitAvailable，缺省兼容旧响应", async () => {
  const sidebar = await read("components/SessionSidebar.tsx");
  assert.match(sidebar, /gitAvailable: d\.gitAvailable \?\? true/);
});

test("中英文都提供了安装指引文案", async () => {
  for (const [file, label] of [
    ["lib/i18n/messages/zh-CN.ts", "中文"],
    ["lib/i18n/messages/en.ts", "英文"],
  ]) {
    const source = await read(file);
    for (const key of [
      "git.missingTitle",
      "git.missingBody",
      "git.missingWindowsHint",
      "git.missingMacHint",
      "git.missingLinuxHint",
    ]) {
      assert.match(
        source,
        new RegExp(`"${key.replace(/\./g, "\\.")}"`),
        `${label}缺少文案 ${key}`
      );
    }
  }
});
