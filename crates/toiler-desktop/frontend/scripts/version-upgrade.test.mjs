import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { VERSION_UPGRADE_LEVELS, versionUpgradeRequest } = await jiti.import("../lib/version-upgrade.ts");

// 请求体里不再带 cwd：它走查询串（`?cwd=`），body 只放升级参数。
// 这样 Rust 侧可以用 `Query<UpgradeParams>` 取路径、`Json<UpgradeRequest>` 取参数，
// 两者各自校验，不会混在一起。
for (const level of ["patch", "minor", "major"]) {
  test(`版本升级 ${level} 先请求只读预览`, () => {
    assert.deepEqual(versionUpgradeRequest(level), { level, dryRun: true });
  });
}

test("确认请求携带原始 HEAD、目标版本及规划快照，不能直接无确认发布", () => {
  const preview = { head: "commit", version: "1.2.3", planId: "snapshot", tag: "v1.2.3" };
  assert.deepEqual(versionUpgradeRequest("patch", preview), {
    level: "patch", dryRun: false,
    expectedHead: "commit", expectedVersion: "1.2.3", expectedPlan: "snapshot",
  });
  assert.deepEqual(VERSION_UPGRADE_LEVELS, ["patch", "minor", "major"]);
});

test("升级入口紧随创建标签，且只走 Rust 服务接口，不依赖 Tauri invoke 或 Python", async () => {
  const panel = await readFile(new URL("../components/GitPanel.tsx", import.meta.url), "utf8");
  const bridge = await readFile(new URL("../lib/version-upgrade.ts", import.meta.url), "utf8");

  // 三个按钮紧跟「创建标签」，且只在桌面壳里可用。
  assert.match(panel, /label=\{t\("git\.createTag"\)\}[\s\S]*?VERSION_UPGRADE_LEVELS\.map/);
  assert.match(panel, /disabled=\{!desktop \|\| Boolean\(summary\.operation\)/);
  assert.match(panel, /git\.versionUpgradeConfirm/);

  // 关键回归：走 HTTP 到 Rust，而不是 Tauri invoke（客户端机器可能没有 Python，
  // 旧实现靠 invoke 跑内嵌 py 脚本）。
  assert.match(bridge, /fetch\(`\/api\/version-upgrade\?cwd=/);
  assert.doesNotMatch(bridge, /invoke<VersionUpgradePreview>/);

  // cwd 放在查询串里，body 不能再重复一份。
  assert.doesNotMatch(bridge, /body: JSON\.stringify\(\{[^}]*cwd/);
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
