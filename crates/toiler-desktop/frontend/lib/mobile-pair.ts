import type { NetworkInterfaceInfo } from "node:os";
import type { DesktopTunnelState } from "@/lib/desktop-access";

/**
 * 手机遥控页的地址推导。
 *
 * 桌面端只负责"给出一个手机能打开的地址"，不参与会话逻辑；这部分全部是纯函数，
 * 便于按网卡形态做单元测试。
 */

export type MobilePairReason =
  | "ok"
  /** 桌面壳里尚未开启手机访问（设置 → 手机访问）。 */
  | "lan-disabled"
  /** 代理刚启用，端口还没回写。 */
  | "starting"
  /** 当前服务直接绑在回环地址上（命令行 dev/start 未带 --hostname）。 */
  | "loopback-only"
  | "no-lan-address";

export interface MobilePairInfo {
  /** 手机可直接打开的遥控页地址；null 表示现在拿不到可用地址。 */
  url: string | null;
  host: string | null;
  port: string | null;
  /** 是否已经可以向局域网提供访问。 */
  lanEnabled: boolean;
  /** 是否已设置访问密码。 */
  passwordRequired: boolean;
  /** 运行在桌面壳里时提示语不同（去设置开启 vs 用 dev:lan 启动）。 */
  desktopShell: boolean;
  /** `url` 为 null 时的原因，供界面给出对应提示。 */
  reason: MobilePairReason;
  /**
   * 公网入口地址（隧道 running 时才有）。quick 模式是随机 trycloudflare 域名，
   * named 模式是固定域名；两者指向的都是同一个入口代理。
   */
  publicUrl?: string | null;
  /** 公网隧道状态，界面据此区分「没开」与「正在启动」。 */
  publicState?: DesktopTunnelState;
}

function isIpv4(value: string): boolean {
  const parts = value.split(".");
  if (parts.length !== 4) return false;
  return parts.every((part) => /^\d{1,3}$/.test(part) && Number(part) <= 255);
}

/**
 * 地址优先级：越小越优先。
 *
 * 家庭/公司 Wi-Fi 的私有网段排在最前，其次是 CGNAT 段（Tailscale 用 100.64/10），
 * 最后才是其它公网/未知地址。回环与 link-local 直接排除。
 */
export function lanAddressRank(address: string): number | null {
  if (!isIpv4(address)) return null;
  const [first, second] = address.split(".").map(Number);
  if (first === 127) return null;
  if (first === 169 && second === 254) return null;
  if (first === 192 && second === 168) return 0;
  if (first === 10) return 1;
  if (first === 172 && second >= 16 && second <= 31) return 2;
  if (first === 100 && second >= 64 && second <= 127) return 3;
  return 4;
}

/** 从网卡列表里挑一个手机最可能连上的 IPv4 地址。 */
export function pickLanAddress(
  interfaces: Record<string, readonly NetworkInterfaceInfo[] | undefined>,
): string | null {
  const candidates: { address: string; rank: number; name: string }[] = [];

  for (const [name, infos] of Object.entries(interfaces)) {
    for (const info of infos ?? []) {
      if (info.internal || info.family !== "IPv4") continue;
      const rank = lanAddressRank(info.address);
      if (rank === null) continue;
      candidates.push({ address: info.address, rank, name });
    }
  }

  candidates.sort((left, right) =>
    left.rank - right.rank
    || left.name.localeCompare(right.name)
    || left.address.localeCompare(right.address));
  return candidates[0]?.address ?? null;
}

/** 没有显式指定端口时，遥控页地址回退到的默认端口。 */
const DEFAULT_MOBILE_PORT = "30141";

/**
 * 遥控页要用的端口。
 *
 * 优先取浏览器实际访问的 Host（反向代理换过端口时以它为准），其次跟随
 * `PI_WEB_PORT` 启动参数，最后回退默认值。
 */
export function resolvePairPort(
  hostHeader: string | null,
  configuredPort?: string | null,
): string {
  const fromHeader = hostHeader?.match(/:(\d+)\s*$/)?.[1];
  if (fromHeader) return fromHeader;
  const configured = configuredPort?.trim();
  return configured || DEFAULT_MOBILE_PORT;
}

/** 拼出遥控页地址，`session` / `cwd` 会按 URL 规则编码。 */
export function buildMobileUrl(
  baseUrl: string,
  params: { session?: string; cwd?: string } = {},
): string {
  const trimmed = baseUrl.replace(/\/+$/, "");
  const query = new URLSearchParams();
  if (params.session) query.set("session", params.session);
  if (params.cwd) query.set("cwd", params.cwd);
  const suffix = query.toString();
  return `${trimmed}/m${suffix ? `?${suffix}` : ""}`;
}

/**
 * 解析手机可用的遥控地址。
 *
 * 两种形态共存：
 *   1. 桌面壳：开关 + 密码存在 `desktop-access.json`，由 Rust 反向代理向局域网提供
 *      入口（Next 自身仍然只绑回环）。
 *   2. 命令行：直接以 `--hostname 0.0.0.0` + `PI_WEB_PASSWORD` 启动，由 server 自己
 *      监听局域网。
 */
export function resolveMobilePairInfo(input: {
  bindHost: string;
  port: string;
  passwordRequired: boolean;
  desktopShell: boolean;
  interfaces: Record<string, readonly NetworkInterfaceInfo[] | undefined>;
  lanAccess?: { enabled: boolean; lanPort: number | null };
  session?: string;
  cwd?: string;
}): MobilePairInfo {
  const base = {
    host: null,
    port: null as string | null,
    passwordRequired: input.passwordRequired,
    desktopShell: input.desktopShell,
  };
  const query = {
    ...(input.session ? { session: input.session } : {}),
    ...(input.cwd ? { cwd: input.cwd } : {}),
  };

  if (input.lanAccess?.enabled) {
    if (input.lanAccess.lanPort === null) {
      return { ...base, url: null, lanEnabled: true, reason: "starting" };
    }
    const host = pickLanAddress(input.interfaces);
    if (!host) {
      return { ...base, url: null, lanEnabled: true, reason: "no-lan-address" };
    }
    const port = String(input.lanAccess.lanPort);
    return {
      ...base,
      host,
      port,
      url: buildMobileUrl(`http://${host}:${port}`, query),
      lanEnabled: true,
      reason: "ok",
    };
  }

  const bindHost = input.bindHost.trim();
  const boundToLan = bindHost !== ""
    && bindHost !== "127.0.0.1"
    && bindHost !== "::1"
    && bindHost !== "localhost";
  if (!boundToLan) {
    return {
      ...base,
      url: null,
      lanEnabled: false,
      reason: input.desktopShell ? "lan-disabled" : "loopback-only",
    };
  }

  const wildcard = bindHost === "0.0.0.0" || bindHost === "::";
  const host = wildcard ? pickLanAddress(input.interfaces) : bindHost;
  if (!host) {
    return { ...base, url: null, lanEnabled: true, reason: "no-lan-address" };
  }
  return {
    ...base,
    host,
    port: input.port,
    url: buildMobileUrl(`http://${host}:${input.port}`, query),
    lanEnabled: true,
    reason: "ok",
  };
}
