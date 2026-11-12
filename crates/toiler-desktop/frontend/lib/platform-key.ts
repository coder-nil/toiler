/**
 * 修饰键（Ctrl 还是 Cmd）在界面上的显示名。
 *
 * 只用于**文案**：真正的按键判断一律同时接受 metaKey 与 ctrlKey，所以这里判断错了
 * 也只会让提示文字不好看，不会让快捷键失效。服务端没有 `navigator`，因此首帧一律
 * 回落到 `Ctrl`，客户端再用 effect 修正（避免 hydration 属性不一致）。
 */
export type ModifierKeyLabel = "Cmd" | "Ctrl";

export function isMacPlatform(): boolean {
  if (typeof navigator === "undefined") return false;
  const platform = navigator.platform || "";
  const userAgent = navigator.userAgent || "";
  return /mac|iphone|ipad|ipod/i.test(platform) || /mac os x|iphone|ipad/i.test(userAgent);
}

export function modifierKeyLabel(): ModifierKeyLabel {
  return isMacPlatform() ? "Cmd" : "Ctrl";
}
