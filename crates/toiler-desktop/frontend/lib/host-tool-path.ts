// host-tool-path.ts - 让打包后的 Pi Desktop 也能在 PATH 上找到 npm
//
// pi 的包管理器（`DefaultPackageManager`）用裸命令名 `npm` 去 spawn（`getNpmCommand()`
// 默认 `{ command: "npm", args: [] }`），libuv 只按 `process.env.PATH` 解析。开发态由
// 终端启动，PATH 里有 nvm / Homebrew 的目录，一切正常；从 Dock / Finder 启动的打包版
// 继承的是 launchd 的最小 PATH（`/usr/bin:/bin:/usr/sbin:/sbin`），于是设置里的
// 「安装插件 / 更新插件」直接报：
//
//   spawn npm ENOENT
//
// 随 app 打包的 Node 自己也带一份 npm（`scripts/bundled-npm.mjs`），所以这里做两件事：
// 1. 找到**用户机器上**真正存在的 npm，把它所在的目录前置到 `process.env.PATH`——
//    已经在 PATH 上能找到 npm 时什么都不改，开发态行为完全不变；
// 2. 哪儿都没有时，才用随 app 打包的那一份（`PI_DESKTOP_RUNTIME_DIR`，由 Tauri 壳传入，
//    内含 `node` 与 `npm`/`npx` 启动器）兜底。
//
// 之后 process 派生的所有子进程（pi 的包管理器、bash 工具、git）都能解析到 npm。

import { existsSync, readdirSync } from "node:fs";
import { homedir } from "node:os";
import { posix, win32 } from "node:path";
import { withNodeOnPath } from "./npx";

export interface HostToolPathPlan {
  platform: NodeJS.Platform;
  /** PATH 上已经能找到的 npm 绝对路径；`null` 表示没找到。 */
  npmOnPath: string | null;
  /** 需要前置到 PATH 的目录（已去重、只保留真的含 npm 的目录）。 */
  directories: string[];
}

export interface HostToolPathProbe {
  platform?: NodeJS.Platform;
  home?: string | null;
  env?: NodeJS.ProcessEnv;
  pathValue?: string | null;
  /** 随 app 打包的 runtime 目录（内含 node 与 npm 启动器）；默认读 `PI_DESKTOP_RUNTIME_DIR`。 */
  runtimeDirectory?: string | null;
  /** 默认 `existsSync`，测试可注入。 */
  isFile?: (path: string) => boolean;
  /** 默认真实 readdir（目录不存在返回空数组），测试可注入。 */
  readDirectory?: (path: string) => string[];
}

/** Windows 上 npm 是 `npm.cmd`，nvm-windows / pnpm 也可能给出 `npm.exe`。 */
export function npmExecutableNames(platform: NodeJS.Platform = process.platform): readonly string[] {
  return platform === "win32" ? ["npm.cmd", "npm.exe", "npm.bat", "npm"] : ["npm"];
}

function pathApi(platform: NodeJS.Platform) {
  return platform === "win32" ? win32 : posix;
}

function splitPath(pathValue: string, platform: NodeJS.Platform): string[] {
  return pathValue
    .split(platform === "win32" ? ";" : ":")
    .map((entry) => entry.trim())
    .filter(Boolean);
}

function comparable(directory: string, platform: NodeJS.Platform): string {
  const trimmed = directory.replace(/[\\/]+$/, "");
  return platform === "win32" ? trimmed.toLowerCase() : trimmed;
}

function readDirectoryNames(directory: string): string[] {
  try {
    return readdirSync(directory, { withFileTypes: true })
      .filter((entry) => entry.isDirectory() || entry.isSymbolicLink())
      .map((entry) => entry.name);
  } catch {
    return [];
  }
}

/** `v24.20.0` / `24.20.0` / `v22` → `[24, 20, 0]`；取不到数字的返回 null（排到最后）。 */
function versionNumbers(name: string): number[] | null {
  const match = name.match(/\d+(?:\.\d+)*/);
  if (!match) return null;
  return match[0].split(".").map(Number);
}

/** 版本号新的排前面，让前置的目录尽量贴近用户平时 `npm` 用的那个 node。 */
export function compareNodeVersionsDescending(a: string, b: string): number {
  const left = versionNumbers(a);
  const right = versionNumbers(b);
  if (!left || !right) {
    if (left) return -1;
    if (right) return 1;
    return a < b ? -1 : a > b ? 1 : 0;
  }
  for (let index = 0; index < Math.max(left.length, right.length); index += 1) {
    const diff = (right[index] ?? 0) - (left[index] ?? 0);
    if (diff !== 0) return diff;
  }
  return 0;
}

