// npx.ts - Cross-platform npx invoker for skills operations
import { execFile } from "child_process";
import { promisify } from "util";
import { existsSync, readdirSync } from "fs";
import { dirname, join } from "path";
import { execPath } from "process";

const execFileAsync = promisify(execFile);

interface NpxLocation {
  path: string;
  layout: string;
  nodeBin: string;
}

/**
 * Try to locate `npx-cli.js` in various Node.js installations.
 * Priority order:
 *   1. npm bundled with the current Node.js (for standard installs, and Pi Desktop's own runtime)
 *   2. Common system-wide paths (Homebrew, macOS pkg, Linux common)
 *   3. NVM installations (newest to oldest)
 */
function findNpxCli(): NpxLocation | null {
  // 1. Check the Node.js binary's own npm bundle (for standard installs)
  const nodeDir = dirname(execPath);
  const directCandidates: { path: string; layout: string }[] = [
    // Windows MSI installer layout: node.exe and node_modules share a dir
    { path: join(nodeDir, "node_modules", "npm", "bin", "npx-cli.js"), layout: "Windows MSI" },
    // Unix layout: .../bin/node + .../lib/node_modules/npm/bin/npx-cli.js
    { path: join(nodeDir, "..", "lib", "node_modules", "npm", "bin", "npx-cli.js"), layout: "Unix/nvm/brew" },
    // macOS pkg (legacy but still possible)
    { path: join(nodeDir, "../lib/node_modules/npm/bin/npx-cli.js"), layout: "macOS pkg" },
    // Pi Desktop 打包的 runtime：node 在根目录，npm 包在 lib/node_modules/npm（没装
    // Node.js 的机器也能装技能，不再依赖系统 npx）。
    { path: join(nodeDir, "lib", "node_modules", "npm", "bin", "npx-cli.js"), layout: "Pi Desktop runtime" },
  ];
  
  for (const { path, layout } of directCandidates) {
    if (existsSync(path)) {
      console.log(`[npx] Found npx-cli.js at ${path} (${layout})`);
      return { path, layout, nodeBin: execPath };
    }
  }

  // 2. Try common system-wide locations (only on Unix/macOS/Linux)
  const systemCandidates: { path: string; nodeBin: string }[] = [
    // macOS Homebrew (Apple Silicon)
    { path: "/opt/homebrew/lib/node_modules/npm/bin/npx-cli.js", nodeBin: "/opt/homebrew/bin/node" },
    // macOS Homebrew (Intel) and many Linux distros
    { path: "/usr/local/lib/node_modules/npm/bin/npx-cli.js", nodeBin: "/usr/local/bin/node" },
    // Debian/Ubuntu default
    { path: "/usr/lib/node_modules/npm/bin/npx-cli.js", nodeBin: "/usr/bin/node" },
  ];

  for (const { path, nodeBin } of systemCandidates) {
    if (existsSync(path) && existsSync(nodeBin)) {
      console.log(`[npx] Found npx-cli.js at ${path} (system, using ${nodeBin})`);
      return { path, layout: "System", nodeBin };
    }
  }

  // 3. Probe NVM installations (scan all versions, pick newest with npm)
  const home = process.env.HOME || process.env.USERPROFILE;
  if (home) {
    const nvmVersionsDir = join(home, ".nvm", "versions", "node");
    if (existsSync(nvmVersionsDir)) {
      try {
        const entries = readdirSync(nvmVersionsDir, { withFileTypes: true })
          .filter(d => d.isDirectory())
          .map(d => d.name)
          // Sort versions descending (newest first): v24.20.0 > v22.19.0 > v18.20.8
          .sort((a, b) => {
            const extractVersion = (s: string): number[] => s.replace(/^v/, "").split(".").map(Number);
            const va = extractVersion(a);
            const vb = extractVersion(b);
            for (let i = 0; i < Math.max(va.length, vb.length); i++) {
              const av = va[i] ?? 0;
              const bv = vb[i] ?? 0;
              if (av !== bv) return bv - av;
            }
            return 0;
          });

        for (const version of entries) {
          const npxPath = join(nvmVersionsDir, version, "lib", "node_modules", "npm", "bin", "npx-cli.js");
          const nodePath = join(nvmVersionsDir, version, "bin", "node");
          if (existsSync(npxPath) && existsSync(nodePath)) {
            console.log(`[npx] Found npx-cli.js at ${npxPath} (NVM ${version})`);
            return { path: npxPath, layout: `NVM ${version}`, nodeBin: nodePath };
          }
        }
      } catch (e) {
        console.error("[npx] Failed to scan NVM versions:", e);
      }
    }
  }

  return null;
}

/**
 * `npm exec` 会用 `sh`/`cmd` 去执行包自己的 launcher（skills 的 bin 是
 * `#!/usr/bin/env node`），而 npx 只会把 prefix/bin 和 node_modules/.bin 补进子进程
 * PATH，不会补 node 自己的目录。从 Dock/Finder 启动的 Pi Desktop 继承的 PATH 只有
 * `/usr/bin:/bin:/usr/sbin:/sbin`，nvm、Homebrew 的 node 目录都写在 shell 的 rc 文件里，
 * 于是 launcher 找不到解释器，装包成功但一执行就失败：
 *
 *   npm warn exec The following package was not found and will be installed: skills@1.5.18
 *   env: node: No such file or directory
 *
 * 把实际用来跑 npx 的 node 目录补到子进程 PATH 最前面，launcher 就一定能找到解释器。
 */
