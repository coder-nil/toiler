/** 插件目录由 Rust 接口提供，前端仅消费目录条目。 */
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
  monthlyDownloads?: number;
  weeklyDownloads?: number;
  npmUrl: string;
  repositoryUrl?: string;
  homepageUrl?: string;
}
