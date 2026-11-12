import { networkInterfaces } from "node:os";
import { readDesktopLanAccess, readDesktopLanRuntime } from "@/lib/desktop-access";
import { pickLanAddress } from "@/lib/mobile-pair";

/**
 * 局域网入口的 origin（`http://192.168.x.x:<lanPort>`）。
 *
 * 手机访问本地入口代理时用的就是它；判定为「同网」时，服务端返回的跳转页会切到
 * 这个 origin 下的 `/m`。没有开手机访问、代理还没监听、或网卡上没有可用的私网
 * 地址时返回 null —— 此时判定退化为「一直走公网」。
 */
export function resolveLanOrigin(): string | null {
  const access = readDesktopLanAccess();
  const runtime = readDesktopLanRuntime();
  if (!access.enabled || runtime.lanPort === null) return null;
  const host = pickLanAddress(networkInterfaces());
  return host ? `http://${host}:${runtime.lanPort}` : null;
}
