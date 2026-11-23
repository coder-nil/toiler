export function subscribeToWindowForeground(onForeground: () => void, target = window): () => void {
  const { document } = target;
  let frame: number | null = null;
  const schedule = () => {
    if (document.visibilityState !== "visible" || frame !== null) return;
    // 恢复窗口时可能同时收到可见性变化和焦点事件，只刷新一次。
    frame = target.requestAnimationFrame(() => {
      frame = null;
      if (document.visibilityState === "visible") onForeground();
    });
  };
  document.addEventListener("visibilitychange", schedule);
  target.addEventListener("focus", schedule);
  return () => {
    document.removeEventListener("visibilitychange", schedule);
    target.removeEventListener("focus", schedule);
    if (frame !== null) target.cancelAnimationFrame(frame);
  };
}
