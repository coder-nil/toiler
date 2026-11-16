/** 手机访问接口的公开配置；文件读写与隧道管理全部由 Rust 服务负责。 */
export type DesktopTunnelMode = "quick" | "named";

export interface DesktopPublicAccess {
  enabled: boolean;
  mode: DesktopTunnelMode;
  hostname: string | null;
}

export type DesktopTunnelState =
  | "disabled"
  | "needs-proxy"
  | "missing-binary"
  | "needs-login"
  | "starting"
  | "running"
  | "error";

export interface DesktopTunnelRuntime {
  state: DesktopTunnelState;
  mode: DesktopTunnelMode;
  url: string | null;
  hostname: string | null;
  pid: number | null;
  restarts: number;
  lastError: string | null;
  logTail: string[];
  updatedAtMs: number | null;
}
