/**
 * 「这台手机该走局域网还是走公网」的判定。
 *
 * 为什么判定必须在服务端：公网入口是 HTTPS，而局域网地址是 HTTP。HTTPS 页面里发起
 * 的 fetch / img / iframe 只要指向 http://192.168.x.x 就会被浏览器的混合内容策略整条
 * 拦掉（Chrome 还有 Private Network Access 预检、iOS 18 起 Safari 有本地网络授权），
 * 所以「先探测局域网、探不通再走公网」在客户端根本走不通。只有**顶层导航**不受此限制。
 *
 * 于是流程是：二维码只放一个稳定入口（公网域名）→ 服务端比对手机与桌面的出口 IP →
 * 同网就返回一个极小的跳转页，由客户端做一次顶层导航切到局域网。
 */

export type MobileRoute = "lan" | "public";

export interface MobileRouteInput {
  /** 发请求的客户端 IP；来源见 clientIpFromHeaders()。null 表示拿不到。 */
  clientIp: string | null;
  /** 桌面端自己的出口公网 IP（v4/v6，任一命中即视为同网）。 */
  desktopIps: readonly string[];
  /** 判定为同网时要切过去的局域网地址；没有（局域网关闭 / 没地址）就不切。 */
  lanUrl: string | null;
  /** 请求本身已经带着 lan=1（用户按返回键回来选的「用外网继续」）。 */
  alreadyLocal: boolean;
  /** `subnet`：IPv4 同 /24、IPv6 同 /64；`exact`：必须完全相等（IPv6 仍按 /64）。 */
  mode?: "subnet" | "exact";
}

export interface MobileRouteDecision {
  route: MobileRoute;
  /** 非 null 时就是那个跳转页要去的地址。 */
  redirectTo: string | null;
}

/** IPv4-mapped IPv6（`::ffff:192.168.1.5`）按 IPv4 处理。 */
export function normalizeAddress(value: string): string {
  const trimmed = value.trim().toLowerCase();
  const mapped = /^::ffff:(\d{1,3}(?:\.\d{1,3}){3})$/.exec(trimmed);
  return mapped ? mapped[1] : trimmed;
}

function ipv4Parts(value: string): number[] | null {
  const parts = value.split(".");
  if (parts.length !== 4) return null;
  const numbers = parts.map((part) => (/^\d{1,3}$/.test(part) ? Number(part) : -1));
  return numbers.every((part) => part >= 0 && part <= 255) ? numbers : null;
}

function ipv6Groups(value: string): string[] | null {
  if (!value.includes(":")) return null;
  const [head, tail] = value.split("::");
  const headGroups = head ? head.split(":").filter(Boolean) : [];
  const tailGroups = tail !== undefined && tail ? tail.split(":").filter(Boolean) : [];
  if (value.includes("::")) {
    const missing = 8 - headGroups.length - tailGroups.length;
    if (missing < 0) return null;
    return [...headGroups, ...Array(missing).fill("0"), ...tailGroups];
  }
  return headGroups.length === 8 ? headGroups : null;
}

/** 私网/回环/链路本地/CGNAT：说明请求本来就是从内网进来的，不必再比对出口 IP。 */
export function isPrivateAddress(value: string): boolean {
  const address = normalizeAddress(value);
  const v4 = ipv4Parts(address);
  if (v4) {
    const [a, b] = v4;
    if (a === 10 || a === 127) return true;
    if (a === 172 && b >= 16 && b <= 31) return true;
    if (a === 192 && b === 168) return true;
    if (a === 169 && b === 254) return true;
    // CGNAT（Tailscale 也用这一段）
    if (a === 100 && b >= 64 && b <= 127) return true;
    return false;
  }
  const groups = ipv6Groups(address);
  if (!groups) return false;
  const [first] = groups;
  if (first === "::1" || address === "::1") return true;
  const numeric = Number.parseInt(first || "0", 16);
  if (Number.isNaN(numeric)) return false;
  if ((numeric & 0xfe00) === 0xfc00) return true; // fc00::/7 唯一本地地址
  if ((numeric & 0xffc0) === 0xfe80) return true; // fe80::/10 链路本地
  return false;
}

/** 两个地址是否属于同一个网络（IPv4 按 /24，IPv6 按 /64；exact 模式下必须相等）。 */
export function sameNetwork(left: string, right: string, mode: "subnet" | "exact" = "subnet"): boolean {
  const a = normalizeAddress(left);
  const b = normalizeAddress(right);
  if (!a || !b) return false;

  const v4a = ipv4Parts(a);
  const v4b = ipv4Parts(b);
  if (v4a && v4b) {
    if (mode === "exact") return a === b;
    return v4a[0] === v4b[0] && v4a[1] === v4b[1] && v4a[2] === v4b[2];
  }
  if (v4a || v4b) return false; // 一侧 v4 一侧 v6：判不出来

  const g6a = ipv6Groups(a);
  const g6b = ipv6Groups(b);
  if (!g6a || !g6b) return a === b;
  const prefix = (group: string) => Number.parseInt(group || "0", 16);
  return g6a.slice(0, 4).every((group, index) => prefix(group) === prefix(g6b[index]));
}

/**
 * 从请求头里取客户端 IP。
 *
 * `CF-Connecting-IP` 由 Cloudflare 边缘写入，经隧道进来的请求无法伪造它；本机的
 * Rust 代理逐字节转发，所以这个头能原样到达。
 */
export function clientIpFromHeaders(get: (name: string) => string | null): string | null {
  for (const header of ["cf-connecting-ip", "x-pi-client-ip", "x-real-ip"]) {
    const value = get(header)?.trim();
    if (value) return normalizeAddress(value);
  }
  return null;
}

/**
 * 判定结果。
 *
 * 默认方向是「公网」：判不出来时公网一定可达，这是失败代价最小的一侧；
 * 只有明确同网才切局域网。
 */
export function resolveMobileRoute(input: MobileRouteInput): MobileRouteDecision {
  if (input.alreadyLocal) return { route: "lan", redirectTo: null };
  const { clientIp, lanUrl } = input;
  if (!clientIp || !lanUrl) return { route: "public", redirectTo: null };
  // 请求本来就是从内网来的（手机直连局域网地址）：就留在局域网。
  if (isPrivateAddress(clientIp)) return { route: "lan", redirectTo: null };

  const mode = input.mode ?? "subnet";
  const same = input.desktopIps.some((ip) => sameNetwork(clientIp, ip, mode));
  return same ? { route: "lan", redirectTo: lanUrl } : { route: "public", redirectTo: null };
}
