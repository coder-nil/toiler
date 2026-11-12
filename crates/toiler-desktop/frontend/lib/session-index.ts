import { createReadStream, existsSync, readFileSync } from "node:fs";
import { mkdir, readdir, stat, writeFile } from "node:fs/promises";
import { createInterface } from "node:readline";
import { dirname, join } from "node:path";
import { getAgentDir } from "@earendil-works/pi-coding-agent";
import { sessionPathKey } from "./session-path";

/**
 * Incremental per-file index for the session list.
 *
 * `SessionManager.listAll()` re-reads and re-parses *every byte* of every
 * session file just to fill `messageCount`, `firstMessage`, the session name
 * and the last activity time. With a few hundred sessions and half a gigabyte
 * on disk that is seconds of CPU inside the request that renders the sidebar,
 * and because it is CPU-bound it also stalls every other API call queued
 * behind it (the chat itself, git status, the model list).
 *
 * The expensive part is not knowing *which* files exist — that is a cheap
 * directory scan — but summarising each file. So the summary is cached per
 * file keyed by `size` + `mtimeMs`, in memory and on disk, and only files that
 * actually changed are parsed again. The file set still comes from a fresh
 * scan every time, so added, removed and renamed sessions stay correct.
 *
 * The summary fields and their definitions mirror the SDK's `buildSessionInfo`
 * so the sidebar keeps showing the same counts and previews as the CLI.
 */

const INDEX_VERSION = 1;
const INDEX_FILE = "desktop-session-index.json";
const PARSE_CONCURRENCY = 10;
const DISCOVERY_CONCURRENCY = 64;
const DISK_WRITE_DEBOUNCE_MS = 1500;

export interface SessionFileSummary {
  path: string;
  id: string;
  cwd: string;
  name?: string;
  parentSessionPath?: string;
  /** ISO timestamp of the session header (falls back to the file mtime). */
  created: string;
  /** ISO timestamp of the last user/assistant activity (falls back to header/mtime). */
  modified: string;
  /** Every `message` entry, system-role ones included — same rule as the SDK. */
  messageCount: number;
  firstMessage: string;
}

interface IndexEntry {
  size: number;
  mtimeMs: number;
  /** `null` marks a file that exists but is not a session transcript. */
  summary: SessionFileSummary | null;
}

declare global {
  // Stored on globalThis so a Next.js hot reload keeps the cache warm.
  var __piSessionIndexCache: Map<string, IndexEntry> | undefined;
  var __piSessionIndexDiskLoaded: boolean | undefined;
  var __piSessionIndexWriteTimer: NodeJS.Timeout | undefined;
  /** Test seam: replaces the disk scan so unit tests never read a real home directory. */
  var __piSessionIndexScanOverride: (() => Promise<SessionFileSummary[]>) | undefined;
}

