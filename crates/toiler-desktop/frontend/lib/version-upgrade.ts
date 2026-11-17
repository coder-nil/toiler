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

/**
 * 组装升级请求体。
 *
 * `cwd` **不在** body 里：它走查询串（`?cwd=…`），Rust 侧用 `Query<UpgradeParams>`
 * 单独校验路径。两者分开后，参数校验失败与路径校验失败能给出不同提示。
 */
export function versionUpgradeRequest(level: VersionUpgradeLevel, preview?: VersionUpgradePreview) {
  return {
    level,
    dryRun: !preview,
    ...(preview ? { expectedHead: preview.head, expectedVersion: preview.version, expectedPlan: preview.planId } : {}),
  };
}

/**
 * 触发版本升级（`POST /api/version-upgrade`）。
 *
 * 走 Rust 服务而不是 Tauri invoke：升级逻辑已经完全在 Rust 侧实现，
 * 客户端机器不再需要 Python。服务端会再校验请求的 `Origin` 必须等于桌面壳的
 * origin，所以手机配对页与 Cloudflare 隧道拿到的都是 403 —— 这个能力只在
 * 桌面窗口内可用（见 `routes/version_upgrade.rs` 的头注释）。
 */
export async function upgradeProjectVersion(cwd: string, level: VersionUpgradeLevel, preview?: VersionUpgradePreview): Promise<VersionUpgradePreview> {
  const response = await fetch(`/api/version-upgrade?cwd=${encodeURIComponent(cwd)}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(versionUpgradeRequest(level, preview)),
  });
  const data = await response.json().catch(() => null) as (VersionUpgradePreview & { error?: string }) | null;
  if (!response.ok) {
    throw new Error(data?.error ?? `版本升级失败（HTTP ${response.status}）`);
  }
  if (!data) throw new Error("版本升级返回为空");
  return data;
}
