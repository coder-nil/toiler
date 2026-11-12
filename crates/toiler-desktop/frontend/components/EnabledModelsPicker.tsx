"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useI18n } from "@/hooks/useI18n";

interface ProviderModelRow {
  id: string;
  name: string;
  selected: boolean;
}

type PickerState =
  | { phase: "loading" }
  | { phase: "error"; message: string }
  | { phase: "ready"; models: ProviderModelRow[]; unscoped: boolean; warnings: string[] };

interface EnabledModelsPickerProps {
  /** pi 的 provider id（内置服务商用 id，models.json 里的服务商用配置名）。 */
  providerId: string;
  /** 模型尚未保存到 models.json 时禁止勾选。 */
  disabled?: boolean;
  /** 变化时重新拉取模型列表（例如 models.json 刚保存）。 */
  reloadKey?: string | number;
  /** 勾选变化并保存成功后触发，用于刷新对话框的模型选择列表。 */
  onChanged?: () => void;
}

/**
 * 服务商详情里的「可用模型 + 勾选」区块。
 *
 * 勾选结果写入 pi 的 `enabledModels` 设置，因此对话框的模型选择列表（以及 pi
 * CLI 的 `--models` 作用域）会立即收窄到勾选的模型。整组勾选会简写成
 * `provider/*`，全部勾选则清空设置（回到“全部可见”的默认状态）。
 */
