import { chmodSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { getAgentDir } from "@earendil-works/pi-coding-agent";
import lockfile from "proper-lockfile";
import { withSettingsSchema } from "./settings-schema";

// The PowerShell switch is stored as pi's own `defaultTools` setting (the same
// file the CLI reads) instead of a desktop-only preference: `bash` and
// `powershell` are two slots for the same capability, and an explicit
// `powershell` entry is how the rest of pi records "use PowerShell here".
//
// Auto-detection still runs when the setting says nothing (lib/shell-tool.ts);
// this module only owns the explicit, user-visible choice.

const DEFAULT_TOOLS = ["read", "bash", "edit", "write"];
const SHELL_TOOLS = new Set(["bash", "powershell"]);

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** True when `defaultTools` explicitly selects PowerShell over bash. */
export function isPowerShellToolEnabled(
  defaultTools: readonly string[] | undefined,
  platform: NodeJS.Platform = process.platform,
): boolean {
  return platform === "win32"
    && defaultTools?.includes("powershell") === true
    && !defaultTools.includes("bash");
}

/** Swap whichever shell slot appears for the requested one. */
export function replaceShellTool(
  toolNames: readonly string[],
  usePowerShell: boolean,
): string[] {
  const shell = usePowerShell ? "powershell" : "bash";
  const result: string[] = [];
  for (const name of toolNames) {
    const next = SHELL_TOOLS.has(name) ? shell : name;
    if (!result.includes(next)) result.push(next);
  }
  return result;
}

export function getPowerShellSettingsPath(agentDir = getAgentDir()): string {
  return join(agentDir, "settings.json");
}

function parseSettings(path: string): Record<string, unknown> {
  if (!existsSync(path)) return {};
  const parsed: unknown = JSON.parse(readFileSync(path, "utf8"));
  if (!isRecord(parsed)) throw new Error("Invalid settings.json: expected an object");
  return parsed;
}

function configuredTools(settings: Record<string, unknown>): string[] | undefined {
  if (settings.defaultTools === undefined) return undefined;
  if (
    !Array.isArray(settings.defaultTools)
    || settings.defaultTools.some((name) => typeof name !== "string")
  ) {
    throw new Error("Invalid settings.json: defaultTools must be an array of strings");
  }
  return settings.defaultTools as string[];
}

function ensureSettingsFile(settingsPath: string): void {
  mkdirSync(dirname(settingsPath), { recursive: true });
  if (existsSync(settingsPath)) return;
  try {
    writeFileSync(settingsPath, "{}", { flag: "wx", mode: 0o600 });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
  }
}

async function withSettingsLock<T>(
  settingsPath: string,
  run: () => T | Promise<T>,
): Promise<T> {
  ensureSettingsFile(settingsPath);
  const release = await lockfile.lock(settingsPath, { realpath: false, retries: 10 });
  try {
    return await run();
  } finally {
    await release();
  }
}

/**
 * Read the stored switch.
 *
 * A missing settings file means "not configured", which is the auto-detect
 * path rather than "PowerShell off".
 */
export async function readPowerShellToolEnabled(
  settingsPath = getPowerShellSettingsPath(),
  platform: NodeJS.Platform = process.platform,
): Promise<boolean> {
  if (!existsSync(settingsPath)) return false;
  return withSettingsLock(settingsPath, () =>
    isPowerShellToolEnabled(configuredTools(parseSettings(settingsPath)), platform));
}

export async function writePowerShellToolEnabled(
  enabled: boolean,
  settingsPath = getPowerShellSettingsPath(),
  platform: NodeJS.Platform = process.platform,
): Promise<boolean> {
  if (platform !== "win32") throw new Error("PowerShell tool settings are only available on Windows");

  return withSettingsLock(settingsPath, () => {
    const settings = withSettingsSchema(parseSettings(settingsPath));
    const currentTools = configuredTools(settings) ?? DEFAULT_TOOLS;
    const nextTools = replaceShellTool(currentTools, enabled);
    // A tool list that has no shell slot at all still needs one, otherwise the
    // switch silently does nothing.
    if (!currentTools.some((name) => SHELL_TOOLS.has(name))) {
      nextTools.push(enabled ? "powershell" : "bash");
    }
    settings.defaultTools = nextTools;
    writeFileSync(settingsPath, JSON.stringify(settings, null, 2), "utf8");
    chmodSync(settingsPath, 0o600);
    return enabled;
  });
}