interface VersionRoot {
  /** 装着各个 node 版本的目录。 */
  directory: string;
  /** 版本目录下 npm 所在的相对层级。 */
  suffix: string[];
}

function versionManagerRoots(
  platform: NodeJS.Platform,
  home: string | null,
  env: NodeJS.ProcessEnv,
): VersionRoot[] {
  const p = pathApi(platform);
  const roots: VersionRoot[] = [];
  if (home) {
    roots.push({ directory: p.join(home, ".nvm", "versions", "node"), suffix: ["bin"] });
    roots.push({ directory: p.join(home, ".local", "share", "fnm", "node-versions"), suffix: ["installation", "bin"] });
    roots.push({ directory: p.join(home, "Library", "Application Support", "fnm", "node-versions"), suffix: ["installation", "bin"] });
    roots.push({ directory: p.join(home, ".nodenv", "versions"), suffix: ["bin"] });
    roots.push({ directory: p.join(home, ".asdf", "installs", "nodejs"), suffix: ["bin"] });
  }
  if (platform === "win32") {
    // nvm-windows 把版本目录直接放在 `%NVM_HOME%` 下，`%NVM_SYMLINK%` 指向当前版本。
    const nvmHome = env.NVM_HOME?.trim();
    if (nvmHome) roots.push({ directory: nvmHome, suffix: [] });
    const appData = env.APPDATA?.trim();
    if (appData) roots.push({ directory: p.join(appData, "fnm", "node-versions"), suffix: ["installation"] });
  }
  return roots;
}

function staticBinDirectories(
  platform: NodeJS.Platform,
  home: string | null,
  env: NodeJS.ProcessEnv,
): string[] {
  const p = pathApi(platform);
  const dirs: string[] = [];
  if (platform === "win32") {
    const appData = env.APPDATA?.trim();
    const localAppData = env.LOCALAPPDATA?.trim();
    const programFiles = env.ProgramFiles?.trim();
    const programFilesX86 = env["ProgramFiles(x86)"]?.trim();
    const nvmSymlink = env.NVM_SYMLINK?.trim();
    if (appData) dirs.push(p.join(appData, "npm"));
    if (localAppData) dirs.push(p.join(localAppData, "Programs", "nodejs"));
    if (programFiles) dirs.push(p.join(programFiles, "nodejs"));
    if (programFilesX86) dirs.push(p.join(programFilesX86, "nodejs"));
    if (nvmSymlink) dirs.push(nvmSymlink);
    return dirs;
  }
  if (platform === "darwin") {
    dirs.push("/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin");
  } else {
    dirs.push("/usr/local/bin", "/usr/bin", "/bin", "/snap/bin");
  }
  if (home) {
    dirs.push(
      p.join(home, ".local", "bin"),
      p.join(home, ".volta", "bin"),
      p.join(home, ".asdf", "shims"),
      p.join(home, ".bun", "bin"),
    );
  }
  return dirs;
}

/** 候选目录：版本管理器（新版本优先）在前，系统/包管理器目录在后。 */
export interface Candidate {
  directory: string;
  /** 同一个版本管理器（或同一个静态目录）算一组，每组最多前置一个目录。 */
  group: string;
  /** 打包自带的 npm 只能在「哪儿都找不到」时使用，不应该抢用户自己的 node。 */
  fallback?: boolean;
}

function candidateEntries(probe: HostToolPathProbe = {}): Candidate[] {
  const platform = probe.platform ?? process.platform;
  const home = probe.home === undefined ? homedir() : probe.home;
  const env = probe.env ?? process.env;
  const p = pathApi(platform);
  const readDirectory = probe.readDirectory ?? readDirectoryNames;
  const candidates: Candidate[] = [];

  for (const root of versionManagerRoots(platform, home, env)) {
    const versions = readDirectory(root.directory).sort(compareNodeVersionsDescending);
    for (const version of versions) {
      candidates.push({ directory: p.join(root.directory, version, ...root.suffix), group: root.directory });
    }
  }
  for (const directory of staticBinDirectories(platform, home, env)) {
    candidates.push({ directory, group: directory });
  }
  const runtimeDirectory = probe.runtimeDirectory === undefined
    ? env.PI_DESKTOP_RUNTIME_DIR?.trim() || null
    : probe.runtimeDirectory;
  if (runtimeDirectory) {
    candidates.push({ directory: runtimeDirectory, group: "bundled-runtime", fallback: true });
  }
  return candidates;
}

/** 候选目录：版本管理器（新版本优先）在前，系统/包管理器目录在后。 */
export function candidateNodeBinDirectories(probe: HostToolPathProbe = {}): string[] {
  return candidateEntries(probe).map((candidate) => candidate.directory);
}

