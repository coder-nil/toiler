/**
 * 打开外部链接（登录授权页、设备码验证页等）。
 *
 * 为什么不能直接 `window.open`：
 *   - **桌面端（Tauri / macOS WebView）根本不创建子窗口**，`window.open` 是
 *     静默无效的（`components/AppShell.tsx` 里导出页面那条链路踩过同一个坑）。所以
 *     打包版里点「登录」不会有任何反应，必须交给 Rust 侧用系统浏览器打开。
 *   - 浏览器里这些调用发生在 SSE 回调里（没有用户手势），大概率被当作弹窗拦截。
 *
 * 结论：调用方除了调用本函数，**还必须把 URL 以可复制的文本展示出来**，
 * 否则桌面端一旦 invoke 失败、浏览器一旦拦截弹窗，用户就无路可走。
 */

declare global {
  interface Window {
    __PI_WEB_API_ORIGIN__?: string;
  }
}

/** 只允许 http/https：登录链路上不该出现 file:/javascript: 之类协议。 */
export function isOpenableExternalUrl(url: string): boolean {
  if (!url) return false;
  try {
    const parsed = new URL(url);
    return parsed.protocol === "https:" || parsed.protocol === "http:";
  } catch {
    return false;
  }
}

export type OpenExternalUrlResult = "desktop" | "window" | "blocked" | "copied" | "invalid";

/** 只取判断拦截时真正要用到的字段，方便在非 DOM 环境里测试。 */
export interface ExternalLinkClickEvent {
  defaultPrevented: boolean;
  button: number;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  preventDefault(): void;
}

/**
 * Markdown 里的外部链接点击：桌面端拦下并交给系统浏览器，浏览器里保留原生行为。
 *
 * 返回 true 表示已接管（调用方不用再做别的）。纯浏览器里返回 false，让
 * `target="_blank"` / 修饰键 / 中键的原生语义（新标签、后台打开）继续生效。
 */
export function takeOverExternalLink(event: ExternalLinkClickEvent, url: string): boolean {
  if (event.defaultPrevented || event.button !== 0) return false;
  if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return false;
  if (!isOpenableExternalUrl(url)) return false;
  if (typeof window === "undefined" || !window.__PI_WEB_API_ORIGIN__) return false;
  event.preventDefault();
  void openExternalUrl(url);
  return true;
}

/**
 * 尽力用「能真正看到浏览器」的方式打开链接。
 *
 * 返回打开方式，便于调用方决定要不要补一句提示；任何情况都不抛异常
 * （登录流程不该被一个打不开的链接打断）。
 *
 * 桌面端 invoke 失败时（旧版二进制没这个命令、系统打开器失败等）退回
 * **复制链接**：复制成功比静默无操作强，用户能自己粘到浏览器。
 */
export async function openExternalUrl(url: string): Promise<OpenExternalUrlResult> {
  if (!isOpenableExternalUrl(url)) return "invalid";

  if (typeof window !== "undefined" && window.__PI_WEB_API_ORIGIN__) {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("open_external_url", { url });
      return "desktop";
    } catch {
      // 走下面：先试着把链接交给用户，再试 window.open。
      if (await copyToClipboard(url)) return "copied";
    }
  }

  if (typeof window === "undefined") return "blocked";
  try {
    const opened = window.open(url, "_blank", "noopener,noreferrer");
    return opened ? "window" : "blocked";
  } catch {
    return "blocked";
  }
}

async function copyToClipboard(url: string): Promise<boolean> {
  try {
    const { copyText } = await import("@/lib/clipboard");
    await copyText(url);
    return true;
  } catch {
    return false;
  }
}