export function withNodeOnPath(
  environment: NodeJS.ProcessEnv,
  nodeDirectories: readonly (string | undefined)[],
  platform: NodeJS.Platform = process.platform,
): NodeJS.ProcessEnv {
  const delimiter = platform === "win32" ? ";" : ":";
  // Windows 上环境块的键可能是 `Path`，且大小写不敏感；把所有变体合并成一个键，
  // 否则会留下两个仅大小写不同的 PATH，spawn 时 libuv 会直接报错。
  const pathKeys = platform === "win32"
    ? Object.keys(environment).filter((name) => /^path$/i.test(name))
    : ["PATH"];
  const pathKey = pathKeys[0] ?? (platform === "win32" ? "Path" : "PATH");
  const currentEntries = pathKeys
    .flatMap((key) => (environment[key] ?? "").split(delimiter))
    .filter(Boolean);
  const nodeEntries = nodeDirectories
    .map((directory) => directory?.trim())
    .filter((directory): directory is string => Boolean(directory));
  const entries = [...new Set([...nodeEntries, ...currentEntries])];

  const result = { ...environment };
  for (const key of pathKeys) delete result[key];
  if (entries.length > 0) result[pathKey] = entries.join(delimiter);
  return result;
}

export interface RunNpxOptions {
  timeout?: number;
  maxBuffer?: number;
  cwd?: string;
  env?: Record<string, string>;
}

export interface RunNpxResult {
  stdout: string;
  stderr: string;
}

/**
 * Cross-platform wrapper for invoking `npx <args>` without ever using a shell,
 * so user-controlled arguments are never interpreted as shell syntax.
 * 
 * This function will:
 * 1. First try to find npx-cli.js bundled with the current Node.js
 * 2. Fall back to system-wide npm installations (Homebrew, /usr/local, etc.)
 * 3. Scan NVM installations (newest to oldest)
 * 4. Throw a descriptive error if nothing is found
 */
export async function runNpx(args: string[], opts: RunNpxOptions = {}): Promise<RunNpxResult> {
  const npxCli = findNpxCli();
  
  console.log("[npx] Current execPath:", execPath);
  console.log("[npx] findNpxCli result:", npxCli ? { path: "<hidden>", layout: npxCli.layout, nodeBin: "<hidden>" } : null);
  
  if (!npxCli) {
    const nvmHint = process.env.HOME 
      ? `Check ~/.nvm/versions/node/ for Node.js installations with npm.`
      : "Reinstall Node.js with npm included.";
    
    throw new Error(
      `Cannot locate npx-cli.js. Pi Desktop ships its own npm under <runtime>/lib/node_modules/npm; ` +
      `if that is missing the install is incomplete.\n` +
      `Searched in:\n` +
      `  - Bundled with: ${execPath}\n` +
      `  - Common system paths (/opt/homebrew, /usr/local, /usr)\n` +
      `  - NVM installations in ${process.env.HOME || "~"}\n\n` +
      `Solutions:\n` +
      `  1. Reinstall Pi Desktop\n` +
      `  2. Or ensure Node.js is installed globally (via nvm or another installer)\n` +
      `  3. Or run 'npm install -g npm' in a terminal to restore npm\n\n` +
      `${nvmHint}`
    );
  }

  console.log(`[npx] Using npx from ${npxCli.layout}: ${npxCli.path}`);
  console.log(`[npx] Will use node binary: ${npxCli.nodeBin}`);
  
  const command = npxCli.nodeBin;
  const commandArgs = [npxCli.path, ...args];
  
  const effectiveCwd = opts.cwd || process.cwd();
  // spawn 用的 node 目录必须进 PATH：npm exec 的 launcher 靠 `#!/usr/bin/env node` 启动。
  const effectiveEnv = withNodeOnPath(
    { ...process.env, ...(opts.env || {}), FORCE_COLOR: "0", GIT_TERMINAL_PROMPT: "0" },
    [dirname(npxCli.nodeBin), dirname(execPath)],
  );

  console.log(`[npx] Executing: ${command} ${commandArgs.join(" ")}`);
  console.log(`[npx] Working directory: ${effectiveCwd}`);

  try {
    const result = await execFileAsync(command, commandArgs, {
      timeout: opts.timeout,
      maxBuffer: opts.maxBuffer,
      cwd: effectiveCwd,
      env: effectiveEnv,
    });
    
    console.log(`[npx] Success: ${result.stdout.length} bytes output`);
    return { stdout: result.stdout || "", stderr: result.stderr || "" };
  } catch (error) {
    const err = error as { code?: string; message?: string; errno?: number; stdout?: string; stderr?: string };
    console.error("[npx] Execution failed:", { code: err.code, message: err.message });
    if (err.stdout) console.error("[npx] stdout:", err.stdout.substring(0, 500));
    if (err.stderr) console.error("[npx] stderr:", err.stderr.substring(0, 500));
    
    if (err.code === "ENOENT" || err.errno === -2) {
      throw new Error(
        `Failed to spawn Node.js process at ${command}.\n` +
        `This usually means:\n` +
        `  - The node binary at ${command} does not exist or is not executable\n` +
        `  - Environment permissions are preventing execution\n\n` +
        `Original error: ${err.message}`
      );
    }
    
    // Re-throw the original error for better context
    throw error;
  }
}