function findNpmIn(
  directories: readonly string[],
  platform: NodeJS.Platform,
  isFile: (path: string) => boolean,
): string | null {
  const p = pathApi(platform);
  for (const directory of directories) {
    for (const name of npmExecutableNames(platform)) {
      const candidate = p.join(directory, name);
      if (isFile(candidate)) return candidate;
    }
  }
  return null;
}

/**
 * 只做判断、不修改环境：PATH 上能找到 npm 就原样返回；找不到时给出「真的含 npm 且
 * 不在 PATH 上」的目录列表，调用方拿它去前置。
 */
export function planHostToolPath(probe: HostToolPathProbe = {}): HostToolPathPlan {
  const platform = probe.platform ?? process.platform;
  const isFile = probe.isFile ?? existsSync;
  const pathValue = probe.pathValue === undefined ? process.env.PATH : probe.pathValue;
  const currentEntries = splitPath(pathValue ?? "", platform);
  const npmOnPath = findNpmIn(currentEntries, platform, isFile);
  if (npmOnPath) return { platform, npmOnPath, directories: [] };

  const alreadyOnPath = new Set(currentEntries.map((entry) => comparable(entry, platform)));
  const usedGroups = new Set<string>();
  const directories: string[] = [];
  const accept = (candidate: Candidate): void => {
    // 一个版本管理器只前置一个目录：把 ~/.nvm/versions/node/* 全部塞进 PATH 既多余，
    // 也会让旧版本的 node 盖住 /usr/local/bin 里的那个。
    if (usedGroups.has(candidate.group)) return;
    const key = comparable(candidate.directory, platform);
    if (alreadyOnPath.has(key)) {
      usedGroups.add(candidate.group);
      return;
    }
    if (!findNpmIn([candidate.directory], platform, isFile)) return;
    usedGroups.add(candidate.group);
    alreadyOnPath.add(key);
    directories.push(candidate.directory);
  };

  const entries = candidateEntries(probe);
  for (const candidate of entries) {
    if (!candidate.fallback) accept(candidate);
  }
  // 打包自带的 npm 只在系统里一个都没有时兜底：装了 Node.js 的机器继续用
  // 用户自己的版本，不会被 app 内置的 node 22 抢走。
  if (directories.length === 0) {
    for (const candidate of entries) {
      if (candidate.fallback) accept(candidate);
    }
  }
  return { platform, npmOnPath: null, directories };
}

function applyDirectories(directories: readonly string[], platform: NodeJS.Platform): void {
  // withNodeOnPath 负责合并仅大小写不同的 `Path` / `PATH`（Windows 上重复键会让
  // libuv 直接拒绝 spawn），结果再写回 process.env。
  const merged = withNodeOnPath({ ...process.env }, directories, platform);
  for (const key of Object.keys(process.env)) {
    if (/^path$/i.test(key)) delete process.env[key];
  }
  const pathKey = Object.keys(merged).find((key) => /^path$/i.test(key)) ?? "PATH";
  process.env[pathKey] = merged[pathKey] ?? directories.join(platform === "win32" ? ";" : ":");
}

let cachedPlan: HostToolPathPlan | null = null;

/**
 * 幂等地修正 `process.env.PATH`：找到 npm、前置它的目录，然后缓存结果。
 * 插件安装 / 更新这类会 spawn 裸 `npm` 的路径都要在开始干活前调用一次。
 */
export function ensureHostToolPath(probe: HostToolPathProbe = {}): HostToolPathPlan {
  if (cachedPlan) return cachedPlan;
  const plan = planHostToolPath(probe);
  cachedPlan = plan;
  if (plan.npmOnPath) return plan;
  if (plan.directories.length > 0) {
    applyDirectories(plan.directories, plan.platform);
    console.log(`[host-path] npm was not on PATH; prepended ${plan.directories.join(", ")}`);
    return plan;
  }
  console.warn(
    "[host-path] npm was not found on PATH or in any known Node.js installation; " +
      "plugin install/update will fail until Node.js is installed.",
  );
  return plan;
}

/** 给「npm 根本不存在」这种情况的中文可操作提示；找到 npm 时返回空字符串。 */
export function missingNpmHint(plan: HostToolPathPlan): string {
  if (plan.npmOnPath || plan.directories.length > 0) return "";
  return (
    "没有找到可用的 npm，无法安装或更新插件：系统 PATH 上没有，随 app 打包的那一份也不可用。" +
    "请重新安装 Pi Desktop；作为临时方案，也可以安装 Node.js（自带 npm），" +
    "或在 ~/.pi/agent/settings.json 里用 npmCommand 指定 npm 的绝对路径。"
  );
}

/** 仅供测试：清掉缓存（已经写进 `process.env.PATH` 的内容不会被撤销）。 */
export function resetHostToolPathForTests(): void {
  cachedPlan = null;
}
