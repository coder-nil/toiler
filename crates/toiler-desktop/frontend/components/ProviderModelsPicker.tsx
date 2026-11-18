"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { RefreshCw } from "lucide-react";
import { useI18n } from "@/hooks/useI18n";
import { refreshProviderModels } from "@/lib/model-discovery";
import { EnabledModelsPicker } from "./EnabledModelsPicker";

type RefreshState =
  | { phase: "idle" }
  | { phase: "loading" }
  | { phase: "success"; count: number }
  | { phase: "error"; message: string };

/** API Key 与订阅详情共用，刷新图标始终紧跟已选数量。 */
export function ProviderModelsPicker({ providerId, configured, busy = false, reloadKey = 0, onChanged, onRefresh }: {
  providerId: string;
  configured: boolean;
  busy?: boolean;
  reloadKey?: string | number;
  onChanged?: () => void;
  onRefresh?: () => void;
}) {
  const { t } = useI18n();
  const [state, setState] = useState<RefreshState>({ phase: "idle" });
  const [refreshTick, setRefreshTick] = useState(0);
  const abortRef = useRef<AbortController | null>(null);

  useEffect(() => {
    setState({ phase: "idle" });
    return () => {
      abortRef.current?.abort();
      abortRef.current = null;
    };
  }, [providerId, configured, reloadKey]);

  const refresh = useCallback(async () => {
    if (busy || abortRef.current) return;
    const controller = new AbortController();
    abortRef.current = controller;
    setState({ phase: "loading" });
    try {
      const models = await refreshProviderModels(providerId, fetch, controller.signal);
      if (controller.signal.aborted) return;
      setState({ phase: "success", count: models.length });
      setRefreshTick((tick) => tick + 1);
      onRefresh?.();
      onChanged?.();
    } catch (error) {
      if (!controller.signal.aborted) {
        setState({ phase: "error", message: error instanceof Error ? error.message : String(error) });
      }
    } finally {
      if (abortRef.current === controller) abortRef.current = null;
    }
  }, [busy, onChanged, onRefresh, providerId]);

  // 未配置凭据时不禁用：让服务端回一句具体的「先登录 / 先保存 Key」，比一颗点不动的图标清楚。
  const disabled = busy || state.phase === "loading";
  return (
    <div style={{ borderTop: "1px solid var(--border)", paddingTop: 14 }}>
      {state.phase === "success" && (
        <p role="status" style={{ margin: "-2px 0 10px", fontSize: 11, color: "#16a34a" }}>{t("models.providerModelsRefreshed", { count: state.count })}</p>
      )}
      {state.phase === "error" && (
        <p role="alert" style={{ margin: "-2px 0 10px", fontSize: 11, color: "#f87171", lineHeight: 1.4 }}>{state.message}</p>
      )}
      <EnabledModelsPicker
        providerId={providerId}
        disabled={state.phase === "loading"}
        reloadKey={`${configured}:${reloadKey}:${refreshTick}`}
        onChanged={onChanged}
        countAction={(
          <button
            type="button"
            onClick={() => void refresh()}
            title={state.phase === "loading" ? t("models.refreshingProviderModels") : t("models.refreshProviderModels")}
            aria-label={t("models.refreshProviderModels")}
            aria-busy={state.phase === "loading"}
            disabled={disabled}
            style={{ display: "inline-flex", alignItems: "center", justifyContent: "center", width: 24, height: 24, padding: 0, border: "none", borderRadius: 4, background: "transparent", color: configured ? "var(--text-muted)" : "var(--text-dim)", cursor: disabled ? "not-allowed" : "pointer", flexShrink: 0 }}
          >
            <RefreshCw size={14} strokeWidth={1.8} aria-hidden="true" className={state.phase === "loading" ? "animate-spin" : undefined} />
          </button>
        )}
      />
    </div>
  );
}
