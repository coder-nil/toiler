import { mkdir } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";

export async function ensurePiAgentsDirectory(homeDirectory = homedir()): Promise<void> {
  await mkdir(join(homeDirectory, ".pi", "agents"), { recursive: true });
}
