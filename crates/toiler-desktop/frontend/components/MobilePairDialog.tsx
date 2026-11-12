"use client";

import { useCallback, useEffect, useState } from "react";
import { QRCodeSVG } from "qrcode.react";
import { useI18n } from "@/hooks/useI18n";
import { copyText } from "@/lib/clipboard";
import type { MobilePairInfo } from "@/lib/mobile-pair";

type PairState =
  | { phase: "loading" }
  | { phase: "error"; message: string }
  | { phase: "ready"; info: MobilePairInfo };

/**
 * 手机遥控入口：把当前会话的遥控地址做成二维码。
 *
 * 只展示地址与启动状态，密钥一律不出现在这里 —— 手机端仍需走 HTTP Basic 认证。
 *
 * `sessionId` 是必填的：没有会话 id 时二维码只能指向「这个项目里最近的活动会话」，
 * 扫出来是别的会话，所以这种状态下不给入口（`ChatInput` 在没有 id 时不渲染按钮）。
 */
export function MobilePairDialog({
  sessionId,
  cwd,
  onClose,
}: {
  sessionId: string;
  cwd?: string;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const [state, setState] = useState<PairState>({ phase: "loading" });
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    const params = new URLSearchParams({ session: sessionId });
    if (cwd) params.set("cwd", cwd);
    const query = params.toString();
    const controller = new AbortController();

    fetch(`/api/mobile/pair${query ? `?${query}` : ""}`, { signal: controller.signal })
      .then((response) => response.json())
      .then((info: MobilePairInfo) => setState({ phase: "ready", info }))
      .catch((error: unknown) => {
        if (controller.signal.aborted) return;
        setState({ phase: "error", message: error instanceof Error ? error.message : String(error) });
      });

    return () => controller.abort();
  }, [sessionId, cwd]);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  // 二维码只放一个入口：优先公网（哪儿都能打开）。手机与服务端同网时怎么切到
  // 局域网直连由服务端在打开那一刻判定，不需要用户选 —— 见 lib/mobile-route.ts。
  const lanUrl = state.phase === "ready" ? state.info.url : null;
  const publicUrl = state.phase === "ready" ? state.info.publicUrl ?? null : null;
  const url = publicUrl ?? lanUrl;

  const handleCopy = useCallback(() => {
    if (!url) return;
    void copyText(url).then(() => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    }).catch(() => {});
  }, [url]);

  return (
    <div
      role="presentation"
      onClick={(event) => { if (event.target === event.currentTarget) onClose(); }}
      style={{ position: "fixed", inset: 0, zIndex: 1100, display: "flex", alignItems: "center", justifyContent: "center", padding: 16, background: "rgba(0,0,0,.4)" }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label={t("mobile.pairTitle")}
        style={{ width: 340, maxWidth: "100%", background: "var(--bg)", border: "1px solid var(--border)", borderRadius: 10, boxShadow: "0 12px 36px rgba(0,0,0,.24)", overflow: "hidden" }}
      >
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8, padding: "12px 14px", borderBottom: "1px solid var(--border)" }}>
          <span style={{ fontSize: 13, fontWeight: 600, color: "var(--text)" }}>{t("mobile.pairTitle")}</span>
          <button
            type="button"
            onClick={onClose}
            aria-label={t("i18n.cancel")}
            style={{ width: 24, height: 24, display: "flex", alignItems: "center", justifyContent: "center", padding: 0, border: "none", borderRadius: 5, background: "none", color: "var(--text-muted)", cursor: "pointer" }}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <line x1="18" y1="6" x2="6" y2="18" /><line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </div>

        <div style={{ padding: "14px", display: "flex", flexDirection: "column", gap: 12 }}>
          {state.phase === "loading" && (
            <p style={{ margin: 0, fontSize: 12, color: "var(--text-muted)" }}>{t("mobile.pairLoading")}</p>
          )}

          {state.phase === "error" && (
            <p style={{ margin: 0, fontSize: 12, color: "#f87171", overflowWrap: "anywhere" }}>{state.message}</p>
          )}

          {state.phase === "ready" && url && (
            <>
              <div style={{ display: "flex", justifyContent: "center" }}>
                <div style={{ padding: 10, borderRadius: 8, background: "#fff" }}>
                  <QRCodeSVG value={url} size={168} level="M" marginSize={0} />
                </div>
              </div>
              <p style={{ margin: 0, fontSize: 11, color: "var(--text-muted)", lineHeight: 1.6 }}>
                {publicUrl ? t("mobile.pairPublicHint") : t("mobile.pairScanHint")}
                {state.info.passwordRequired ? ` ${t("mobile.pairAuthHint")}` : ""}
              </p>
              {!publicUrl && (
                <p style={{ margin: 0, fontSize: 10, color: "var(--text-dim)", lineHeight: 1.6 }}>
                  {t("mobile.pairPublicOff")}
                </p>
              )}
              <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
                <code style={{ flex: 1, minWidth: 0, fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--text-muted)", background: "var(--bg-panel)", border: "1px solid var(--border)", borderRadius: 5, padding: "6px 8px", overflowWrap: "anywhere" }}>
                  {url}
                </code>
                <button
                  type="button"
                  onClick={handleCopy}
                  style={{ flexShrink: 0, padding: "6px 10px", border: "1px solid var(--border)", borderRadius: 5, background: copied ? "var(--bg-hover)" : "none", color: copied ? "#16a34a" : "var(--text-muted)", cursor: "pointer", fontSize: 11 }}
                >
                  {copied ? t("mobile.pairCopied") : t("mobile.pairCopy")}
                </button>
              </div>
            </>
          )}

          {state.phase === "ready" && !url && (
            <>
              <p style={{ margin: 0, fontSize: 12, color: "var(--text-muted)", lineHeight: 1.6 }}>
                {state.info.reason === "no-lan-address"
                  ? t("mobile.pairNoAddress")
                  : state.info.reason === "lan-disabled"
                    ? t("mobile.pairLanDisabled")
                    : state.info.reason === "starting"
                      ? t("mobile.pairStarting")
                      : t("mobile.pairLanRequired")}
              </p>
              {state.info.reason === "loopback-only" && (
                <code style={{ fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--text-muted)", background: "var(--bg-panel)", border: "1px solid var(--border)", borderRadius: 5, padding: "8px", overflowWrap: "anywhere", lineHeight: 1.6 }}>
                  {"PI_WEB_PASSWORD='…' npm run dev:lan"}
                </code>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
