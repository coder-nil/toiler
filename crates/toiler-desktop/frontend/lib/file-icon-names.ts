/**
 * 文件名 → Catppuccin 图标名（纯查表，不含 JSX）。
 *
 * 从 `components/FileIcons.tsx` 抽出来的唯一理由是可测：代码块头部的「围栏语言 →
 * 文件图标」要断言「映射表里的每个文件名都能解析出图标且对应的 SVG 确实存在」，
 * 这条断言在组件文件里做不干净。组件那边只负责把图标名画成 `<span>`。
 */

export type CatppuccinIconName =
  | "_file"
  | "_folder"
  | "_folder_open"
  | "bash"
  | "config"
  | "css"
  | "database"
  | "docker"
  | "env"
  | "git"
  | "graphql"
  | "html"
  | "java"
  | "javascript"
  | "javascript-react"
  | "json"
  | "lock"
  | "npm-lock"
  | "bun-lock"
  | "next"
  | "eslint"
  | "markdown"
  | "ms-word"
  | "pdf"
  | "python"
  | "rust"
  | "sass"
  | "terraform"
  | "toml"
  | "typescript"
  | "typescript-react"
  | "yaml"
  | "go";

const EXTENSION_ICONS: Record<string, CatppuccinIconName> = {
  ts: "typescript",
  tsx: "typescript-react",
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  jsx: "javascript-react",
  java: "java",
  py: "python",
  json: "json",
  jsonl: "json",
  css: "css",
  less: "css",
  scss: "sass",
  html: "html",
  htm: "html",
  md: "markdown",
  mdx: "markdown",
  yaml: "yaml",
  yml: "yaml",
  toml: "toml",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  fish: "bash",
  rs: "rust",
  go: "go",
  sql: "database",
  graphql: "graphql",
  gql: "graphql",
  tf: "terraform",
  hcl: "terraform",
  docx: "ms-word",
  pdf: "pdf",
  lock: "lock",
};

function getSpecialFileIcon(name: string): CatppuccinIconName | undefined {
  if (name === "dockerfile" || name.startsWith("dockerfile.")) return "docker";
  if (name === ".env" || name.startsWith(".env.")) return "env";
  if ([".gitignore", ".gitattributes", ".gitmodules"].includes(name)) return "git";
  if (name === "package-lock.json") return "npm-lock";
  if (name === "bun.lock") return "bun-lock";
  if (["next.config.js", "next.config.mjs", "next.config.cjs", "next.config.ts"].includes(name)) return "next";
  if ([".eslintrc", ".eslintrc.js", ".eslintrc.json", ".eslintrc.yml", "eslint.config.mjs", "eslint.config.js"].includes(name)) return "eslint";
  if (["yarn.lock", "pnpm-lock.yaml", "cargo.lock"].includes(name)) return "lock";
  if (name.endsWith(".config.ts") || name.endsWith(".config.js") || name.endsWith(".config.mjs") || name.endsWith(".config.cjs")) return "config";
  return undefined;
}

/**
 * 解析文件名（或单独一个扩展名）对应的图标名；解析不到返回 `undefined`。
 *
 * 传 `ts` 和传 `a.ts` 结果一样：扩展名用 `split(".")` 取最后一段，不带点的输入
 * 自然整段就是扩展名。
 */
export function resolveFileIconName(fileName: string): CatppuccinIconName | undefined {
  const lower = fileName.toLowerCase();
  const specialIcon = getSpecialFileIcon(lower);
  if (specialIcon) return specialIcon;

  const ext = lower.split(".").pop() ?? "";
  return EXTENSION_ICONS[ext];
}
