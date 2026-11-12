import { createCipheriv, createDecipheriv, createHash, randomBytes } from "node:crypto";
import { execFile } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { promisify } from "node:util";
import { getAgentDir } from "@earendil-works/pi-coding-agent";
import { writePrivateFileAtomicSync } from "./atomic-file";

const execFileAsync = promisify(execFile);
const CREDENTIAL_FILE = "git-credentials.json";
const KEYCHAIN_SERVICE = "pi-desktop-git-credentials";
const KEYCHAIN_ACCOUNT = "encryption-key";

export type GitCredentialKind = "https" | "ssh" | "none";

export interface GitCredential {
  kind: Exclude<GitCredentialKind, "none">;
  username?: string;
  secret: string;
}

interface EncryptedValue {
  iv: string;
  tag: string;
  ciphertext: string;
}

interface CredentialFile {
  version: 1;
  entries: Record<string, EncryptedValue>;
}

export function gitCredentialKind(remote: string | null): GitCredentialKind {
  if (!remote) return "none";
  if (/^https:\/\//i.test(remote)) return "https";
  if (/^(?:ssh:\/\/|[^@\s/:]+@[^:\s]+:)/i.test(remote)) return "ssh";
  return "none";
}

function remoteKey(remote: string): string {
  // A remote URL can contain a username. It is not needed to locate an entry.
  const normalized = remote.replace(/^(https?:\/\/)[^/@]+@/i, "$1").replace(/\/+$/, "");
  return createHash("sha256").update(normalized).digest("hex");
}

function credentialPath(): string {
  return join(getAgentDir(), CREDENTIAL_FILE);
}

function readCredentialFile(): CredentialFile {
  const filePath = credentialPath();
  if (!existsSync(filePath)) return { version: 1, entries: {} };
  const parsed: unknown = JSON.parse(readFileSync(filePath, "utf8"));
  if (!parsed || typeof parsed !== "object" || (parsed as { version?: unknown }).version !== 1) {
    throw new Error("Invalid Git credential store");
  }
  const entries = (parsed as { entries?: unknown }).entries;
  if (!entries || typeof entries !== "object" || Array.isArray(entries)) throw new Error("Invalid Git credential store");
  return { version: 1, entries: entries as Record<string, EncryptedValue> };
}

async function encryptionKey(): Promise<Buffer> {
  // The macOS Keychain keeps the data-encryption key outside the credential file.
  // Other platforms deliberately do not persist Git secrets until equivalent secure
  // storage is available, rather than falling back to a recoverable local key.
  if (process.platform !== "darwin") throw new Error("Secure credential storage is unavailable on this platform");
  try {
    const { stdout } = await execFileAsync("security", ["find-generic-password", "-a", KEYCHAIN_ACCOUNT, "-s", KEYCHAIN_SERVICE, "-w"]);
    const key = Buffer.from(stdout.trim(), "base64");
    if (key.length !== 32) throw new Error("Invalid Git credential encryption key");
    return key;
  } catch (error) {
    if ((error as { code?: unknown }).code !== 44) throw error;
  }

  const key = randomBytes(32);
  await execFileAsync("security", ["add-generic-password", "-U", "-a", KEYCHAIN_ACCOUNT, "-s", KEYCHAIN_SERVICE, "-w", key.toString("base64")]);
  return key;
}

function encrypt(value: GitCredential, key: Buffer): EncryptedValue {
  const iv = randomBytes(12);
  const cipher = createCipheriv("aes-256-gcm", key, iv);
  const ciphertext = Buffer.concat([cipher.update(JSON.stringify(value), "utf8"), cipher.final()]);
  return { iv: iv.toString("base64"), tag: cipher.getAuthTag().toString("base64"), ciphertext: ciphertext.toString("base64") };
}

function decrypt(value: EncryptedValue, key: Buffer): GitCredential | null {
  try {
    const decipher = createDecipheriv("aes-256-gcm", key, Buffer.from(value.iv, "base64"));
    decipher.setAuthTag(Buffer.from(value.tag, "base64"));
    const parsed: unknown = JSON.parse(Buffer.concat([decipher.update(Buffer.from(value.ciphertext, "base64")), decipher.final()]).toString("utf8"));
    if (!parsed || typeof parsed !== "object") return null;
    const credential = parsed as Partial<GitCredential>;
    if ((credential.kind !== "https" && credential.kind !== "ssh") || typeof credential.secret !== "string" || !credential.secret) return null;
    if (credential.kind === "https" && typeof credential.username !== "string") return null;
    return credential as GitCredential;
  } catch {
    return null;
  }
}

export async function loadGitCredential(remote: string | null): Promise<GitCredential | null> {
  const kind = gitCredentialKind(remote);
  if (!remote || kind === "none") return null;
  const entry = readCredentialFile().entries[remoteKey(remote)];
  if (!entry) return null;
  const credential = decrypt(entry, await encryptionKey());
  return credential?.kind === kind ? credential : null;
}

export async function saveGitCredential(remote: string, credential: GitCredential): Promise<void> {
  if (gitCredentialKind(remote) !== credential.kind) throw new Error("Credential type does not match the remote URL");
  const agentDir = getAgentDir();
  if (!existsSync(agentDir)) mkdirSync(agentDir, { recursive: true, mode: 0o700 });
  const file = readCredentialFile();
  file.entries[remoteKey(remote)] = encrypt(credential, await encryptionKey());
  writePrivateFileAtomicSync(credentialPath(), JSON.stringify(file));
  chmodSync(credentialPath(), 0o600);
}
