import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { win32 } from "node:path";
import { getPowerShellConfig, getShellConfig } from "@earendil-works/pi-coding-agent";
import { isPowerShellToolEnabled } from "./powershell-settings";

export type DesktopShellTool = "bash" | "powershell";

export interface DesktopShellSelection {
  tool: DesktopShellTool;
  /**
   * Explicit bash path to hand to pi's bash tool. Only set when the SDK probe
   * cannot find bash on its own; `undefined` keeps pi's normal resolution.
   */
  shellPath?: string;
}

interface ShellPathSettings {
  getShellPath(): string | undefined;
  /**
   * pi's `defaultTools` setting. An explicit `powershell` entry is the user's
   * own choice (设置 → 常规 → Shell 工具) and outranks the availability probe.
   */
  getDefaultTools?(): string[] | undefined;
}

/**
 * Pick the shell tool pi should expose on Windows.
 *
 * pi's `bash` tool only knows how to launch Git Bash / MSYS2 / Cygwin. A
 * Windows machine without any of those can still run commands through the
 * built-in `powershell` tool, so fall back to it instead of failing every
 * command with `No bash shell found`.
 *
 * An explicit `shellPath` is always treated as a bash configuration. If it is
 * broken, keep bash so the SDK reports the bad path instead of silently
 * ignoring the user's setting.
 */
export function selectShellTool(options: {
  platform: NodeJS.Platform;
  configuredShellPath?: string;
  bashAvailable: boolean;
  powerShellAvailable: boolean;
}): DesktopShellTool {
  if (options.platform !== "win32") return "bash";
  if (options.configuredShellPath) return "bash";
  if (options.bashAvailable) return "bash";
  return options.powerShellAvailable ? "powershell" : "bash";
}

/**
 * Git for Windows installers put `git.exe` in `<root>\cmd` on PATH, while
 * `bash.exe` lives in `<root>\bin` — which is not on PATH. The SDK probe only
 * checks the default `%ProgramFiles%\Git` locations, so a custom or portable
 * install looks like "no bash" even though Git itself works.
 *
 * Derive the bash paths from the git executable so a working Git installation
 * keeps bash semantics instead of dropping to PowerShell.
 */
export function gitBashCandidates(gitExecutable: string, execPath?: string): string[] {
  const candidates: string[] = [];
  if (execPath) {
    // <root>\mingw64\libexec\git-core -> <root>\bin\bash.exe
    candidates.push(win32.join(win32.resolve(execPath, "..", "..", ".."), "bin", "bash.exe"));
  }
  // <root>\cmd\git.exe -> <root>\bin\bash.exe
  candidates.push(win32.join(win32.dirname(win32.dirname(gitExecutable)), "bin", "bash.exe"));
  return [...new Set(candidates)];
}

function findExecutableOnPath(executable: string): string | null {
  const pathKey = Object.keys(process.env).find((name) => name.toUpperCase() === "PATH");
  const entries = (pathKey ? process.env[pathKey] : "")?.split(";") ?? [];
  for (const entry of entries) {
    if (!entry) continue;
    const candidate = win32.join(entry, executable);
    if (existsSync(candidate)) return candidate;
  }
  return null;
}

/** Resolve `<git install>\bin\bash.exe` from the git executable on PATH. */
export function resolveGitBashPath(gitExecutable: string | null = findExecutableOnPath("git.exe")): string | null {
  if (process.platform !== "win32" || !gitExecutable) return null;

  let execPath: string | undefined;
  try {
    const result = spawnSync(gitExecutable, ["--exec-path"], {
      encoding: "utf8",
      timeout: 5_000,
      windowsHide: true,
    });
    if (result.status === 0 && result.stdout?.trim()) execPath = result.stdout.trim();
  } catch {
    // `--exec-path` is only a hint; the install-root guess still applies.
  }

  for (const candidate of gitBashCandidates(gitExecutable, execPath)) {
    if (existsSync(candidate)) return candidate;
  }
  return null;
}

/** Resolve the shell tool (and any bash path) using host settings and probes. */
export function resolveShellSelection(
  settings: ShellPathSettings,
  platform: NodeJS.Platform = process.platform,
): DesktopShellSelection {
  const configuredShellPath = settings.getShellPath();
  // An explicit shell path is a bash configuration on every platform and must
  // reach the bash tool unchanged, or custom shells (Cygwin, a versioned
  // bash.exe) would silently stop being used.
  if (configuredShellPath) return { tool: "bash", shellPath: configuredShellPath };
  if (platform !== "win32") return { tool: "bash" };

  // 用户在设置里显式选了 PowerShell：这是他的选择，不去探测 bash 是否存在。
  if (isPowerShellToolEnabled(settings.getDefaultTools?.(), platform)) {
    return { tool: "powershell" };
  }

  let bashAvailable = false;
  try {
    getShellConfig();
    bashAvailable = true;
  } catch {
    // No Git Bash / MSYS2 / Cygwin installation found in the default spots.
  }

  let gitBashPath: string | undefined;
  if (!bashAvailable) {
    gitBashPath = resolveGitBashPath() ?? undefined;
    if (gitBashPath) bashAvailable = true;
  }

  let powerShellAvailable = false;
  if (!bashAvailable) {
    try {
      getPowerShellConfig();
      powerShellAvailable = true;
    } catch {
      // The SDK will report the missing-bash error, which is actionable.
    }
  }

  const tool = selectShellTool({ platform, bashAvailable, powerShellAvailable });
  return tool === "bash" && gitBashPath ? { tool, shellPath: gitBashPath } : { tool };
}

/** Swap the portable "bash" slot for PowerShell when Windows has no bash. */
export function applyShellTool(toolNames: readonly string[], shell: DesktopShellTool): string[] {
  if (shell === "bash" || !toolNames.includes("bash")) return [...toolNames];
  return [...new Set(toolNames.map((name) => (name === "bash" ? "powershell" : name)))];
}

/**
 * Normalize a session's inherited active-tool list.
 *
 * The host extension registers the PowerShell override as an extension tool, so
 * pi's `includeAllExtensionTools` startup path can activate it alongside the
 * default bash tool. Keep only the shell the Desktop actually selected.
 */
export function normalizeActiveShellTool(
  toolNames: readonly string[],
  shell: DesktopShellTool,
): string[] {
  const withoutPowerShell = toolNames.filter((name) => name !== "powershell");
  if (shell !== "powershell") return withoutPowerShell;
  return [...new Set(withoutPowerShell.map((name) => (name === "bash" ? "powershell" : name)))];
}
