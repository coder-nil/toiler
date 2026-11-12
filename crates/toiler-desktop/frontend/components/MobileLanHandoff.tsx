"use client";

import { useEffect, useState } from "react";
import { useI18n } from "@/hooks/useI18n";

const ATTEMPT_KEY = "pi-lan-handoff-target";

/**
 * 公网入口 → 局域网地址的一次性跳转页。
 *
 * 二维码只放一个稳定入口（公网域名），是否切局域网由服务端判定（见 lib/mobile-route.ts）。
 * 这一页就是「判定为同网」时返回的东西：它只做一次顶层导航。
 *
 * 几个刻意的设计：
 *   * **不用 302** —— 302 会把这一页从浏览历史里抹掉，判定失误时用户没有退路；
 *   * 用 `location.href` 保留历史，并用 sessionStorage 记住「这次已经试过了」，
 *     避免用户按返回键回来时又被弹一次（死循环）；
 *   * 「用外网继续」的链接**始终渲染**：iOS Safari 的 bfcache 恢复页面时不会重跑
 *     effect，如果只靠状态切换，用户返回后会卡在「正在切换…」上。
 */
export function MobileLanHandoff({ target, stayUrl }: { target: string; stayUrl: string }) {
  const { t } = useI18n();
  const [retried, setRetried] = useState(false);

  useEffect(() => {
    let attempted = false;
    try {
      attempted = window.sessionStorage.getItem(ATTEMPT_KEY) === target;
      if (!attempted) window.sessionStorage.setItem(ATTEMPT_KEY, target);
    } catch {
      // 隐私模式下 sessionStorage 不可用：跳一次，退路交给浏览器的返回键。
    }
    if (attempted) {
      setRetried(true);
      return;
    }
    window.location.href = target;
  }, [target]);

  return (
    <div
      style={{
        minHeight: "100dvh",
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: 10,
        padding: 24,
        background: "var(--bg, #111)",
        color: "var(--text, #eee)",
        fontFamily: "-apple-system, BlinkMacSystemFont, sans-serif",
        textAlign: "center",
      }}
    >
      <span style={{ fontSize: 14 }}>{t("mobile.lanSwitchTitle")}</span>
      <code style={{ fontSize: 12, color: "var(--text-muted, #999)", overflowWrap: "anywhere" }}>
        {target}
      </code>
      {retried && (
        <span style={{ fontSize: 12, color: "var(--text-muted, #999)", lineHeight: 1.7, maxWidth: 320 }}>
          {t("mobile.lanSwitchFallbackHint")}
        </span>
      )}
      <a
        href={stayUrl}
        style={{
          marginTop: 6,
          padding: "8px 14px",
          borderRadius: 8,
          border: "1px solid var(--border, #333)",
          color: "var(--text, #eee)",
          fontSize: 13,
          textDecoration: "none",
        }}
      >
        {t("mobile.lanSwitchFallbackAction")}
      </a>
    </div>
  );
}
