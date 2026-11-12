import { chmodSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { getAgentDir } from "@earendil-works/pi-coding-agent";
import lockfile from "proper-lockfile";
import { withSettingsSchema } from "./settings-schema";

// 引导（steering）与续跑（follow-up）队列的投递模式是 pi 自己的设置
// （settings.json 的 `steeringMode` / `followUpMode`），不是桌面端私有偏好：
// 默认值 `one-at-a-time` 表示每完成一个 assistant turn 只投递一条排队消息，
// 因此连发多条引导会被拆到多个 turn 里陆续注入，看起来像「引导没生效」。
//
// 这里只负责读写那两个字段；让活着的 AgentSession 立刻生效由
// `set_steering_mode` / `set_follow_up_mode` RPC 命令完成（SDK 自己也会把
// 同样的值写回同一个文件，幂等）。

export type QueueMode = "all" | "one-at-a-time";

export interface QueueModes {
  steeringMode: QueueMode;
  followUpMode: QueueMode;
}

export const DEFAULT_QUEUE_MODE: QueueMode = "one-at-a-time";

const QUEUE_MODE_KEYS = ["steeringMode", "followUpMode"] as const;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** 非法值回落到默认模式，避免一个手改坏掉的 settings.json 让会话读不出模式。 */
export function parseQueueMode(value: unknown, fallback: QueueMode = DEFAULT_QUEUE_MODE): QueueMode {
  return value === "all" || value === "one-at-a-time" ? value : fallback;
}

export function getQueueModeSettingsPath(agentDir = getAgentDir()): string {
  return join(agentDir, "settings.json");
}

function parseSettings(path: string): Record<string, unknown> {
  if (!existsSync(path)) return {};
  const parsed: unknown = JSON.parse(readFileSync(path, "utf8"));
  if (!isRecord(parsed)) throw new Error("Invalid settings.json: expected an object");
  return parsed;
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

function readModes(settings: Record<string, unknown>): QueueModes {
  return {
    steeringMode: parseQueueMode(settings.steeringMode),
    followUpMode: parseQueueMode(settings.followUpMode),
  };
}

/** 缺字段等于 pi 的默认值，不是「未配置」。 */
export async function readQueueModes(settingsPath = getQueueModeSettingsPath()): Promise<QueueModes> {
  if (!existsSync(settingsPath)) return readModes({});
  return withSettingsLock(settingsPath, () => readModes(parseSettings(settingsPath)));
}

export async function writeQueueModes(
  patch: Partial<QueueModes>,
  settingsPath = getQueueModeSettingsPath(),
): Promise<QueueModes> {
  const provided = QUEUE_MODE_KEYS.filter((key) => patch[key] !== undefined);
  if (provided.length === 0) throw new Error("提供 steeringMode 或 followUpMode");

  return withSettingsLock(settingsPath, () => {
    const settings = withSettingsSchema(parseSettings(settingsPath));
    for (const key of provided) settings[key] = patch[key];
    writeFileSync(settingsPath, JSON.stringify(settings, null, 2), "utf8");
    chmodSync(settingsPath, 0o600);
    return readModes(settings);
  });
}
