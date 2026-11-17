import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { VERSION_UPGRADE_LEVELS, versionUpgradeRequest } = await jiti.import("../lib/version-upgrade.ts");

for (const level of ["patch", "minor", "major"]) {
  test(`版本升级 ${level} 先请求只读预览`, () => {
    assert.deepEqual(versionUpgradeRequest("/project", level), { cwd: "/project", level, dryRun: true });
  });
}

test("确认请求携带原始 HEAD、目标版本及规划快照，不能直接无确认发布", () => {
  const preview = { head: "commit", version: "1.2.3", planId: "snapshot", tag: "v1.2.3" };
  assert.deepEqual(versionUpgradeRequest("/project", "patch", preview), {
    cwd: "/project", level: "patch", dryRun: false,
    expectedHead: "commit", expectedVersion: "1.2.3", expectedPlan: "snapshot",
  });
  assert.deepEqual(VERSION_UPGRADE_LEVELS, ["patch", "minor", "major"]);
});

test("升级入口紧随创建标签，且仅使用桌面命令而不向 HTTP 暴露脚本执行", async () => {
  const panel = await readFile(new URL("../components/GitPanel.tsx", import.meta.url), "utf8");
  const bridge = await readFile(new URL("../lib/version-upgrade.ts", import.meta.url), "utf8");
  assert.match(panel, /label=\{t\("git.createTag"\)\}[\s\S]*?VERSION_UPGRADE_LEVELS\.map/);
  assert.match(panel, /disabled=\{!desktop \|\| Boolean\(summary.operation\)/);
  assert.match(panel, /git\.versionUpgradeConfirm/);
  assert.match(bridge, /invoke<VersionUpgradePreview>\("upgrade_project_version"/);
  assert.doesNotMatch(bridge, /fetch\(/);
});

test("升级文案在中英两种语言中均齐全", async () => {
  for (const locale of ["zh-CN", "en"]) {
    const source = await readFile(new URL(`../lib/i18n/messages/${locale}.ts`, import.meta.url), "utf8");
    for (const key of ["patch", "minor", "major"]) {
      assert.ok(source.includes(`"git.versionUpgrade.${key}"`));
    }
    for (const key of ["Confirm", "Scope", "Apply", "Done", "DesktopOnly", "Hint"]) {
      assert.ok(source.includes(`"git.versionUpgrade${key}"`));
    }
  }
});
