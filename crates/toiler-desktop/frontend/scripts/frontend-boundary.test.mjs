import assert from "node:assert/strict";
import { builtinModules } from "node:module";
import { readdir, readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import test from "node:test";
import ts from "typescript";

const root = fileURLToPath(new URL("../", import.meta.url));
const builtins = new Set(builtinModules.map((name) => name.replace(/^node:/, "")));

async function sourceFiles(directory) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...await sourceFiles(path));
    else if (/\.tsx?$/.test(entry.name)) files.push(path);
  }
  return files;
}

test("浏览器源码不得重新引入旧 pi SDK 或 Node 服务模块", async () => {
  for (const directory of ["app", "components", "hooks", "lib"]) {
    for (const file of await sourceFiles(join(root, directory))) {
      const source = await readFile(file, "utf8");
      for (const { fileName } of ts.preProcessFile(source, true, true).importedFiles) {
        assert.ok(!fileName.startsWith("@earendil-works/pi-"), `${file} 引用了旧 SDK：${fileName}`);
        assert.ok(!fileName.startsWith("node:") && !builtins.has(fileName), `${file} 引用了 Node 模块：${fileName}`);
      }
    }
  }
});

test("依赖与构建配置不再保留 pi SDK", async () => {
  const pkg = JSON.parse(await readFile(join(root, "package.json"), "utf8"));
  const lock = JSON.parse(await readFile(join(root, "package-lock.json"), "utf8"));
  for (const name of Object.keys({ ...pkg.dependencies, ...pkg.devDependencies })) {
    assert.ok(!name.startsWith("@earendil-works/pi-"), name);
  }
  for (const name of Object.keys(lock.packages)) {
    assert.ok(!name.includes("node_modules/@earendil-works/pi-"), name);
  }
  const config = await readFile(join(root, "next.config.ts"), "utf8");
  assert.ok(!config.includes("@earendil-works/pi-"));
  assert.ok(!config.includes("NEXT_PUBLIC_PI_VERSION"));
});
