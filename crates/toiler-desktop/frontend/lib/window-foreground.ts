import { listen, TauriEvent } from "@tauri-apps/api/event";

export function subscribeToWindowForeground(
  onForeground: () => void,
  target = window,
  listenNative = listen,
): () => void {
  const { document } = target;
  let frame: number | null = null;
  let disposed = false;
  let unlistenNative: (() => void) | undefined;
  const cancelFrame = () => {
    if (frame === null) return;
    target.cancelAnimationFrame(frame);
    frame = null;
  };
  const schedule = () => {
    if (unlistenNative || document.visibilityState !== "visible" || frame !== null) return;
    // 浏览器可能同时收到可见性变化和焦点事件，只刷新一次。
    frame = target.requestAnimationFrame(() => {
      frame = null;
      if (document.visibilityState === "visible") onForeground();
    });
  };
  document.addEventListener("visibilitychange", schedule);
  target.addEventListener("focus", schedule);
  if (target.__PI_WEB_DESKTOP__) {
    // 原生窗口重新聚焦时，WebView 不一定会收到 DOM 焦点或可见性事件。
    // 原生信号不等页面绘制；如果监听失败，仍保留 DOM 事件作为兜底。
    void listenNative(TauriEvent.WINDOW_FOCUS, () => {
      if (disposed) return;
      cancelFrame();
      onForeground();
    }, { target: { kind: "Window", label: "main" } }).then((unlisten) => {
      if (disposed) return unlisten();
      unlistenNative = unlisten;
    }).catch(() => undefined);
  }
  return () => {
    disposed = true;
    document.removeEventListener("visibilitychange", schedule);
    target.removeEventListener("focus", schedule);
    cancelFrame();
    void Promise.resolve(unlistenNative?.()).catch(() => undefined);
  };
}
