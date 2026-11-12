"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { QRCodeSVG } from "qrcode.react";
import { useI18n } from "@/hooks/useI18n";
import { copyText } from "@/lib/clipboard";
import type { DesktopPublicAccess, DesktopTunnelRuntime } from "@/lib/desktop-access";

type AccessState =
  | { phase: "loading" }
  | { phase: "error"; message: string }
  | {
      phase: "ready";
      enabled: boolean;
      password: string | null;
      lanPort: number | null;
      minPasswordLength: number;
      desktopShell: boolean;
      public: DesktopPublicAccess;
      tunnel: DesktopTunnelRuntime;
    };

interface AccessPayload {
  enabled?: boolean;
  password?: string | null;
  lanPort?: number | null;
  minPasswordLength?: number;
  desktopShell?: boolean;
  public?: DesktopPublicAccess;
  /** 只有固定域名模式才用得上，界面已不再展示。 */
  publicHostnameDisplay?: string | null;
  publicHostnameDetected?: string | null;
  tunnel?: DesktopTunnelRuntime;
  error?: string;
  code?: string;
}

const SAVED_PASSWORD_MASK = "••••••••";

const EMPTY_TUNNEL: DesktopTunnelRuntime = {
  state: "disabled",
  mode: "quick",
  url: null,
  hostname: null,
  pid: null,
  restarts: 0,
  lastError: null,
  logTail: [],
  updatedAtMs: null,
};

const TUNNEL_STATUS_KEY: Record<DesktopTunnelRuntime["state"], string> = {
  "disabled": "settings.mobilePublicOff",
  "needs-proxy": "settings.mobilePublicNeedsProxy",
  "missing-binary": "settings.mobilePublicMissingBinary",
  "needs-login": "settings.mobilePublicNeedsLogin",
  "starting": "settings.mobilePublicStarting",
  "running": "settings.mobilePublicRunning",
  "error": "settings.mobilePublicError",
};

/**
 * 设置 → 手机访问。
 *
 * 这里只负责把开关、密码与公网入口写进配置文件；真正监听局域网的是桌面壳里的
 * 反向代理，真正对外的隧道也在桌面壳里，两者都热加载这份配置，所以改动不需要
 * 重启任何东西，也不会打断正在运行的任务。
 */
