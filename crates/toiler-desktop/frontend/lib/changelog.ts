import type { ChangelogRelease } from "@/data/changelog-types";
import changelog from "@/data/changelog.json";

export type { ChangelogRelease };

/**
 * 内置的版本变更记录。数据来自仓库根目录的 `CHANGELOG.md`，由
 * `npm run changelog:sync` 生成 `data/changelog.json` 并打进包内，
 * 因此读取是纯本地的，离线也能看到完整历史。
 */
export const CHANGELOG: ChangelogRelease[] = changelog as ChangelogRelease[];

export const APPLICATION_VERSION = process.env.NEXT_PUBLIC_PACKAGE_VERSION ?? "0.0.0";
export const PI_VERSION = process.env.NEXT_PUBLIC_PI_VERSION ?? "0.0.0";

export function currentRelease(): ChangelogRelease | undefined {
  return CHANGELOG.find((release) => release.version === APPLICATION_VERSION) ?? CHANGELOG[0];
}

/** 供「复制全部」使用：把内置数据还原成 Markdown。 */
export function changelogToMarkdown(releases: ChangelogRelease[] = CHANGELOG): string {
  return releases
    .map((release) => {
      const heading = release.date
        ? `## [${release.version}] - ${release.date}`
        : `## [${release.version}]`;
      const body = release.sections
        .map((section) => `### ${section.title}\n\n${section.items.map((item) => `- ${item}`).join("\n")}`)
        .join("\n\n");
      return body ? `${heading}\n\n${body}` : heading;
    })
    .join("\n\n");
}
