import type { DesktopTunnelState } from "./desktop-access";

export type MobilePairReason =
  | "ok"
  | "lan-disabled"
  | "starting"
  | "loopback-only"
  | "no-lan-address";

/** 地址由 Rust 服务推导，前端仅负责展示配对信息。 */
export interface MobilePairInfo {
  url: string | null;
  host: string | null;
  port: string | null;
  lanEnabled: boolean;
  passwordRequired: boolean;
  desktopShell: boolean;
  reason: MobilePairReason;
  publicUrl?: string | null;
  publicState?: DesktopTunnelState;
}
