import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const bundledCatalog = require("../data/plugin-catalog.json") as unknown;

export interface PluginCatalogEntry {
  name: string;
  source: string;
  description: string;
  descriptionLanguage: string;
  descriptionZh?: string;
  descriptionZhSource?: "curated" | "machine";
  descriptionEn?: string;
  descriptionEnSource?: "machine";
  version?: string;
  updatedAt?: string;
  publisher?: string;
  /** npm 最近 30 天下载量，代表官网（npm registry）上的使用次数。 */
  monthlyDownloads?: number;
  weeklyDownloads?: number;
  npmUrl: string;
  repositoryUrl?: string;
  homepageUrl?: string;
}

interface RegistryPackage {
  name?: unknown;
  description?: unknown;
  version?: unknown;
  date?: unknown;
  keywords?: unknown;
  author?: unknown;
  links?: unknown;
}

interface RegistryObject {
  package?: RegistryPackage;
  downloads?: { monthly?: unknown; weekly?: unknown };
  score?: { final?: unknown };
}

export interface RegistrySearchResponse {
  objects?: unknown;
  total?: unknown;
  time?: unknown;
}

const CURATED_DESCRIPTIONS: Record<string, string> = {
  "pi-mcp-adapter": "连接标准 MCP 服务器，并通过按需发现把工具接入 Pi。支持本地命令、HTTP、OAuth 及常见 MCP 配置文件。",
  "pi-mcp-extension": "MCP 客户端扩展，让 Pi 可以连接并调用任意 MCP 服务器。",
  "@pi-unipi/mcp": "MCP 服务器管理扩展，可浏览、添加、配置、启用和使用 MCP 服务器。",
  "@amaster.ai/pi-browser-use": "通过 Chrome DevTools MCP 为 Pi 提供浏览器自动化工具。",
  "@spences10/pi-mcp": "为 Pi 提供 MCP 服务器集成，并控制大型工具结果带来的上下文开销。",
  "@dreki-gg/pi-mcp": "面向 Pi 的 MCP 客户端扩展。",
};