export function EnabledModelsPicker({ providerId, disabled = false, reloadKey, onChanged }: EnabledModelsPickerProps) {
  const { t } = useI18n();
  const [state, setState] = useState<PickerState>({ phase: "loading" });
  const [query, setQuery] = useState("");
  const [saving, setSaving] = useState(false);
  const [savedAt, setSavedAt] = useState(0);
  const [actionError, setActionError] = useState<string | null>(null);
  const requestIdRef = useRef(0);
  const selectedRef = useRef<Set<string>>(new Set());
  const saveChainRef = useRef<Promise<void>>(Promise.resolve());

  const load = useCallback(async (retriesLeft: number) => {
    const requestId = ++requestIdRef.current;
    try {
      const res = await fetch(`/api/models-config/enabled?provider=${encodeURIComponent(providerId)}`);
      const data = await res.json() as {
        models?: ProviderModelRow[];
        unscoped?: boolean;
        warnings?: string[];
        refreshing?: boolean;
        error?: string;
      };
      if (requestId !== requestIdRef.current) return;
      if (!res.ok || data.error || !Array.isArray(data.models)) {
        setState({ phase: "error", message: data.error ?? `HTTP ${res.status}` });
        return;
      }
      const models = data.models;
      selectedRef.current = new Set(models.filter((model) => model.selected).map((model) => model.id));
      setState({
        phase: "ready",
        models,
        unscoped: data.unscoped ?? true,
        warnings: data.warnings ?? [],
      });
      // 首次打开该服务商时模型列表可能还在后台补，稍后再取一次。
      if (models.length === 0 && data.refreshing && retriesLeft > 0) {
        window.setTimeout(() => { void load(retriesLeft - 1); }, 1500);
      }
    } catch (error) {
      if (requestId !== requestIdRef.current) return;
      setState({ phase: "error", message: error instanceof Error ? error.message : String(error) });
    }
  }, [providerId]);

  useEffect(() => {
    setState({ phase: "loading" });
    setQuery("");
    setActionError(null);
    selectedRef.current = new Set();
    void load(2);
  }, [load, reloadKey]);

  const persist = useCallback(async () => {
    setSaving(true);
    setActionError(null);
    try {
      const res = await fetch("/api/models-config/enabled", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ provider: providerId, modelIds: [...selectedRef.current] }),
      });
      const data = await res.json() as { error?: string; enabledModels?: string[] | null };
      if (!res.ok || data.error) {
        setActionError(res.status === 400 ? t("models.visibleKeepOne") : (data.error ?? `HTTP ${res.status}`));
        void load(0);
        return;
      }
      setSavedAt(Date.now());
      onChanged?.();
    } catch (error) {
      setActionError(error instanceof Error ? error.message : String(error));
    } finally {
      setSaving(false);
    }
  }, [load, onChanged, providerId, t]);

  /** 串行化保存，避免快速连点导致后一个请求覆盖前一个的选择。 */
  const queueSave = useCallback(() => {
    saveChainRef.current = saveChainRef.current
      .then(() => persist())
      .catch(() => {});
  }, [persist]);

  useEffect(() => {
    if (!savedAt) return;
    const timer = window.setTimeout(() => setSavedAt(0), 2000);
    return () => window.clearTimeout(timer);
  }, [savedAt]);

  const models = state.phase === "ready" ? state.models : [];
  const selectedCount = models.filter((model) => model.selected).length;
  const normalizedQuery = query.trim().toLocaleLowerCase();
  const visibleModels = models.filter((model) => !normalizedQuery
    || model.name.toLocaleLowerCase().includes(normalizedQuery)
    || model.id.toLocaleLowerCase().includes(normalizedQuery));

  const toggle = (id: string) => {
    if (disabled) return;
    const next = new Set(selectedRef.current);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    selectedRef.current = next;
    setState((prev) => prev.phase === "ready"
      ? { ...prev, models: prev.models.map((model) => (model.id === id ? { ...model, selected: next.has(id) } : model)) }
      : prev);
    queueSave();
  };

  const setAll = (select: boolean) => {
    if (disabled) return;
    const next = new Set(select ? models.map((model) => model.id) : []);
    selectedRef.current = next;
    setState((prev) => prev.phase === "ready"
      ? { ...prev, models: prev.models.map((model) => ({ ...model, selected: select })) }
      : prev);
    queueSave();
  };

  const statusText = state.phase === "ready" && models.length > 0
    ? (state.unscoped ? t("models.visibleUnscoped") : t("models.visibleScoped"))
    : null;

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8 }}>
        <div style={{ display: "flex", alignItems: "baseline", gap: 8, minWidth: 0 }}>
          <span style={{ fontSize: 13, fontWeight: 600, color: "var(--text)" }}>{t("models.visibleTitle")}</span>
          {models.length > 0 && (
            <span style={{ fontSize: 11, color: "var(--text-dim)" }}>
              {t("models.visibleSelectedCount", { selected: selectedCount, total: models.length })}
            </span>
          )}
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 8, flexShrink: 0 }}>
          {saving && <span style={{ fontSize: 11, color: "var(--text-dim)" }}>{t("models.visibleSaving")}</span>}
          {!saving && savedAt > 0 && <span style={{ fontSize: 11, color: "#16a34a" }}>{t("models.visibleSaved")}</span>}
          {models.length > 0 && !disabled && (
            <div style={{ display: "inline-flex", border: "1px solid var(--border)", borderRadius: 5, overflow: "hidden" }}>
              <button
                type="button"
                onClick={() => setAll(true)}
                style={{ padding: "2px 8px", border: "none", borderRight: "1px solid var(--border)", background: "transparent", color: "var(--text-muted)", cursor: "pointer", fontSize: 11 }}
              >
                {t("models.visibleShowAll")}
              </button>
              <button
                type="button"
                onClick={() => setAll(false)}
                style={{ padding: "2px 8px", border: "none", background: "transparent", color: "var(--text-muted)", cursor: "pointer", fontSize: 11 }}
              >
                {t("models.visibleHideAll")}
              </button>
            </div>
          )}
        </div>
      </div>

      {statusText && (
        <p style={{ margin: 0, fontSize: 11, color: "var(--text-muted)", lineHeight: 1.5 }}>{statusText}</p>
      )}

      {state.phase === "loading" && (
        <p style={{ margin: 0, fontSize: 11, color: "var(--text-dim)" }}>{t("models.visibleRefreshing")}</p>
      )}

      {state.phase === "error" && (
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <span style={{ fontSize: 11, color: "#f87171" }}>{state.message}</span>
          <button
            type="button"
            onClick={() => void load(0)}
            style={{ padding: "2px 8px", border: "1px solid var(--border)", borderRadius: 5, background: "none", color: "var(--text-muted)", cursor: "pointer", fontSize: 11 }}
          >
            {t("models.visibleRetry")}
          </button>
        </div>
      )}

      {state.phase === "ready" && models.length === 0 && (
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <span style={{ fontSize: 11, color: "var(--text-dim)" }}>{t("models.visibleEmpty")}</span>
          <button
            type="button"
            onClick={() => void load(1)}
            style={{ padding: "2px 8px", border: "1px solid var(--border)", borderRadius: 5, background: "none", color: "var(--text-muted)", cursor: "pointer", fontSize: 11 }}
          >
            {t("models.visibleRetry")}
          </button>
        </div>
      )}

      {state.phase === "ready" && models.length > 0 && (
        <>
          {models.length > 8 && (
            <input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder={t("models.visibleSearch")}
              aria-label={t("models.visibleSearch")}
              style={{ width: "100%", height: 30, padding: "0 9px", border: "1px solid var(--border)", borderRadius: 5, background: "var(--bg-panel)", color: "var(--text)", fontSize: 12, outline: "none" }}
            />
          )}
          <div style={{ display: "flex", flexDirection: "column", maxHeight: 260, overflowY: "auto", border: "1px solid var(--border)", borderRadius: 6 }}>
            {visibleModels.map((model) => (
              <label
                key={model.id}
                style={{ display: "flex", alignItems: "center", gap: 8, padding: "7px 10px", borderBottom: "1px solid var(--border)", cursor: disabled ? "default" : "pointer", opacity: disabled ? 0.6 : 1 }}
              >
                <input
                  type="checkbox"
                  checked={model.selected}
                  disabled={disabled}
                  onChange={() => toggle(model.id)}
                  style={{ margin: 0, accentColor: "var(--accent)", flexShrink: 0 }}
                />
                <span style={{ display: "flex", flexDirection: "column", gap: 1, minWidth: 0 }}>
                  <span style={{ fontSize: 12, color: "var(--text)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{model.name}</span>
                  {model.name !== model.id && (
                    <span style={{ fontSize: 10, color: "var(--text-dim)", fontFamily: "var(--font-mono)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{model.id}</span>
                  )}
                </span>
              </label>
            ))}
            {visibleModels.length === 0 && (
              <div style={{ padding: "8px 10px", fontSize: 11, color: "var(--text-dim)" }}>{t("models.discoveryNoMatches")}</div>
            )}
          </div>
        </>
      )}

      {state.phase === "ready" && state.warnings.length > 0 && (
        <div style={{ fontSize: 10, color: "#f59e0b", lineHeight: 1.5 }}>
          {state.warnings.map((warning) => <div key={warning}>{warning}</div>)}
        </div>
      )}

      {actionError && <p style={{ margin: 0, fontSize: 11, color: "#f87171" }}>{actionError}</p>}
    </div>
  );
}
