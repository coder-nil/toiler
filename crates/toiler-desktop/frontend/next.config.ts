import type { NextConfig } from "next";
import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";

// Toiler 的前端是 pi-desktop 那份 Next 应用的静态导出版本。
//
// 与 pi 的 next.config.ts 的差异（每一条都是「这里没有 Node 后端」导致的）：
//
// | pi | toiler | 原因 |
// |---|---|---|
// | `output` 按 `PI_WEB_BUILD_TARGET` 在 `standalone` / `export` 间切 | 恒为 `export` | toiler 只有静态托管，`/api` 由 axum 提供，不跑 Next server |
// | `distDir` 按构建目标取三个值 | 恒为 `dist` | 只有一个构建目标 |
// | `outputFileTracingIncludes` 兜 pi-coding-agent 的运行时资源 | 删掉 | 那是 standalone 打包用的，静态导出没有 server 侧追踪 |
// | `serverExternalPackages` | 删掉 | 同上；导出构建里没有 server bundle |
// | `headers()` 给 `/`、`/sw.js` 加安全头 | 删掉 | `output: export` 不支持自定义 headers（Next 会警告）。这些头改由 Rust 的 `assets.rs` 发 |
// | `allowedDevOrigins` | 删掉 | 不做 Next dev server |
//
// 保留的部分是真正影响产物的：`images.unoptimized`（静态导出不能用优化器）与
// 四个 `NEXT_PUBLIC_*` 版本变量（关于对话框读它们）。

const configDir = dirname(fileURLToPath(import.meta.url));

let piVersion = "unknown";
let packageVersion = "0.0.0";
try {
  // pi-coding-agent 的版本会显示在「关于」里，是唯一需要读 node_modules 的地方。
  const piPkgPath = join(
    configDir,
    "node_modules/@earendil-works/pi-coding-agent/package.json",
  );
  piVersion = (JSON.parse(readFileSync(piPkgPath, "utf8")) as { version: string }).version;
} catch {
  /* 没装依赖时给默认值，不让构建直接失败 */
}
try {
  const packagePath = join(configDir, "package.json");
  packageVersion = (JSON.parse(readFileSync(packagePath, "utf8")) as { version: string }).version;
} catch {
  /* 同上 */
}

const nextConfig: NextConfig = {
  output: "export",
  distDir: "dist",
  outputFileTracingRoot: configDir,
  images: { unoptimized: true },
  env: {
    NEXT_PUBLIC_APP_VERSION: packageVersion,
    NEXT_PUBLIC_PACKAGE_VERSION: packageVersion,
    NEXT_PUBLIC_PI_VERSION: piVersion,
  },
};

export default nextConfig;
