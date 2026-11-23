/* 依赖桌面壳注入而不是 React，完整历史等独立页也能使用同一套快捷键。 */
(function () {
  if (!window.__TOILER_DESKTOP__) return;
  var mac = window.__TOILER_OVERLAY_TITLEBAR__;
  var pending = Promise.resolve();

  function adjust(action) {
    if (!window.__TAURI_INTERNALS__) return;
    /* 连续按键与重置按发生顺序执行，不能让过期回复覆盖红绿灯避让。 */
    pending = pending.then(function () {
      return window.__TAURI_INTERNALS__.invoke("adjust_interface_zoom", { action: action });
    }).then(function (percent) {
      if (mac) document.documentElement.style.setProperty("--toiler-interface-scale", String(percent / 100));
    }).catch(function (error) {
      console.warn("调整界面倍率失败", error);
    });
  }

  window.addEventListener("keydown", function (event) {
    if (event.defaultPrevented || event.isComposing || event.altKey ||
        (mac ? !event.metaKey || event.ctrlKey : !event.ctrlKey || event.metaKey)) return;
    var key = event.key;
    var action;
    if (key === "+" || key === "=") action = "in";
    else if (!event.shiftKey && key === "-") action = "out";
    else if (!event.shiftKey && key === "0") action = "reset";
    if (!action || !window.__TAURI_INTERNALS__) return;
    event.preventDefault();
    event.stopPropagation();
    adjust(action);
  }, true);

  /* 原生倍率可能在跨文档导航时重置；后退从页面缓存恢复时也同步留白。 */
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", function () { adjust("restore"); }, { once: true });
  } else {
    adjust("restore");
  }
  window.addEventListener("pageshow", function (event) {
    if (event.persisted) adjust("restore");
  });
})();
