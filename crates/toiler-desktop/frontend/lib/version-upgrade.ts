/** 桌面升级请求必须带回预览快照，防止确认时版本或提交范围已经变化。 */
export const VERSION_UPGRADE_LEVELS = ["patch", "minor", "major"] as const;
export type VersionUpgradeLevel = typeof VERSION_UPGRADE_LEVELS[number];

export interface VersionUpgradePreview {
  current: string;
  version: string;
  tag: string;
  head: string;
  planId: string;
  files: string[];
  output: string;
  commit?: string;
}

export function versionUpgradeRequest(cwd: string, level: VersionUpgradeLevel, preview?: VersionUpgradePreview) {
  return {
    cwd,
    level,
    dryRun: !preview,
    ...(preview ? { expectedHead: preview.head, expectedVersion: preview.version, expectedPlan: preview.planId } : {}),
  };
}

export async function upgradeProjectVersion(cwd: string, level: VersionUpgradeLevel, preview?: VersionUpgradePreview): Promise<VersionUpgradePreview> {
  // 不能回退到 HTTP 或执行项目自带脚本；版本升级只在桌面窗口提供。
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<VersionUpgradePreview>("upgrade_project_version", { request: versionUpgradeRequest(cwd, level, preview) });
}
