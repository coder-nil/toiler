import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import test from "node:test";

const root = fileURLToPath(new URL("../", import.meta.url));

const read = (relative) => readFile(join(root, relative), "utf8");

/**
 * 这些断言守的是「没装 Git 时用户能不能看到安装提示」这条链路。
 *
 * 它很容易被无意破坏：底部 Git 按钮原本只在 `activeProjectIsGit === true`
 * 时出现，而没装 Git 时那个值必然是 false —— 于是按钮消失、面板打不开、
 * `git.missingTitle` 那段文案永远不会渲染，整个提示功能等于没做。
 * 同理，那个「不是仓库就自动关面板」的 effect 也会把提示面板立刻关掉。
 */
test("没装 Git 时底部 Git 入口仍然出现", async () => {
  const shell = await read("components/AppShell.tsx");
  const line = shell
    .split("\n")
    .find((text) => text.includes('id !== "git"'));
  assert.ok(line, "找不到底部工具条的 git 过滤条件");
  assert.match(
    line,
    /gitAvailable === false/,
    "入口条件必须包含「没装 git」，否则用户看不到安装提示"
  );
});

test("没装 Git 时 Git 面板不会被自动关闭", async () => {
  const shell = await read("components/AppShell.tsx");
  assert.match(
    shell,
    /activeProjectIsGit === false && gitPanelCwd === null && gitAvailable !== false/,
    "自动关闭面板的条件必须排除「没装 git」的情况"
  );
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

test("侧边栏把 gitAvailable 一起上报", async () => {
  const sidebar = await read("components/SessionSidebar.tsx");
  assert.match(sidebar, /gitAvailable: boolean/, "worktreeState 需要带 gitAvailable");
  assert.match(
    sidebar,
    /onProjectGitStateChange\?\.\([\s\S]*?gitAvailable/,
    "上报时必须带上 gitAvailable，否则 AppShell 无从判断"
  );
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