export function MobileAccessSettings() {
  const { t } = useI18n();
  const [state, setState] = useState<AccessState>({ phase: "loading" });
  const [lanUrl, setLanUrl] = useState<string | null>(null);
  const [publicUrl, setPublicUrl] = useState<string | null>(null);
  const [passwordDraft, setPasswordDraft] = useState("");
  const [passwordDirty, setPasswordDirty] = useState(false);
  const [showPassword, setShowPassword] = useState(false);
  const [saving, setSaving] = useState(false);
  const [notice, setNotice] = useState<{ kind: "ok" | "error"; text: string } | null>(null);
  // 轮询会反复同步后端状态，但用户正在输入时不能覆盖草稿。
  const passwordDirtyRef = useRef(false);

  const resetPasswordDraft = useCallback(() => {
    passwordDirtyRef.current = false;
    setPasswordDirty(false);
    setPasswordDraft("");
    setShowPassword(false);
  }, []);

  const applyPayload = useCallback((data: AccessPayload) => {
    const ready: AccessState = {
      phase: "ready",
      enabled: data.enabled ?? false,
      password: data.password ?? null,
      lanPort: data.lanPort ?? null,
      minPasswordLength: data.minPasswordLength ?? 8,
      desktopShell: data.desktopShell ?? false,
      public: data.public ?? { enabled: false, mode: "quick", hostname: null },
      tunnel: data.tunnel ?? EMPTY_TUNNEL,
    };
    setState(ready);
    // 已保存的密码只以固定掩码展示；明文不进入输入框，也不参与下一次提交。
    if (!passwordDirtyRef.current) {
      setPasswordDraft("");
      setShowPassword(false);
    }

    return ready;
  }, []);

  const load = useCallback(async () => {
    try {
      const [accessResponse, pairResponse] = await Promise.all([
        fetch("/api/mobile/access"),
        fetch("/api/mobile/pair"),
      ]);
      const data = await accessResponse.json() as AccessPayload;
      const pair = await pairResponse.json() as { url?: string | null; publicUrl?: string | null };
      setLanUrl(pair.url ?? null);
      setPublicUrl(pair.publicUrl ?? null);
      if (!accessResponse.ok || data.error) {
        setState({ phase: "error", message: data.error ?? `HTTP ${accessResponse.status}` });
        return;
      }
      applyPayload(data);
    } catch (error) {
      setState({ phase: "error", message: error instanceof Error ? error.message : String(error) });
    }
  }, [applyPayload]);

  useEffect(() => {
    void load();
  }, [load]);

  // 代理收到新配置到真正监听之间有一个很短的窗口，轮询一次把端口补上。
  useEffect(() => {
    if (state.phase !== "ready" || !state.enabled || state.lanPort !== null) return;
    const timer = window.setTimeout(() => { void load(); }, 1200);
    return () => window.clearTimeout(timer);
  }, [state, load]);

  // 隧道从 starting 到 running 才能拿到公网地址，所以启动期间也轮询一次；
  // 稳定态（running / 各类阻塞）就停下，别一直敲自己的 API。
  useEffect(() => {
    if (state.phase !== "ready" || !state.public.enabled) return;
    if (state.tunnel.state !== "starting" && state.tunnel.state !== "disabled") return;
    const timer = window.setTimeout(() => { void load(); }, 1500);
    return () => window.clearTimeout(timer);
  }, [state, load]);

  const errorText = useCallback((data: AccessPayload, fallbackMin: number) => {
    switch (data.code) {
      case "password-required":
        return t("settings.mobilePasswordRequired");
      case "too-short":
        return t("settings.mobilePasswordTooShort", { count: fallbackMin });
      default:
        return data.error ?? "HTTP error";
    }
  }, [t]);

  const save = useCallback(async (enabled: boolean) => {
    setSaving(true);
    setNotice(null);
    const requestPassword = passwordDirty ? passwordDraft : "";
    try {
      const response = await fetch("/api/mobile/access", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ enabled, password: requestPassword }),
      });
      const data = await response.json() as AccessPayload;
      if (!response.ok || data.error) {
        setNotice({
          kind: "error",
          text: errorText(data, state.phase === "ready" ? state.minPasswordLength : 8),
        });
        return;
      }
      applyPayload(data);
      resetPasswordDraft();
      setNotice({ kind: "ok", text: t("settings.mobileSaved") });
    } catch (error) {
      setNotice({ kind: "error", text: error instanceof Error ? error.message : String(error) });
    } finally {
      setSaving(false);
    }
  }, [applyPayload, errorText, passwordDirty, passwordDraft, resetPasswordDraft, state, t]);

  /**
   * 公网入口的开关。界面不再让用户选地址形式，所以这里每次都把模式归一化回临时
   * 地址（quick，trycloudflare）—— 否则配置文件里残留的固定域名会让面板上没有
   * 任何控件能改回来。PUT 时带上局域网开关与密码草稿，避免互相覆盖。
   */
  const savePublic = useCallback(async (enabled: boolean) => {
    if (state.phase !== "ready") return;
    setSaving(true);
    setNotice(null);
    const requestPassword = passwordDirty ? passwordDraft : "";
    try {
      const response = await fetch("/api/mobile/access", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          enabled: state.enabled,
          password: requestPassword,
          public: { enabled, mode: "quick", hostname: "" },
        }),
      });
      const data = await response.json() as AccessPayload;
      if (!response.ok || data.error) {
        setNotice({ kind: "error", text: errorText(data, state.minPasswordLength) });
        return;
      }
      const ready = applyPayload(data);
      resetPasswordDraft();
      // 刚打开时壳里还没写下运行态，先把状态顶成 starting，界面不会闪回「已关闭」。
      if (ready.public.enabled && ready.tunnel.state === "disabled") {
        setState({ ...ready, tunnel: { ...ready.tunnel, state: "starting" } });
      }
      setNotice({ kind: "ok", text: t("settings.mobileSaved") });
    } catch (error) {
      setNotice({ kind: "error", text: error instanceof Error ? error.message : String(error) });
    } finally {
      setSaving(false);
    }
  }, [applyPayload, errorText, passwordDirty, passwordDraft, resetPasswordDraft, state, t]);

  if (state.phase === "loading") {
    return <div style={{ color: "var(--text-muted)", fontSize: 12 }}>{t("settings.mobileLoading")}</div>;
  }
  if (state.phase === "error") {
    return <div style={{ color: "#f87171", fontSize: 12, overflowWrap: "anywhere" }}>{state.message}</div>;
  }

  const hasSavedPassword = state.password !== null;
  const showingSavedPasswordMask = hasSavedPassword && !passwordDirty;
  const passwordTooShort = passwordDirty
    && passwordDraft.trim().length > 0
    && passwordDraft.trim().length < state.minPasswordLength;
  const canEnable = hasSavedPassword || passwordDraft.trim().length >= state.minPasswordLength;
  const tunnelUrl = state.tunnel.state === "running" ? publicUrl ?? state.tunnel.url : null;
  const tunnelStatus = t(TUNNEL_STATUS_KEY[state.tunnel.state]);
  // 入口只有一份：公网隧道在跑就用公网地址（哪儿都能扫到），否则回落到局域网地址。
  const entryUrl = tunnelUrl ?? lanUrl;
  const entryIsPublic = tunnelUrl !== null;

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16, maxWidth: 620 }}>
      <p style={{ margin: 0, color: "var(--text-muted)", fontSize: 12, lineHeight: 1.6 }}>
        {t("settings.mobileBody")}
      </p>

      {!state.desktopShell && (
        <div style={{ padding: "9px 11px", border: "1px solid rgba(245,158,11,.35)", borderRadius: 7, color: "var(--text-muted)", fontSize: 11, lineHeight: 1.6 }}>
          {t("settings.mobileCliOnly")}
        </div>
      )}

      <label style={{ display: "flex", alignItems: "center", gap: 9, cursor: "pointer" }}>
        <input
          type="checkbox"
          checked={state.enabled}
          disabled={saving || (!state.enabled && !canEnable)}
          onChange={(event) => void save(event.target.checked)}
          style={{ margin: 0, accentColor: "var(--accent)" }}
        />
        <span style={{ fontSize: 12, color: "var(--text)" }}>{t("settings.mobileEnable")}</span>
        {!state.enabled && !canEnable && (
          <span style={{ fontSize: 11, color: "var(--text-dim)" }}>{t("settings.mobilePasswordFirst")}</span>
        )}
      </label>

      <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
        <span style={{ fontSize: 11, color: "var(--text-muted)" }}>{t("settings.mobilePassword")}</span>
        <div style={{ display: "flex", gap: 6 }}>
          <div style={{ position: "relative", flex: 1, minWidth: 0 }}>
            <input
              type={showingSavedPasswordMask || showPassword ? "text" : "password"}
              value={showingSavedPasswordMask ? SAVED_PASSWORD_MASK : passwordDraft}
              onChange={(event) => {
                passwordDirtyRef.current = true;
                setPasswordDirty(true);
                setPasswordDraft(event.target.value);
              }}
              onFocus={(event) => {
                if (showingSavedPasswordMask) event.currentTarget.select();
              }}
              onBlur={() => {
                if (passwordDirty && !passwordDraft.trim()) resetPasswordDraft();
              }}
              placeholder={t("settings.mobilePasswordPlaceholder", { count: state.minPasswordLength })}
              autoComplete="new-password"
              spellCheck={false}
              style={{
                width: "100%", height: 34, padding: "0 34px 0 10px",
                border: "1px solid var(--border)", borderRadius: 6,
                background: "var(--bg-panel)", color: "var(--text)",
                fontSize: 12, fontFamily: "var(--font-mono)", outline: "none",
              }}
            />
            {!showingSavedPasswordMask && <button
              type="button"
              onClick={() => setShowPassword((value) => !value)}
              aria-label={showPassword ? t("i18n.hideDetails") : t("i18n.showDetails")}
              style={{ position: "absolute", right: 5, top: "50%", transform: "translateY(-50%)", width: 24, height: 24, display: "flex", alignItems: "center", justifyContent: "center", padding: 0, border: "none", background: "none", color: "var(--text-dim)", cursor: "pointer" }}
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                {showPassword
                  ? <><path d="M17.94 17.94A10.94 10.94 0 0 1 12 20C7 20 2.73 16.89 1 12a18.45 18.45 0 0 1 5.06-6.94" /><path d="M9.9 4.24A10.94 10.94 0 0 1 12 4c5 0 9.27 3.11 11 8a18.5 18.5 0 0 1-2.16 3.19" /><path d="M14.12 14.12A3 3 0 0 1 9.88 9.88" /><path d="M1 1l22 22" /></>
                  : <><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8S1 12 1 12Z" /><circle cx="12" cy="12" r="3" /></>}
              </svg>
            </button>}
          </div>
          <button
            type="button"
            onClick={() => void save(state.enabled)}
            disabled={saving || passwordTooShort}
            style={{
              flexShrink: 0, height: 34, padding: "0 12px", border: "none", borderRadius: 6,
              background: saving || passwordTooShort ? "var(--bg-panel)" : "var(--accent)",
              color: saving || passwordTooShort ? "var(--text-dim)" : "#fff",
              cursor: saving || passwordTooShort ? "default" : "pointer",
              fontSize: 12, fontWeight: 600,
            }}
          >
            {saving ? t("i18n.saving") : t("i18n.save")}
          </button>
        </div>
        <span style={{ fontSize: 10, color: passwordTooShort ? "#f87171" : "var(--text-dim)" }}>
          {t("settings.mobilePasswordHint", { count: state.minPasswordLength })}
        </span>
      </div>

      {notice && <span style={{ fontSize: 11, color: notice.kind === "ok" ? "#16a34a" : "#f87171" }}>{notice.text}</span>}

      {/* 局域网与公网通往的是同一个遥控页，所以入口只给一份：地址、二维码、复制按钮。
          外网开关就挂在这份入口下面 —— 它只是给同一个入口再加一条公网通道。 */}
      <div style={{ borderTop: "1px solid var(--border)", paddingTop: 14, display: "flex", flexDirection: "column", gap: 10 }}>
        <label style={{ display: "flex", alignItems: "center", gap: 9, cursor: state.enabled && canEnable ? "pointer" : "default" }}>
          <input
            type="checkbox"
            checked={state.public.enabled}
            disabled={saving || !state.enabled || !canEnable}
            onChange={(event) => void savePublic(event.target.checked)}
            style={{ margin: 0, accentColor: "var(--accent)" }}
          />
          <span style={{ fontSize: 12, color: state.enabled ? "var(--text)" : "var(--text-dim)" }}>
            {t("settings.mobilePublicEnable")}
          </span>
          {state.public.enabled && (
            <span style={{ fontSize: 11, color: state.tunnel.state === "running" ? "#16a34a" : "var(--text-muted)" }}>
              {tunnelStatus}
              {state.tunnel.restarts > 0 ? ` · ${t("settings.mobilePublicRestarts", { count: state.tunnel.restarts })}` : ""}
            </span>
          )}
          {!state.enabled && (
            <span style={{ fontSize: 11, color: "var(--text-dim)" }}>{t("settings.mobilePublicNeedsProxyHint")}</span>
          )}
        </label>

        {state.enabled && (
          state.lanPort === null || !entryUrl ? (
            <span style={{ fontSize: 11, color: "var(--text-muted)" }}>{t("settings.mobileStarting")}</span>
          ) : (
            <>
              <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
                <code style={{ fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--text-muted)", background: "var(--bg-panel)", border: "1px solid var(--border)", borderRadius: 5, padding: "6px 8px", overflowWrap: "anywhere" }}>
                  {entryUrl}
                </code>
                <button
                  type="button"
                  onClick={() => { void copyText(entryUrl); }}
                  style={{ padding: "5px 10px", border: "1px solid var(--border)", borderRadius: 5, background: "none", color: "var(--text-muted)", cursor: "pointer", fontSize: 11 }}
                >
                  {t("mobile.pairCopy")}
                </button>
              </div>
              <div style={{ alignSelf: "flex-start", padding: 10, borderRadius: 8, background: "#fff" }}>
                <QRCodeSVG value={entryUrl} size={148} level="M" marginSize={0} />
              </div>
              <span style={{ fontSize: 11, color: "var(--text-muted)", lineHeight: 1.6 }}>
                {entryIsPublic ? t("mobile.pairPublicHint") : t("settings.mobileScanHint")}
              </span>
              {state.public.enabled && state.tunnel.lastError && state.tunnel.state !== "running" && (
                <span style={{ fontSize: 10, color: "var(--text-dim)", overflowWrap: "anywhere" }}>
                  {state.tunnel.lastError}
                </span>
              )}
              {state.public.enabled && state.tunnel.logTail.length > 0 && (
                <details>
                  <summary style={{ fontSize: 10, color: "var(--text-dim)", cursor: "pointer" }}>
                    {t("settings.mobilePublicLogs")}
                  </summary>
                  <pre style={{
                    margin: "6px 0 0", padding: 8, maxHeight: 160, overflow: "auto",
                    background: "var(--bg-panel)", border: "1px solid var(--border)", borderRadius: 5,
                    color: "var(--text-muted)", fontSize: 10, lineHeight: 1.5, whiteSpace: "pre-wrap", overflowWrap: "anywhere",
                  }}>
                    {state.tunnel.logTail.slice(-10).join("\n")}
                  </pre>
                </details>
              )}
            </>
          )
        )}
      </div>
    </div>
  );
}