function getIndexCache(): Map<string, IndexEntry> {
  if (!globalThis.__piSessionIndexCache) globalThis.__piSessionIndexCache = new Map();
  return globalThis.__piSessionIndexCache;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Reads the persisted cache once per process. A corrupt or stale-version file
 * only costs one full scan, so every failure here is swallowed.
 */
function loadDiskCache(): void {
  if (globalThis.__piSessionIndexDiskLoaded) return;
  globalThis.__piSessionIndexDiskLoaded = true;

  const file = join(getAgentDir(), INDEX_FILE);
  try {
    if (!existsSync(file)) return;
    const parsed = JSON.parse(readFileSync(file, "utf8")) as {
      version?: number;
      entries?: Record<string, IndexEntry>;
    };
    if (parsed?.version !== INDEX_VERSION || !parsed.entries) return;

    const cache = getIndexCache();
    for (const [path, entry] of Object.entries(parsed.entries)) {
      if (!entry || typeof entry.size !== "number" || typeof entry.mtimeMs !== "number") continue;
      const summary = entry.summary;
      if (summary && (typeof summary.path !== "string" || typeof summary.id !== "string")) continue;
      cache.set(sessionPathKey(path), { size: entry.size, mtimeMs: entry.mtimeMs, summary: summary ?? null });
    }
  } catch {
    /* unreadable cache: fall through to a full scan */
  }
}

function scheduleIndexWrite(): void {
  if (globalThis.__piSessionIndexWriteTimer) return;
  const timer = setTimeout(() => {
    globalThis.__piSessionIndexWriteTimer = undefined;
    void writeIndexCache();
  }, DISK_WRITE_DEBOUNCE_MS);
  timer.unref?.();
  globalThis.__piSessionIndexWriteTimer = timer;
}

async function writeIndexCache(): Promise<void> {
  const entries: Record<string, IndexEntry> = {};
  for (const [key, entry] of getIndexCache()) entries[key] = entry;

  const file = join(getAgentDir(), INDEX_FILE);
  try {
    await mkdir(dirname(file), { recursive: true });
    await writeFile(file, `${JSON.stringify({ version: INDEX_VERSION, entries })}\n`, "utf8");
  } catch {
    /* best effort: the next scan simply re-parses */
  }
}

async function mapWithConcurrency<T, R>(
  items: T[],
  limit: number,
  map: (item: T) => Promise<R>,
): Promise<R[]> {
  const results = new Array<R>(items.length);
  let nextIndex = 0;
  const worker = async () => {
    while (nextIndex < items.length) {
      const index = nextIndex;
      nextIndex += 1;
      results[index] = await map(items[index]);
    }
  };
  await Promise.all(Array.from({ length: Math.max(1, Math.min(limit, items.length)) }, () => worker()));
  return results;
}

/** Every `.jsonl` under `~/.pi/agent/sessions/<encoded-cwd>/`, as the SDK lists them. */
async function listSessionFiles(): Promise<string[]> {
  const root = join(getAgentDir(), "sessions");
  let rootEntries;
  try {
    rootEntries = await readdir(root, { withFileTypes: true });
  } catch {
    return [];
  }

  const dirs = rootEntries
    .filter((entry) => entry.isDirectory() || entry.isSymbolicLink())
    .map((entry) => join(root, entry.name));

  const perDir = await mapWithConcurrency(dirs, DISCOVERY_CONCURRENCY, async (dir) => {
    try {
      return (await readdir(dir)).filter((file) => file.endsWith(".jsonl")).map((file) => join(dir, file));
    } catch {
      return [];
    }
  });
  return perDir.flat();
}

function messageText(message: Record<string, unknown>): string {
  const content = message.content;
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .filter((block): block is Record<string, unknown> => (
      isRecord(block) && block.type === "text" && typeof block.text === "string"
    ))
    .map((block) => block.text as string)
    .join(" ");
}

function messageActivityTime(entry: Record<string, unknown>, message: Record<string, unknown>): number | undefined {
  if (typeof message.timestamp === "number") return message.timestamp;
  if (typeof entry.timestamp !== "string") return undefined;
  const parsed = Date.parse(entry.timestamp);
  return Number.isNaN(parsed) ? undefined : parsed;
}

function isoOrFallback(value: unknown, fallbackMs: number): string {
  if (typeof value === "string") {
    const parsed = Date.parse(value);
    if (!Number.isNaN(parsed)) return new Date(parsed).toISOString();
  }
  return new Date(fallbackMs).toISOString();
}

/**
 * Summarises one transcript. Returns `null` when the file is not a session
 * (the first entry is not a `session` header) or cannot be read.
 */
async function summarizeSessionFile(filePath: string, mtimeMs: number): Promise<SessionFileSummary | null> {
  let header: Record<string, unknown> | null = null;
  let messageCount = 0;
  let firstMessage = "";
  let name: string | undefined;
  let lastActivityTime: number | undefined;

  const stream = createReadStream(filePath, { encoding: "utf8" });
  const lines = createInterface({ input: stream, crlfDelay: Infinity });
  try {
    for await (const line of lines) {
      if (!line) continue;
      let entry: unknown;
      try {
        entry = JSON.parse(line);
      } catch {
        continue;
      }
      if (!isRecord(entry)) continue;

      if (!header) {
        if (entry.type !== "session") return null;
        header = entry;
        continue;
      }
      if (entry.type === "session_info") {
        // The latest explicit name wins, including a cleared one.
        const nextName = typeof entry.name === "string" ? entry.name.trim() : "";
        name = nextName || undefined;
        continue;
      }
      if (entry.type !== "message") continue;

      messageCount += 1;
      const message = entry.message;
      if (!isRecord(message) || typeof message.role !== "string") continue;
      if (message.role !== "user" && message.role !== "assistant") continue;

      const activity = messageActivityTime(entry, message);
      if (typeof activity === "number") lastActivityTime = Math.max(lastActivityTime ?? 0, activity);

      if (message.role !== "user" || firstMessage) continue;
      const text = messageText(message);
      if (text) firstMessage = text;
    }
  } catch {
    return null;
  } finally {
    lines.close();
    stream.destroy();
  }

  if (!header) return null;

  const headerTime = typeof header.timestamp === "string" ? Date.parse(header.timestamp) : NaN;
  const modified = typeof lastActivityTime === "number" && lastActivityTime > 0
    ? new Date(lastActivityTime).toISOString()
    : isoOrFallback(header.timestamp, mtimeMs);

  return {
    path: filePath,
    id: typeof header.id === "string" ? header.id : "",
    cwd: typeof header.cwd === "string" ? header.cwd : "",
    ...(name ? { name } : {}),
    ...(typeof header.parentSession === "string" ? { parentSessionPath: header.parentSession } : {}),
    created: Number.isNaN(headerTime) ? new Date(mtimeMs).toISOString() : new Date(headerTime).toISOString(),
    modified,
    messageCount,
    firstMessage: firstMessage || "(no messages)",
  };
}

/**
 * Summaries for every session transcript on disk. Unchanged files (same size
 * and mtime) come straight from the cache, so a warm scan costs one `stat` per
 * file instead of a full parse.
 */
export async function scanSessionFiles(): Promise<SessionFileSummary[]> {
  const override = globalThis.__piSessionIndexScanOverride;
  if (override) return override();

  loadDiskCache();
  const cache = getIndexCache();
  const files = await listSessionFiles();
  const seen = new Set<string>();

  const summaries = await mapWithConcurrency(files, PARSE_CONCURRENCY, async (filePath) => {
    const key = sessionPathKey(filePath);
    seen.add(key);

    let stats;
    try {
      stats = await stat(filePath);
    } catch {
      return null;
    }

    const cached = cache.get(key);
    if (cached && cached.size === stats.size && cached.mtimeMs === stats.mtimeMs) {
      return cached.summary;
    }

    const summary = await summarizeSessionFile(filePath, stats.mtimeMs);
    cache.set(key, { size: stats.size, mtimeMs: stats.mtimeMs, summary });
    return summary;
  });

  // Deleted or moved files must not keep feeding the cache.
  for (const key of [...cache.keys()]) {
    if (!seen.has(key)) cache.delete(key);
  }

  scheduleIndexWrite();
  return summaries.filter((summary): summary is SessionFileSummary => summary !== null);
}

/** Drops every cached summary so the next scan re-reads all files. */
export function resetSessionIndex(): void {
  // A pending write would otherwise land against whatever agent dir is current
  // when it fires, which is not necessarily the one that was scanned.
  if (globalThis.__piSessionIndexWriteTimer) {
    clearTimeout(globalThis.__piSessionIndexWriteTimer);
    globalThis.__piSessionIndexWriteTimer = undefined;
  }
  getIndexCache().clear();
  globalThis.__piSessionIndexDiskLoaded = false;
}
