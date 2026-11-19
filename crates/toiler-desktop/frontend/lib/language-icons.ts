/**
 * Markdown 代码围栏的语言名 → 文件名，供代码块头部取文件图标。
 *
 * 为什么绕一层「文件名」而不是直接给图标名：文件面板、TabBar、聊天提及用的是
 * 同一套 `getFileIcon(name)`（按扩展名 / 特殊文件名查 `public/icons/catppuccin`）。
 * 把围栏语言翻译成文件名，代码块就能命中同一份映射表，不用维护第二份图标清单；
 * 反过来，图标表里没有的语言（xml、c、makefile…）自然就落空，头部只显示文字，
 * 这比塞一个通用文件图标更不容易误导读代码的人。
 *
 * 同一个语言会有多个围栏写法（```ts / ```typescript / ```TS），键一律用小写；
 * 值写成「能真实存在于磁盘上的文件名」，这样 `Dockerfile` 这类按特殊名匹配的
 * 图标也能走同一条路径。
 */
const CODE_LANGUAGE_FILES: Record<string, string> = {
  typescript: "ts",
  ts: "ts",
  mts: "ts",
  cts: "ts",
  tsx: "tsx",
  javascript: "js",
  js: "js",
  mjs: "js",
  cjs: "js",
  jsx: "jsx",
  json: "json",
  jsonc: "json",
  json5: "json",
  jsonl: "jsonl",
  css: "css",
  less: "less",
  scss: "scss",
  sass: "scss",
  html: "html",
  htm: "htm",
  markdown: "md",
  md: "md",
  mdx: "mdx",
  yaml: "yaml",
  yml: "yml",
  toml: "toml",
  python: "py",
  py: "py",
  python3: "py",
  rust: "rs",
  rs: "rs",
  go: "go",
  golang: "go",
  java: "java",
  sql: "sql",
  mysql: "sql",
  postgres: "sql",
  postgresql: "sql",
  sqlite: "sql",
  graphql: "graphql",
  gql: "graphql",
  terraform: "tf",
  tf: "tf",
  hcl: "hcl",
  shell: "sh",
  shellscript: "sh",
  sh: "sh",
  bash: "bash",
  zsh: "zsh",
  fish: "fish",
  console: "sh",
  terminal: "sh",
  dockerfile: "Dockerfile",
  docker: "Dockerfile",
};

/**
 * 返回该围栏语言对应的文件名；图标表里没有这门语言时返回 `null`。
 *
 * 只做一次查表，不做「猜扩展名」的兜底：`getFileIcon` 对未知扩展名会给通用文件
 * 图标，那个兜底在这里会把 `text`、`diff` 也画成文件，等于每条代码块都带图标。
 */
export function codeLanguageIconFile(language: string | null | undefined): string | null {
  const key = (language ?? "").trim().toLowerCase();
  if (!key) return null;
  return CODE_LANGUAGE_FILES[key] ?? null;
}
