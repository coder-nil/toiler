// 把 Next 的静态导出产物构建到 `dist/`。
//
// 产物不需要手动搬运：`crates/toiler-server/build.rs` 在嵌入前端时会优先找
// 这个目录（`crates/toiler-desktop/frontend/dist`），找不到才退回内置占位页。
// 这样仓库里只有源码，`npm run build` 的产物全部落在 .gitignore 覆盖的路径下。
//
// 用法：
//   npm install        # 首次
//   npm run build      # 构建产物到 dist/
//   然后重新编译 Rust：cargo build -p toiler-desktop

import { rm, access, readdir } from "node:fs/promises";
import { spawn } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const frontendRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const distDirectory = join(frontendRoot, "dist");

function run(command, arguments_, options = {}) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(command, arguments_, {
      cwd: frontendRoot,
      stdio: "inherit",
      ...options,
    });
    child.once("error", reject);
    child.once("exit", (code) => {
      if (code === 0) resolveRun();
      else reject(new Error(`命令失败（退出码 ${code}）：${command} ${arguments_.join(" ")}`));
    });
  });
}

// 依赖没装时 Next 会以「找不到 next」告终，先给出可操作的提示。
try {
  await access(join(frontendRoot, "node_modules", "next"));
} catch {
  console.error("缺少 node_modules，先在 crates/toiler-desktop/frontend 里跑 npm install");
  process.exit(1);
}

// `output: export` 不会自己清 dist/，旧的 chunk 残留会混进新构建。
await rm(distDirectory, { recursive: true, force: true });

// 显式指定 webpack：这个 shell 里带着 `TURBOPACK=1`（Pi Desktop 自己设的），
// 两个 bundler 同时出现 Next 会直接报错。置空字符串不算取消，必须真正删掉这个
// 环境变量，否则子进程仍会读到它。
//
// 同理要删掉 Pi Desktop 注入的 `__NEXT_PRIVATE_STANDALONE_CONFIG` /
// `__NEXT_PRIVATE_ORIGIN`：前者是 **pi-desktop 自己的** Next 配置（standalone、
// distDir 指向它的 `.next`），子进程读到后会在 `next build` 里带着错的配置跑，
// 报一个与源码无关的 `TypeError: generate is not a function`。而脚本在这条命令
// 之前已经 `rm -rf dist`，所以一次失败的构建会把产物一起删掉。
const childEnvironment = { ...process.env };
delete childEnvironment.TURBOPACK;
delete childEnvironment.__NEXT_PRIVATE_STANDALONE_CONFIG;
delete childEnvironment.__NEXT_PRIVATE_ORIGIN;
await run(process.execPath, ["node_modules/next/dist/bin/next", "build", "--webpack"], {
  env: childEnvironment,
});

try {
  await access(join(distDirectory, "index.html"));
} catch {
  console.error(`构建没有产出 index.html，检查 ${distDirectory}`);
  process.exit(1);
}

const files = await countFiles(distDirectory);
console.log(`\n产物：${distDirectory}（${files} 个文件）`);
console.log("接下来重新编译 Rust 以嵌入新产物：cargo build -p toiler-desktop");

async function countFiles(directory) {
  let total = 0;
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const child = join(directory, entry.name);
    total += entry.isDirectory() ? await countFiles(child) : 1;
  }
  return total;
}
