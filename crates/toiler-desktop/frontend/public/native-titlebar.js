/* macOS/Windows 仅在原生确认材质可用后打开透明标题带，其他平台默认实色。 */
(function () {
  var mac = window.__TOILER_OVERLAY_TITLEBAR__;
  if (!mac && !window.__TOILER_CUSTOM_TITLEBAR__) return;
  var lastDark;
  var revision = 0;
  function apply(effect) {
    var root = document.documentElement;
    if (!root) return;
    var supported = effect === "vibrancy" || effect === "acrylic";
    root.classList.toggle("toiler-native-titlebar", supported);
    root.setAttribute("data-titlebar-material", supported ? effect : "solid");
  }
  function sync(force) {
    var root = document.documentElement;
    if (!root) return;
    if (!window.__TAURI_INTERNALS__) {
      apply("solid");
      return;
    }
    var dark = root.classList.contains("dark");
    if (!force && lastDark === dark) return;
    lastDark = dark;
    var request = ++revision;
    window.__TAURI_INTERNALS__.invoke("sync_titlebar_appearance", { dark: dark }).then(function (effect) {
      if (request === revision) apply(effect);
    }).catch(function (error) {
      if (request !== revision) return;
      lastDark = undefined;
      apply("solid");
      console.warn("原生标题栏材质不可用，已回退实色", error);
    });
  }
  function start() {
    if (mac) document.documentElement.classList.add("pi-overlay-chrome");
    new MutationObserver(function () { sync(false); }).observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class"],
    });
    sync(false);
  }
  window.addEventListener("pi-app-ready", function () { sync(true); });
  /* 从系统设置切回应用时重新检查透明/高对比度选项，无需定时轮询。 */
  window.addEventListener("focus", function () { sync(true); });
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start, { once: true });
  } else {
    start();
  }
})();
