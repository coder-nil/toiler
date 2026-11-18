/* 只同步原生材质主题与页面就绪状态；背景采样由 WindowServer 连续完成。 */
(function () {
  if (!window.__TOILER_OVERLAY_TITLEBAR__) return;
  var lastDark;
  function sync() {
    var root = document.documentElement;
    if (!root || !window.__TAURI_INTERNALS__) return;
    var dark = root.classList.contains("dark");
    if (lastDark === dark) return;
    lastDark = dark;
    window.__TAURI_INTERNALS__.invoke("sync_titlebar_appearance", { dark: dark }).catch(function (error) {
      lastDark = undefined;
      console.warn("同步原生标题栏主题失败", error);
    });
  }
  function start() {
    document.documentElement.classList.add("pi-overlay-chrome");
    new MutationObserver(sync).observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class"],
    });
    sync();
  }
  window.addEventListener("pi-app-ready", function () {
    lastDark = undefined;
    sync();
  });
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start, { once: true });
  } else {
    start();
  }
})();
