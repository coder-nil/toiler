import { readFileSync, readdirSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

/**
 * 自动发现「这个隧道已经绑了哪个域名」。
 *
 * cloudflared 没有「列出当前隧道绑定了哪些主机名」这种命令，但**登录证书本身就是
 * 一个作用域受限的 API token**：`~/.cloudflared/cert.pem` 是 PEM 包着的
 * `{ zoneID, accountID, apiToken }`。用它查一次 DNS 记录，就能把指向本隧道的
 * CNAME 反推出来 —— 于是用户只要跑过一次 `cloudflared tunnel login` 并建过路由，
 * 就再也不需要在界面里手填域名。
 *
 * 全部失败路径都返回 null：发现不到域名只是退回「手填一次」，不该让设置页报错。
 */

export interface OriginToken {
  zoneID: string;
  accountID: string;
  apiToken: string;
}

const TUNNEL_ID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const SUCCESS_TTL_MS = 5 * 60 * 1000;
const FAILURE_TTL_MS = 60 * 1000;

export function cloudflaredHome(): string {
  return join(homedir(), ".cloudflared");
}

/** 解析 `cloudflared tunnel login` 生成的 ARGO TUNNEL TOKEN。 */
export function parseOriginToken(pem: string): OriginToken | null {
  const body = pem
    .split(/\r?\n/)
    .filter((line) => !line.startsWith("-----"))
    .join("")
    .trim();
  if (!body) return null;
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(Buffer.from(body, "base64").toString("utf-8")) as Record<string, unknown>;
  } catch {
    return null;
  }
  const zoneID = typeof parsed.zoneID === "string" ? parsed.zoneID : null;
  const accountID = typeof parsed.accountID === "string" ? parsed.accountID : null;
  const apiToken = typeof parsed.apiToken === "string" ? parsed.apiToken : null;
  return zoneID && accountID && apiToken ? { zoneID, accountID, apiToken } : null;
}

/** 隧道 id：`~/.cloudflared/<uuid>.json` 的文件名就是它。多个时取最近写入的。 */
export function findTunnelId(directory = cloudflaredHome()): string | null {
  let entries: string[];
  try {
    entries = readdirSync(directory);
  } catch {
    return null;
  }
  const candidates = entries
    .filter((name) => name.endsWith(".json") && TUNNEL_ID_PATTERN.test(name.slice(0, -5)))
    .map((name) => {
      let mtime = 0;
      try {
        mtime = statSync(join(directory, name)).mtimeMs;
      } catch {
        mtime = 0;
      }
      return { id: name.slice(0, -5), mtime };
    })
    .sort((left, right) => right.mtime - left.mtime);
  return candidates[0]?.id ?? null;
}

/** 在 DNS 记录里找指向该隧道的 CNAME，返回它的主机名（punycode）。 */
export function findTunnelHostname(records: unknown, tunnelId: string): string | null {
  if (!Array.isArray(records)) return null;
  const target = `${tunnelId}.cfargotunnel.com`.toLowerCase();
  for (const record of records) {
    if (typeof record !== "object" || record === null) continue;
    const entry = record as Record<string, unknown>;
    if (entry.type !== "CNAME") continue;
    const content = typeof entry.content === "string" ? entry.content.toLowerCase() : "";
    const name = typeof entry.name === "string" ? entry.name.replace(/\.$/, "") : "";
    if (content === target && name) return name.toLowerCase();
  }
  return null;
}

let cache: { hostname: string | null; expiresAt: number } | null = null;

/** 仅测试用：清掉进程内缓存。 */
export function resetTunnelHostnameCache(): void {
  cache = null;
}

export interface DiscoverOptions {
  directory?: string;
  fetchImpl?: typeof fetch;
  force?: boolean;
  now?: number;
}

async function detect(directory: string, doFetch: typeof fetch): Promise<string | null> {
  let pem: string;
  try {
    pem = readFileSync(join(directory, "cert.pem"), "utf-8");
  } catch {
    return null;
  }
  const token = parseOriginToken(pem);
  if (!token) return null;
  const tunnelId = findTunnelId(directory);
  if (!tunnelId) return null;

  try {
    const response = await doFetch(
      `https://api.cloudflare.com/client/v4/zones/${token.zoneID}/dns_records?type=CNAME&per_page=100`,
      { headers: { Authorization: `Bearer ${token.apiToken}` } },
    );
    if (!response.ok) return null;
    const payload = await response.json() as { result?: unknown };
    return findTunnelHostname(payload.result, tunnelId);
  } catch {
    return null;
  }
}

/**
 * 发现本隧道绑定的域名。成功缓存 5 分钟，失败缓存 1 分钟（避免每次开设置页都打
 * Cloudflare，也避免发现不到时反复重试）。
 */
export async function discoverTunnelHostname(options: DiscoverOptions = {}): Promise<string | null> {
  const now = options.now ?? Date.now();
  if (!options.force && cache && cache.expiresAt > now) return cache.hostname;

  const directory = options.directory ?? cloudflaredHome();
  const hostname = await detect(directory, options.fetchImpl ?? fetch);
  cache = { hostname, expiresAt: now + (hostname ? SUCCESS_TTL_MS : FAILURE_TTL_MS) };
  return hostname;
}
