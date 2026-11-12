/**
 * `data/changelog.json` 的形状。由 `scripts/sync-changelog.mjs` 从 `CHANGELOG.md`
 * 生成，界面通过 `lib/changelog.ts` 读取——手写这个 JSON 不会生效。
 */
export type ChangelogSectionKind =
  | "added"
  | "changed"
  | "fixed"
  | "removed"
  | "deprecated"
  | "security"
  | "other";

export type ChangelogSection = {
  /** `CHANGELOG.md` 里的原始小节标题，例如 `Added`。 */
  title: string;
  kind: ChangelogSectionKind;
  items: string[];
};

export type ChangelogRelease = {
  version: string;
  date: string;
  sections: ChangelogSection[];
};
