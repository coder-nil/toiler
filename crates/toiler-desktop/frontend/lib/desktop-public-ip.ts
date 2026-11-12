/**
 * 桌面端自己的出口公网 IP —— 用来和手机的出口 IP 比对（同一个 NAT 出口 = 同一个网络，
 * 这就是「手机和电脑在不在同一个局域网」在服务端唯一可靠的信号）。
 *
 * 两条硬约束：
 *   * **永远不阻塞页面**：拿不到就用上次的值（可能为空 → 判定退化为「走公网」），
 *     并且在后台继续刷新；
 *   * 缓存 5 分钟：家宽重播、切网都会改变出口 IP，但不必每条请求都去问一遍。
 */

const TRACE_IPV4_URL = "https://cloudflare.com/cdn-cgi/trace";
const TRACE_IPV6_URL = "https://[2606:4700:4700::1111]/cdn-cgi/trace";
const SUCCESS_TTL_MS = 5 * 60 * 1000;
const FAILURE_TTL_MS = 60 * 1000;
const REQUEST_TIMEOUT_MS = 4000;
const DEFAULT_MAX_WAIT_MS = 1200;

let cache: { ips: string[]; expiresAt: number } | null = null;
let inflight: Promise<string[]> | null = null;

/** 从 cloudflare trace 的响应里取 `ip=`（纯函数，便于单测）。 */
export function parseTraceIp(body: string): string | null {
  const value = /^ip=(.+)$/m.exec(body)?.[1]?.trim();
  return value ? value : null;
}

/** 手上有的出口 IP（可能是过期值：过期值也比没有强）。 */
export function peekDesktopEgressIps(): string[] {
  return cache?.ips ?? [];
}

async function refresh(fetchImpl: typeof fetch, nowMs: number): Promise<string[]> {
  const ips: string[] = [];
  for (const url of [TRACE_IPV4_URL, TRACE_IPV6_URL]) {
    try {
      const response = await fetchImpl(url, { signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS) });
      if (!response.ok) continue;
      const ip = parseTraceIp(await response.text());
      if (ip && !ips.includes(ip)) ips.push(ip);
    } catch {
      // 取不到只是判定退化，不该影响访问本身。
    }
  }
  // 双栈机器上两条都能通；一条都不通时缩短缓存，早点再试。
  cache = { ips, expiresAt: nowMs + (ips.length > 0 ? SUCCESS_TTL_MS : FAILURE_TTL_MS) };
  return ips;
}

function ensureFresh(fetchImpl: typeof fetch, nowMs: number): Promise<string[]> {
  inflight ??= refresh(fetchImpl, nowMs).finally(() => {
    inflight = null;
  });
  return inflight;
}

/** 后台预热（设置页/配对接口打开时就调一下，等手机来扫时缓存已经热了）。 */
export function warmDesktopEgressIps(options: { fetchImpl?: typeof fetch; now?: number } = {}): void {
  const now = options.now ?? Date.now();
  if (cache && cache.expiresAt > now) return;
  void ensureFresh(options.fetchImpl ?? fetch, now);
}

/**
 * 判定用的取值：缓存还有效就直接用；否则最多等 `maxWaitMs`，超时就先用旧值，
 * 后台继续刷新（冷启动第一次扫码时不会因为查 IP 卡住页面）。
 */
export async function desktopEgressIps(
  options: { fetchImpl?: typeof fetch; now?: number; maxWaitMs?: number } = {},
): Promise<string[]> {
  const now = options.now ?? Date.now();
  if (cache && cache.expiresAt > now) return cache.ips;

  const fetchImpl = options.fetchImpl ?? fetch;
  const pending = ensureFresh(fetchImpl, now);
  const maxWaitMs = options.maxWaitMs ?? DEFAULT_MAX_WAIT_MS;
  if (maxWaitMs > 0) {
    await Promise.race([
      pending.then(() => undefined).catch(() => undefined),
      new Promise<void>((resolve) => setTimeout(resolve, maxWaitMs)),
    ]);
  }
  return peekDesktopEgressIps();
}

/** 仅测试用。 */
export function resetDesktopEgressCache(): void {
  cache = null;
  inflight = null;
}