function cleanString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function countValue(value: unknown): number | undefined {
  const parsed = typeof value === "number" ? value : Number.parseInt(String(value ?? ""), 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : undefined;
}

function publisherName(value: unknown): string | undefined {
  if (typeof value === "string") return cleanString(value);
  if (value && typeof value === "object" && "name" in value) {
    return cleanString((value as { name?: unknown }).name);
  }
  return undefined;
}

function linkValue(value: unknown, key: string): string | undefined {
  if (!value || typeof value !== "object") return undefined;
  return cleanString((value as Record<string, unknown>)[key]);
}

function isPiPackage(pkg: RegistryPackage): boolean {
  const keywords = Array.isArray(pkg.keywords)
    ? pkg.keywords.filter((value): value is string => typeof value === "string").map((value) => value.toLowerCase())
    : [];
  return keywords.includes("pi-package") || keywords.includes("pi-extension");
}

export function parsePluginCatalog(payload: RegistrySearchResponse): PluginCatalogEntry[] {
  if (!Array.isArray(payload.objects)) return [];
  return payload.objects.flatMap((item) => {
    if (!item || typeof item !== "object") return [];
    const raw = item as RegistryObject;
    const pkg = raw.package;
    const name = cleanString(pkg?.name);
    if (!pkg || !name || !isPiPackage(pkg)) return [];
    const description = cleanString(pkg.description) ?? "Pi extension package";
    const links = pkg.links;
    const npmUrl = linkValue(links, "npm") ?? `https://www.npmjs.com/package/${encodeURIComponent(name)}`;
    return [{
      name,
      source: `npm:${name}`,
      description,
      descriptionLanguage: detectDescriptionLanguage(description),
      descriptionZh: CURATED_DESCRIPTIONS[name],
      descriptionZhSource: CURATED_DESCRIPTIONS[name] ? "curated" : undefined,
      version: cleanString(pkg.version),
      updatedAt: cleanString(pkg.date),
      publisher: publisherName(pkg.author),
      monthlyDownloads: countValue(raw.downloads?.monthly),
      weeklyDownloads: countValue(raw.downloads?.weekly),
      npmUrl,
      repositoryUrl: linkValue(links, "repository"),
      homepageUrl: linkValue(links, "homepage"),
    } satisfies PluginCatalogEntry];
  });
}

interface TranslationCacheEntry {
  value: string;
  expiresAt: number;
}

const TRANSLATION_TTL_MS = 24 * 60 * 60 * 1000;
const TRANSLATION_TIMEOUT_MS = 10_000;
const TRANSLATION_ENDPOINT = "https://api.mymemory.translated.net/get";

declare global {
  var __piPluginTranslationCache: Map<string, TranslationCacheEntry> | undefined;
}

function getTranslationCache(): Map<string, TranslationCacheEntry> {
  return globalThis.__piPluginTranslationCache ??= new Map();
}

/** MyMemory requires a source language, so use conservative script/word detection. */
export function detectDescriptionLanguage(text: string): string {
  if (/[\u3400-\u9fff]/.test(text)) return "zh-CN";
  if (/[іїєґІЇЄҐ]/.test(text) || /(еталонних|правил|допомогою|застосунок)/i.test(text)) return "uk";
  if (/[ыэъёЫЭЪЁ]/.test(text)) return "ru";
  if (/[\u0370-\u03ff]/.test(text)) return "el";
  if (/[\u0600-\u06ff]/.test(text)) return "ar";
  if (/[\u0590-\u05ff]/.test(text)) return "he";
  return "en";
}

async function translateDescription(text: string, targetLanguage: "en" | "zh-CN"): Promise<string | undefined> {
  const normalized = text.trim();
  if (!normalized || normalized.length > 500) return undefined;
  const sourceLanguage = detectDescriptionLanguage(normalized);
  if (sourceLanguage === targetLanguage) return undefined;
  const cache = getTranslationCache();
  const cacheKey = `${sourceLanguage}\0${targetLanguage}\0${normalized}`;
  const cached = cache.get(cacheKey);
  if (cached && cached.expiresAt > Date.now()) return cached.value;

  try {
    const url = new URL(TRANSLATION_ENDPOINT);
    url.searchParams.set("q", normalized);
    url.searchParams.set("langpair", `${sourceLanguage}|${targetLanguage}`);
    const response = await fetch(url, {
      cache: "no-store",
      headers: { Accept: "application/json" },
      signal: AbortSignal.timeout(TRANSLATION_TIMEOUT_MS),
    });
    if (!response.ok) return undefined;
    const payload = await response.json() as { responseData?: { translatedText?: unknown } };
    const translated = cleanString(payload.responseData?.translatedText);
    if (!translated || translated === normalized) return undefined;
    cache.set(cacheKey, { value: translated, expiresAt: Date.now() + TRANSLATION_TTL_MS });
    return translated;
  } catch {
    return undefined;
  }
}

/** Fill missing Chinese summaries without replacing the curated descriptions. */
export async function translatePluginDescriptions(
  entries: PluginCatalogEntry[],
  targetLanguage: "en" | "zh-CN" = "zh-CN",
): Promise<PluginCatalogEntry[]> {
  let cursor = 0;
  const workers = Array.from({ length: Math.min(8, entries.length) }, async () => {
    while (cursor < entries.length) {
      const index = cursor++;
      const entry = entries[index];
      if (entry.descriptionLanguage === targetLanguage) continue;
      if (targetLanguage === "zh-CN" && entry.descriptionZh) continue;
      if (targetLanguage === "en" && entry.descriptionEn) continue;
      const translated = await translateDescription(entry.description, targetLanguage);
      if (translated) {
        entries[index] = targetLanguage === "zh-CN"
          ? { ...entry, descriptionZh: translated, descriptionZhSource: "machine" }
          : { ...entry, descriptionEn: translated, descriptionEnSource: "machine" };
      }
    }
  });
  await Promise.all(workers);
  return entries;
}

/**
 * 远程目录源：pi.dev 的官方插件目录，与 npm registry 搜索互为补充。
 *
 * 两者不能混用：npm 搜索的 `popularity` 打分和 pi.dev 的「Most downloads」排序
 * 不是同一个东西（搜 pi-package 时排第一的 pi-web-access 下载量反而低于
 * pi-mcp-adapter），所以官网可见时优先用官网列表。
 */export function parsePiDevCatalog(html: string): PluginCatalogEntry[] {
  const entries: PluginCatalogEntry[] = [];
  const seen = new Set<string>();
  // 卡片上行内属性顺序固定；用 [^>]*? 容忍将来插入其他 data-* 属性。
  const cardPattern = /data-package-name="([^"]+)"[^>]*?data-package-downloads="([^"]*)"/g;
  let match: RegExpExecArray | null;
  while ((match = cardPattern.exec(html))) {
    const name = match[1].trim();
    if (!name || seen.has(name)) continue;
    seen.add(name);
    const downloads = Number.parseInt(match[2], 10);
    entries.push({
      name,
      source: `npm:${name}`,
      description: "",
      descriptionLanguage: "en",
      npmUrl: `https://www.npmjs.com/package/${encodeURIComponent(name)}`,
      monthlyDownloads: Number.isFinite(downloads) && downloads > 0 ? downloads : undefined,
    });
  }
  return entries;
}

/** pi.dev 只给名字和使用量，用 npm 的 package 元数据把其余字段补齐。 */
export function mergeCatalogEntries(
  primary: readonly PluginCatalogEntry[],
  fallback: readonly PluginCatalogEntry[],
): PluginCatalogEntry[] {
  const byName = new Map(fallback.map((entry) => [entry.name, entry]));
  return primary.map((entry) => {
    const extra = byName.get(entry.name);
    if (!extra) return entry;
    return {
      ...extra,
      ...entry,
      name: entry.name,
      // 排序依据必须是主列表（官网）的值，不能被元数据里的旧值覆盖。
      monthlyDownloads: entry.monthlyDownloads ?? extra.monthlyDownloads,
      description: entry.description || extra.description,
      descriptionLanguage: entry.description
        ? entry.descriptionLanguage
        : extra.descriptionLanguage,
      descriptionZh: extra.descriptionZh,
      descriptionZhSource: extra.descriptionZhSource,
      version: entry.version ?? extra.version,
      updatedAt: extra.updatedAt,
      publisher: extra.publisher,
      repositoryUrl: extra.repositoryUrl,
      homepageUrl: extra.homepageUrl,
      npmUrl: entry.npmUrl || extra.npmUrl,
    };
  });
}

/**
 * 按官网（npm）使用量倒序排列：下载量高的插件优先。
 * 无下载数据的条目排在已知下载量的条目之后，并退回到内置精选顺序 + 名称。
 */
export function sortPluginCatalog(entries: PluginCatalogEntry[]): PluginCatalogEntry[] {
  const featured = Object.keys(CURATED_DESCRIPTIONS);
  const downloadsOf = (entry: PluginCatalogEntry): number | null => {
    const value = entry.monthlyDownloads ?? entry.weeklyDownloads;
    return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : null;
  };
  const featuredRank = (entry: PluginCatalogEntry) => {
    const rank = featured.indexOf(entry.name);
    return rank === -1 ? featured.length : rank;
  };
  return [...entries].sort((a, b) => {
    const aDownloads = downloadsOf(a);
    const bDownloads = downloadsOf(b);
    if (aDownloads !== null || bDownloads !== null) {
      if (aDownloads === null) return 1;
      if (bDownloads === null) return -1;
      if (aDownloads !== bDownloads) return bDownloads - aDownloads;
    }
    const rankDelta = featuredRank(a) - featuredRank(b);
    if (rankDelta !== 0) return rankDelta;
    return a.name.localeCompare(b.name);
  });
}

export function getBundledPluginCatalog(): PluginCatalogEntry[] {
  return sortPluginCatalog(bundledCatalog as PluginCatalogEntry[]);
}

/**
 * 用持久化的下载量补齐目录条目（内置目录的下载量是快照，可能过时）。
 * 只为已知的包名覆盖，不用缓存里的陌生条目扩充目录。
 */
export function applyPluginDownloads(
  entries: PluginCatalogEntry[],
  counts: { monthly: Record<string, number>; weekly?: Record<string, number> } | null | undefined,
): PluginCatalogEntry[] {
  if (!counts || Object.keys(counts.monthly).length === 0) return entries;
  return entries.map((entry) => {
    const monthly = counts.monthly[entry.name];
    const weekly = counts.weekly?.[entry.name];
    if (monthly === undefined && weekly === undefined) return entry;
    return {
      ...entry,
      monthlyDownloads: monthly ?? entry.monthlyDownloads,
      weeklyDownloads: weekly ?? entry.weeklyDownloads,
    };
  });
}

/** 把一次联网搜索的结果（下载量）合并成缓存形状。 */
export function collectPluginDownloads(entries: readonly PluginCatalogEntry[]): {
  monthly: Record<string, number>;
  weekly: Record<string, number>;
} {
  const monthly: Record<string, number> = {};
  const weekly: Record<string, number> = {};
  for (const entry of entries) {
    if (typeof entry.monthlyDownloads === "number") monthly[entry.name] = entry.monthlyDownloads;
    if (typeof entry.weeklyDownloads === "number") weekly[entry.name] = entry.weeklyDownloads;
  }
  return { monthly, weekly };
}

export function searchPluginCatalog(entries: PluginCatalogEntry[], query: string): PluginCatalogEntry[] {
  const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (terms.length === 0) return sortPluginCatalog(entries);
  return sortPluginCatalog(entries.filter((entry) => {
    const haystack = [entry.name, entry.description, entry.descriptionZh ?? "", entry.descriptionEn ?? ""].join(" ").toLowerCase();
    return terms.every((term) => haystack.includes(term));
  }));
}
