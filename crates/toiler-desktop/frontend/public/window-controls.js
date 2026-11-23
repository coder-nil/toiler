/* 标题栏由壳注入，不能依赖 React：会话导出页是独立 HTML，也需要窗口控制。 */
(function () {
  if (!window.__TOILER_CUSTOM_TITLEBAR__) return;
  function mount() {
    document.documentElement.classList.add("toiler-custom-titlebar");
    var style = document.createElement("style");
    var inset = ".toiler-custom-titlebar:not(:has(.right-panel-open)) .app-topbar,.toiler-custom-titlebar .right-panel-open .app-file-panel-titlebar{box-sizing:border-box;padding-right:138px}";
    /* 窗口按钮高于普通界面浮层（500），但必须被模态蒙版（1000 起）遮挡并拦截点击。 */
    style.textContent = inset + ".toiler-window-controls{--toiler-control-color:#1f1f1f;position:fixed;top:0;right:0;display:flex;height:42px;z-index:900;background:var(--bg-chrome,var(--bg,#202020));color:var(--toiler-control-color);user-select:none}.dark .toiler-window-controls{--toiler-control-color:#fff}.toiler-window-controls button{display:flex;align-items:center;justify-content:center;width:46px;border:0;background:transparent;color:inherit;cursor:default;transition:background-color 120ms ease,color 120ms ease}.toiler-window-controls button:hover{background:color-mix(in srgb,currentColor 10%,transparent)}.dark .toiler-window-controls button:hover{background:color-mix(in srgb,currentColor 12%,transparent)}.toiler-window-controls .close:hover{background:#c42b1c;color:#fff}.toiler-window-controls button:active{background:color-mix(in srgb,currentColor 18%,transparent)}.dark .toiler-window-controls button:active{background:color-mix(in srgb,currentColor 20%,transparent)}.toiler-window-controls .close:active{background:#a82016;color:#fff}.toiler-window-controls button:focus-visible{outline:2px solid var(--accent,#58a6ff);outline-offset:-2px}";
    document.head.appendChild(style);
    var controls = document.createElement("div");
    controls.className = "toiler-window-controls";
    controls.setAttribute("data-tauri-drag-region", "false");
    function invoke(command) {
      return window.__TAURI_INTERNALS__.invoke("plugin:window|" + command, { label: "main" });
    }
    function button(label, icon, command, className) {
      var element = document.createElement("button");
      element.type = "button";
      element.title = label;
      element.setAttribute("aria-label", label);
      element.className = className || "";
      element.innerHTML = '<svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" aria-hidden="true"><path d="' + icon + '"/></svg>';
      element.addEventListener("click", function () {
        invoke(command).then(sync).catch(console.error);
      });
      controls.appendChild(element);
      return element;
    }
    button("最小化", "M1 6h10", "minimize");
    var maximize = button("最大化", "M1 1h10v10H1z", "toggle_maximize");
    button("关闭", "m1 1 10 10M11 1 1 11", "close", "close");
    function sync() {
      return invoke("is_maximized").then(function (maximized) {
        var label = maximized ? "还原" : "最大化";
        maximize.title = label;
        maximize.setAttribute("aria-label", label);
        maximize.querySelector("path").setAttribute("d", maximized ? "M4 3V1h7v7H9M1 4h7v7H1z" : "M1 1h10v10H1z");
      });
    }
    window.addEventListener("resize", function () { sync().catch(console.error); });
    document.body.appendChild(controls);
    sync().catch(console.error);
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", mount, { once: true });
  else mount();
})();
