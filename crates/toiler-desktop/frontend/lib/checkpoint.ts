import { createHash, randomUUID } from "crypto";
import { promises as fs } from "fs";
import type { Dirent } from "fs";
import path from "path";

const SKIP = new Set([".git", "node_modules", ".next", "target"]);
const MAX_FILES = 10_000;
const MAX_FILE_BYTES = 20 * 1024 * 1024;

type FileRecord = { path: string; hash: string; mode: number; data?: string };
export type Checkpoint = {
  id: string; sessionId: string; cwd: string; entryId?: string; createdAt: string;
  before: FileRecord[]; after?: FileRecord[];
};

function rootDir(): string {
  return path.join(process.env.PI_AGENT_DIR || path.join(process.env.HOME || process.env.USERPROFILE || ".", ".pi", "agent"), "checkpoints");
}
function filePath(sessionId: string, id: string): string { return path.join(rootDir(), encodeURIComponent(sessionId), `${id}.json`); }
function digest(data: Buffer): string { return createHash("sha256").update(data).digest("hex"); }

export async function expireOtherCheckpoints(sessionId: string): Promise<number> {
  const root = rootDir();
  const currentSessionDir = encodeURIComponent(sessionId);
  let entries: Dirent[];
  try {
    entries = await fs.readdir(root, { withFileTypes: true });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return 0;
    throw error;
  }

  const expired = entries.filter((entry) => entry.isDirectory() && entry.name !== currentSessionDir);
  await Promise.all(expired.map((entry) => fs.rm(path.join(root, entry.name), { recursive: true, force: true })));
  return expired.length;
}

async function walk(cwd: string): Promise<FileRecord[]> {
  const out: FileRecord[] = [];
  async function visit(dir: string): Promise<void> {
    if (out.length >= MAX_FILES) throw new Error("Workspace has too many files for a checkpoint");
    for (const entry of await fs.readdir(dir, { withFileTypes: true })) {
      if (SKIP.has(entry.name)) continue;
      const absolute = path.join(dir, entry.name);
      if (entry.isDirectory()) { await visit(absolute); continue; }
      if (!entry.isFile()) continue;
      const stat = await fs.stat(absolute);
      if (stat.size > MAX_FILE_BYTES) continue;
      const data = await fs.readFile(absolute);
      out.push({ path: path.relative(cwd, absolute).split(path.sep).join("/"), hash: digest(data), mode: stat.mode & 0o777, data: data.toString("base64") });
    }
  }
  await visit(cwd);
  return out.sort((a, b) => a.path.localeCompare(b.path));
}

async function save(cp: Checkpoint): Promise<void> {
  await fs.mkdir(path.dirname(filePath(cp.sessionId, cp.id)), { recursive: true });
  await fs.writeFile(filePath(cp.sessionId, cp.id), JSON.stringify(cp));
}
export async function createCheckpoint(sessionId: string, cwd: string): Promise<Checkpoint> {
  const cp: Checkpoint = { id: randomUUID(), sessionId, cwd, createdAt: new Date().toISOString(), before: await walk(cwd) };
  await save(cp); return cp;
}
export async function finalizeCheckpoint(sessionId: string, id: string, entryId: string | undefined): Promise<Checkpoint> {
  const file = filePath(sessionId, id); const cp = JSON.parse(await fs.readFile(file, "utf8")) as Checkpoint;
  cp.after = await walk(cp.cwd); if (entryId) cp.entryId = entryId; await save(cp); return cp;
}
export async function listCheckpoints(sessionId: string): Promise<Checkpoint[]> {
  const dir = path.join(rootDir(), encodeURIComponent(sessionId));
  try { const names = await fs.readdir(dir); return (await Promise.all(names.filter((n) => n.endsWith(".json")).map(async (n) => JSON.parse(await fs.readFile(path.join(dir, n), "utf8")) as Checkpoint))).sort((a, b) => a.createdAt.localeCompare(b.createdAt)); } catch { return []; }
}
export async function restoreCheckpoint(sessionId: string, id: string): Promise<{ restored: string[] }> {
  const cp = JSON.parse(await fs.readFile(filePath(sessionId, id), "utf8")) as Checkpoint;
  if (!cp.after) throw new Error("Checkpoint is not finalized");
  const later = (await listCheckpoints(sessionId)).filter((candidate) => candidate.after && candidate.createdAt > cp.createdAt).at(-1);
  const current = await walk(cp.cwd); const expected = new Map(cp.after.map((f) => [f.path, f.hash]));
  const actual = new Map(current.map((f) => [f.path, f.hash]));
  const changed = [...new Set([...expected.keys(), ...actual.keys()])].filter((p) => expected.get(p) !== actual.get(p));
  if (changed.length) throw new Error(`Workspace changed since this checkpoint: ${changed.slice(0, 8).join(", ")}${changed.length > 8 ? "…" : ""}`);
  if (later) {
    const latest = new Map(later.after!.map((f) => [f.path, f.hash]));
    const diverged = [...new Set([...latest.keys(), ...actual.keys()])].filter((p) => latest.get(p) !== actual.get(p));
    if (diverged.length) throw new Error("此会话已有更新的 checkpoint，且工作区状态不再是最新状态，已拒绝恢复");
  }
  const before = new Map(cp.before.map((f) => [f.path, f]));
  for (const record of current) if (!before.has(record.path)) await fs.rm(path.join(cp.cwd, record.path));
  for (const [relative, record] of before) {
    const target = path.join(cp.cwd, relative); await fs.mkdir(path.dirname(target), { recursive: true });
    await fs.writeFile(target, Buffer.from(record.data || "", "base64")); await fs.chmod(target, record.mode);
  }
  return { restored: [...new Set([...before.keys(), ...current.filter((f) => !before.has(f.path)).map((f) => f.path)])] };
}
