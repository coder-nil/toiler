/**
 * 标签名输入框的示例值：仓库里已有标签的**下一个**版本号。
 *
 * 示例值不能写死。`create_tag` 成功后面板会清空输入框，用户下次建标签时看到的
 * 提示应当随仓库已有的标签往前走（`v0.1.3` → `v0.1.4`）；写死 `v1.2.0` 时，
 * 它只会是一句与本仓库版本无关的广告，还容易被照着创建成错误的标签。
 *
 * 递增规则与 pi 的 `version:next` 一致（见 `data/changelog.json` 里
 * `0.85.1-alpha.13` 那条）：
 *
 * | 现有标签 | 下一个 |
 * |---|---|
 * | `v1.2.3` | `v1.2.4` |
 * | `v0.85.1-alpha.13` | `v0.85.1-alpha.14` |
 * | `v1.2.3-beta` | `v1.2.3-beta.1` |
 *
 * 只认 `[v]MAJOR.MINOR.PATCH[-PRERELEASE]` 形状：分支名、日期标签（`2026-11-13`）
 * 之类无法可靠递增，跳过它们而不是猜。
 */

interface ParsedTag {
  /** 前导 `v`，没有时为空串；递增后原样保留，不擅自加或去。 */
  prefix: string;
  major: number;
  minor: number;
  patch: number;
  /** 预发布标识，不含前导 `-`；没有时为空串。 */
  prerelease: string;
}

const TAG_PATTERN = /^(v?)(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$/;

/**
 * 应用版本（`package.json` 的 `version`）也解析不出来时用的示例值。
 *
 * 只有在版本注入失败（`NEXT_PUBLIC_PACKAGE_VERSION` 缺失，取值 `0.0.0`）时才会
 * 走到这里：此时没有可信的基数，给一个中性示例好过显示 `v0.0.1`。
 */
const FALLBACK_EXAMPLE = "v1.0.0";

function parseTag(tag: string): ParsedTag | null {
  const match = TAG_PATTERN.exec(tag.trim());
  if (!match) return null;
  return {
    prefix: match[1],
    major: Number(match[2]),
    minor: Number(match[3]),
    patch: Number(match[4]),
    prerelease: match[5] ?? "",
  };
}

/** 数字段逐位比较，用于找出「最大的那个标签」。 */
function compareParsed(a: ParsedTag, b: ParsedTag): number {
  if (a.major !== b.major) return a.major - b.major;
  if (a.minor !== b.minor) return a.minor - b.minor;
  if (a.patch !== b.patch) return a.patch - b.patch;
  // 有预发布标识的低于同版本号的正式标签（semver 的 precedence 规则）。
  if (a.prerelease === b.prerelease) return 0;
  if (!a.prerelease) return 1;
  if (!b.prerelease) return -1;
  return a.prerelease.localeCompare(b.prerelease, "en", { numeric: true });
}

function bumpParsed(tag: ParsedTag): string {
  const { prefix, major, minor, patch, prerelease } = tag;
  if (!prerelease) return `${prefix}${major}.${minor}.${patch + 1}`;
  // 预发布只推进最后一个数字段（`alpha.13` → `alpha.14`）；没有数字段时补一个
  // （`beta` → `beta.1`），与 pi 的做法相同。
  const parts = prerelease.split(".");
  const last = parts.length - 1;
  if (/^\d+$/.test(parts[last])) {
    parts[last] = String(Number(parts[last]) + 1);
    return `${prefix}${major}.${minor}.${patch}-${parts.join(".")}`;
  }
  return `${prefix}${major}.${minor}.${patch}-${prerelease}.1`;
}

/**
 * 推导标签名输入框的示例值。
 * @param tags 当前仓库的本地标签（`GET /api/git` 的 `tags`）
 * @param appVersion 应用版本，用作仓库里没有可递增标签时的基数
 * @returns 建议使用的下一个标签名，例如 `v0.1.4`
 */
export function nextTagName(tags: readonly string[], appVersion: string): string {
  const parsed = tags.map(parseTag).filter((tag): tag is ParsedTag => tag !== null);
  if (parsed.length > 0) {
    return bumpParsed(parsed.reduce((max, tag) => (compareParsed(tag, max) > 0 ? tag : max)));
  }
  // 仓库里没有可递增的标签：拿应用版本起步，并补上与发布工作流一致的前缀——
  // CI 只在推送 `v*` tag 时才发布，所以这里给带 `v` 的形状。
  const base = parseTag(appVersion);
  if (!base) return FALLBACK_EXAMPLE;
  return bumpParsed({ ...base, prefix: base.prefix || "v" });
}
