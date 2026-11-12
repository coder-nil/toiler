import { sendNotification } from "@tauri-apps/plugin-notification";

declare global {
  interface Window {
    __PI_WEB_DESKTOP__?: boolean;
  }
}

type DesktopWindow = Pick<Window, "__PI_WEB_DESKTOP__">;

export interface DesktopNotification {
  title: string;
  body: string;
}

export function isDesktopShell(target: DesktopWindow | undefined = typeof window === "undefined" ? undefined : window): boolean {
  return target?.__PI_WEB_DESKTOP__ === true;
}

export function showDesktopNotification(notification: DesktopNotification): boolean {
  if (!isDesktopShell()) return false;
  sendNotification(notification);
  return true;
}
