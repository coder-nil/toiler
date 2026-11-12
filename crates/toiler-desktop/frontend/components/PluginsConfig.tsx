"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { sendAgentCommand } from "@/lib/agent-client";
import { useIsMobile } from "@/hooks/useIsMobile";
import type { PluginCatalogEntry } from "@/lib/plugin-catalog";
import type { PluginPackageInfo, PluginsResponse } from "@/lib/api-types";
import { useI18n } from "@/hooks/useI18n";

type PluginScope = PluginPackageInfo["scope"];
type PluginAction = "install" | "remove" | "update" | "disable" | "enable";

function shortenPath(path: string): string {
  return path.replace(/^\/(?:Users|home)\/[^/]+/, "~");
}

function normalizePluginSourceInput(value: string): string {
  const match = value.trim().match(/^\$?\s*pi\s+install\s+(\S+)\s*$/);
  return match?.[1] ?? value;
}

function packageKey(pkg: Pick<PluginPackageInfo, "source" | "scope">): string {
  return `${pkg.scope}\0${pkg.source}`;
}

function resourceSummary(pkg: PluginPackageInfo, t: ReturnType<typeof useI18n>["t"]): string {
  if (pkg.disabled) return t("i18n.disabled");
  const parts = [
    pkg.counts.extensions ? t("i18n.resourceCount", { count: pkg.counts.extensions, label: t("i18n.extensionShort") }) : "",
    pkg.counts.skills ? t("i18n.resourceCount", { count: pkg.counts.skills, label: t("i18n.skillShort") }) : "",
    pkg.counts.prompts ? t("i18n.resourceCount", { count: pkg.counts.prompts, label: t("i18n.promptShort") }) : "",
    pkg.counts.themes ? t("i18n.resourceCount", { count: pkg.counts.themes, label: t("i18n.themeShort") }) : "",
  ].filter(Boolean);
  return parts.length ? parts.join(" · ") : t("i18n.noResources");
}

function versionSummary(pkg: PluginPackageInfo, t: ReturnType<typeof useI18n>["t"]): string {
  const parts = [];
  if (pkg.version) parts.push(t("i18n.installedVersion", { version: pkg.version }));
  if (pkg.configuredVersion) parts.push(t("i18n.configuredVersion", { version: pkg.configuredVersion }));
  return parts.length ? parts.join(" · ") : t("i18n.unknown");
}

function installLocation(scope: PluginScope, cwd: string): string {
  // 后端把全局插件装到 `<toilerHome>/plugins/{npm,git}`（见 routes/plugins.rs 的头注释：
  // **不**回退 `~/.pi/agent/npm`），项目级才落 `.pi/agent/`。文案要跟实际落点一致。
  return scope === "project"
    ? `${shortenPath(cwd)}/.pi/agent/{npm,git}`
    : "~/.agents/toiler/plugins/{npm,git}";
}

function findInstalledPackage(
  packages: PluginPackageInfo[],
  source: string,
  scope: PluginScope,
): PluginPackageInfo | undefined {
  const trimmed = source.trim();
  const withoutNpmPrefix = trimmed.startsWith("npm:") ? trimmed.slice(4) : trimmed;
  return packages.find((pkg) => pkg.scope === scope && pkg.source === trimmed)
    ?? packages.find((pkg) => pkg.scope === scope && pkg.source === `npm:${withoutNpmPrefix}`)
    ?? packages.find((pkg) => pkg.scope === scope && pkg.source.endsWith(trimmed));
}

function statusColor(status: PluginPackageInfo["status"]): string {
  if (status === "loaded") return "var(--accent)";
  if (status === "installed") return "#f59e0b";
  if (status === "disabled") return "var(--text-dim)";
  return "#ef4444";
}

function ResourceList({ pkg }: { pkg: PluginPackageInfo }) {
  const { t } = useI18n();
  const groups = ([
    ["extension", t("i18n.extensions")],
    ["skill", t("i18n.skills")],
    ["prompt", t("i18n.prompts")],
    ["theme", t("i18n.themes")],
  ] as const)
    .map(([kind, label]) => ({
      kind,
      label,
      resources: pkg.resources.filter((resource) => resource.kind === kind),
    }))
    .filter((group) => group.resources.length > 0);

  if (groups.length === 0) {
    return (
      <div style={{ fontSize: 12, color: "var(--text-dim)" }}>
        {pkg.disabled ? t("i18n.packageDisabled") : t("i18n.noResolvedResources")}
      </div>
    );
  }

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 12,
      }}
    >
      {groups.map((group, groupIndex) => (
        <div
          key={group.kind}
          style={{
            borderTop: groupIndex === 0 ? "none" : "1px solid var(--border)",
            paddingTop: groupIndex === 0 ? 0 : 12,
          }}
        >
          <div
            style={{
              fontSize: 10,
              fontWeight: 700,
              color: "var(--text-dim)",
              textTransform: "uppercase",
              marginBottom: 6,
            }}
          >
            {group.label}
          </div>
          <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
            {group.resources.map((resource) => (
              <div key={`${resource.kind}:${resource.path}`} style={{ minWidth: 0 }}>
                <div
                  style={{
                    fontSize: 12,
                    color: "var(--text)",
                    fontFamily: "var(--font-mono)",
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                  }}
                  title={resource.path}
                >
                  {resource.name}
                </div>
                <div
                  style={{
                    fontSize: 10,
                    color: "var(--text-dim)",
                    fontFamily: "var(--font-mono)",
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                    marginTop: 1,
                  }}
                  title={resource.path}
                >
                  {resource.relativePath}
                </div>
              </div>
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}

function ScopeTag({ scope }: { scope: PluginScope }) {
  return (
    <span
      style={{
        fontSize: 10,
        padding: "1px 5px",
        borderRadius: 3,
        flexShrink: 0,
        background: scope === "project" ? "rgba(99,102,241,0.12)" : "rgba(120,120,120,0.12)",
        color: scope === "project" ? "rgba(99,102,241,0.85)" : "var(--text-dim)",
      }}
    >
      {scope}
    </span>
  );
}

function buttonStyle(disabled?: boolean, danger?: boolean): React.CSSProperties {
  return {
    padding: "6px 12px",
    background: danger ? "rgba(239,68,68,0.08)" : "none",
    border: "1px solid var(--border)",
    borderRadius: 6,
    color: danger ? "#ef4444" : "var(--text-muted)",
    cursor: disabled ? "not-allowed" : "pointer",
    fontSize: 12,
    opacity: disabled ? 0.5 : 1,
  };
}

function Toggle({
  enabled,
  loading,
  onToggle,
  label,
}: {
  enabled: boolean;
  loading: boolean;
  onToggle: () => void;
  label: string;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      disabled={loading}
      title={label}
      aria-label={label}
      aria-pressed={enabled}
      style={{
        flexShrink: 0,
        width: 40,
        height: 22,
        borderRadius: 11,
        border: "none",
        padding: 0,
        cursor: loading ? "wait" : "pointer",
        background: enabled ? "var(--accent)" : "var(--border)",
        position: "relative",
        transition: "background 0.18s",
        outline: "none",
        opacity: loading ? 0.65 : 1,
      }}
    >
      <span
        style={{
          position: "absolute",
          top: 3,
          left: enabled ? 21 : 3,
          width: 16,
          height: 16,
          borderRadius: "50%",
          background: "var(--bg)",
          boxShadow: "0 1px 4px rgba(0,0,0,0.22)",
          transition: "left 0.18s cubic-bezier(.4,0,.2,1)",
        }}
      />
    </button>
  );
}

function SegmentedScope({
  value,
  projectResourcesLoaded,
  onChange,
}: {
  value: PluginScope;
  projectResourcesLoaded: boolean;
  onChange: (scope: PluginScope) => void;
}) {
  const { t } = useI18n();
  return (
    <div
      style={{
        display: "inline-flex",
        border: "1px solid var(--border)",
        borderRadius: 7,
        overflow: "hidden",
        height: 30,
      }}
    >
      {(["global", "project"] as PluginScope[]).map((scope) => {
        const active = value === scope;
        const disabled = scope === "project" && !projectResourcesLoaded;
        return (
          <button
            key={scope}
            onClick={() => {
              if (!disabled) onChange(scope);
            }}
            disabled={disabled}
            title={disabled ? t("trust.projectScopeUnavailable") : undefined}
            style={{
              width: 76,
              border: "none",
              borderRight: scope === "global" ? "1px solid var(--border)" : "none",
              background: active ? "var(--bg-selected)" : "none",
              color: active ? "var(--text)" : "var(--text-muted)",
              cursor: disabled ? "not-allowed" : "pointer",
              opacity: disabled ? 0.45 : 1,
              fontSize: 12,
            }}
          >
            {scope}
          </button>
        );
      })}
    </div>
  );
}

function formatDownloadCount(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(value >= 10_000_000 ? 0 : 1)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(value >= 100_000 ? 0 : 1)}K`;
  return String(value);
}

function PluginUsageBadge({ plugin }: { plugin: PluginCatalogEntry }) {
  const { t } = useI18n();
  const monthly = plugin.monthlyDownloads;
  const weekly = plugin.weeklyDownloads;
  if (typeof monthly !== "number" && typeof weekly !== "number") return null;
  const label = typeof monthly === "number"
    ? t("i18n.pluginMonthlyInstalls", { count: formatDownloadCount(monthly) })
    : t("i18n.pluginWeeklyInstalls", { count: formatDownloadCount(weekly as number) });
  return (
    <span
      title={typeof monthly === "number"
        ? t("i18n.pluginUsageTitle", { count: monthly.toLocaleString() })
        : t("i18n.pluginUsageWeeklyTitle", { count: (weekly as number).toLocaleString() })}
      style={{
        display: "inline-flex", alignItems: "center", gap: 3, flexShrink: 0,
        padding: "1px 5px", borderRadius: 4, fontSize: 10,
        color: "var(--text-muted)", background: "var(--bg-hover)", border: "1px solid var(--border)",
      }}
    >
      <svg width="9" height="9" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <path d="M8 2v8" /><path d="M4.5 7.5 8 11l3.5-3.5" /><path d="M2.5 13.5h11" />
      </svg>
      {label}
    </span>
  );
}

function PluginCatalogPanel({
  scope,
  onScopeChange,
  projectResourcesLoaded,
  installedPackages,
  busySource,
  actionError,
  actionMessage,
  onInstall,
}: {
  scope: PluginScope;
  onScopeChange: (scope: PluginScope) => void;
  projectResourcesLoaded: boolean;
  installedPackages: PluginPackageInfo[];
  busySource: string | null;
  actionError: string | null;
  actionMessage: string | null;
  onInstall: (source: string) => void;
}) {
  const { locale, t } = useI18n();
  const [query, setQuery] = useState("");
  const [plugins, setPlugins] = useState<PluginCatalogEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [nextFrom, setNextFrom] = useState(0);
  const [hasMore, setHasMore] = useState(false);
  const [translatingNames, setTranslatingNames] = useState<Set<string>>(() => new Set());

  const translateMissingDescriptions = useCallback(async (entries: PluginCatalogEntry[], signal?: AbortSignal) => {
    const missing = entries.filter((entry) => locale === "zh-CN"
      ? entry.descriptionLanguage !== "zh-CN" && !entry.descriptionZh
      : entry.descriptionLanguage !== "en" && !entry.descriptionEn);
    if (missing.length === 0) return;
    const names = new Set(missing.map((entry) => entry.name));
    setTranslatingNames((current) => new Set([...current, ...names]));
    try {
      const response = await fetch("/api/plugins/catalog/translate", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          targetLanguage: locale,
          plugins: missing.map(({ name, description }) => ({ name, description })),
        }),
        signal,
      });
      const data = await response.json() as {
        translations?: Record<string, Pick<PluginCatalogEntry, "descriptionZh" | "descriptionZhSource" | "descriptionEn" | "descriptionEnSource">>;
      };
      if (!response.ok || !data.translations) return;
      setPlugins((current) => current.map((plugin) => {
        const translation = data.translations?.[plugin.name];
        return translation ? { ...plugin, ...translation } : plugin;
      }));
    } catch (err) {
      if (!(err instanceof DOMException && err.name === "AbortError")) {
        console.warn("Failed to translate plugin descriptions:", err);
      }
    } finally {
      setTranslatingNames((current) => new Set([...current].filter((name) => !names.has(name))));
    }
  }, [locale]);

  const load = useCallback(async (from: number, append: boolean, signal?: AbortSignal) => {
    if (append) setLoadingMore(true);
    else setLoading(true);
    setError(null);
    try {
      const params = new URLSearchParams({ q: query, from: String(from), size: "40" });
      const response = await fetch(`/api/plugins/catalog?${params.toString()}`, { signal });
      const data = await response.json() as {
        plugins?: PluginCatalogEntry[];
        from?: number;
        size?: number;
        hasMore?: boolean;
        error?: string;
      };
      if (!response.ok || data.error) throw new Error(data.error ?? `HTTP ${response.status}`);
      const incoming = data.plugins ?? [];
      setPlugins((current) => {
        if (!append) return incoming;
        const seen = new Set(current.map((plugin) => plugin.name));
        return [...current, ...incoming.filter((plugin) => !seen.has(plugin.name))];
      });
      const pageFrom = data.from ?? from;
      setNextFrom(pageFrom + (data.size ?? 40));
      setHasMore(Boolean(data.hasMore));
      void translateMissingDescriptions(incoming, signal);
    } catch (err) {
      if (err instanceof DOMException && err.name === "AbortError") return;
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (append) setLoadingMore(false);
      else setLoading(false);
    }
  }, [query, translateMissingDescriptions]);

  useEffect(() => {
    const controller = new AbortController();
    const timer = window.setTimeout(() => {
      setPlugins([]);
      setNextFrom(0);
      setHasMore(false);
      void load(0, false, controller.signal);
    }, query ? 260 : 0);
    return () => {
      window.clearTimeout(timer);
      controller.abort();
    };
  }, [load, query]);

  const isInstalled = useCallback((plugin: PluginCatalogEntry) => installedPackages.some((pkg) => (
    pkg.packageName === plugin.name || pkg.source === plugin.source || pkg.source.startsWith(`${plugin.source}@`)
  )), [installedPackages]);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 14, maxWidth: 700 }}>
      <div>
        <div style={{ fontSize: 15, fontWeight: 700, color: "var(--text)" }}>{t("i18n.discoverPlugins")}</div>
        <div style={{ fontSize: 12, color: "var(--text-dim)", marginTop: 4, lineHeight: 1.45 }}>
          {t("i18n.pluginCatalogIntro")}
        </div>
      </div>

      <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
        <span style={{ fontSize: 12, color: "var(--text-muted)" }}>{t("i18n.installTo")}</span>
        <SegmentedScope
          value={scope}
          projectResourcesLoaded={projectResourcesLoaded}
          onChange={onScopeChange}
        />
      </div>

      <div style={{ position: "relative" }}>
        <input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder={t("i18n.searchPlugins")}
          aria-label={t("i18n.searchPlugins")}
          style={{
            width: "100%",
            height: 36,
            padding: "0 11px 0 31px",
            border: "1px solid var(--border)",
            borderRadius: 6,
            background: "var(--bg-panel)",
            color: "var(--text)",
            fontSize: 13,
            outline: "none",
          }}
        />
        <span aria-hidden="true" style={{ position: "absolute", left: 10, top: 0, bottom: 0, display: "flex", alignItems: "center", color: "var(--text-dim)", pointerEvents: "none" }}>
          <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round"><circle cx="7" cy="7" r="4.5" /><line x1="10.6" y1="10.6" x2="14" y2="14" /></svg>
        </span>
      </div>

      {actionMessage && <div role="status" style={{ fontSize: 12, color: "#16a34a" }}>{actionMessage}</div>}
      {actionError && <div role="alert" style={{ fontSize: 12, color: "#ef4444", whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>{actionError}</div>}

      {loading ? (
        <div style={{ padding: "20px 4px", color: "var(--text-muted)", fontSize: 12 }}>{t("i18n.pluginCatalogLoading")}</div>
      ) : error ? (
        <div style={{ display: "flex", alignItems: "center", gap: 10, color: "#ef4444", fontSize: 12 }}>
          <span style={{ overflowWrap: "anywhere" }}>{error}</span>
          <button type="button" onClick={() => void load(0, false)} style={buttonStyle()}>{t("i18n.retry")}</button>
        </div>
      ) : plugins.length === 0 ? (
        <div style={{ padding: "20px 4px", color: "var(--text-dim)", fontSize: 12 }}>{t("i18n.noPluginMatches")}</div>
      ) : (
        <div style={{ display: "flex", flexDirection: "column", borderTop: "1px solid var(--border)" }}>
          {plugins.map((plugin) => {
            const installed = isInstalled(plugin);
            const busy = busySource === plugin.source;
            const localizedDescription = locale === "zh-CN"
              ? plugin.descriptionLanguage === "zh-CN" ? plugin.description : plugin.descriptionZh
              : plugin.descriptionLanguage === "en" ? plugin.description : plugin.descriptionEn;
            const machineTranslated = locale === "zh-CN"
              ? plugin.descriptionZhSource === "machine"
              : plugin.descriptionEnSource === "machine";
            const showOriginal = Boolean(localizedDescription && localizedDescription !== plugin.description);
            return (
              <div key={plugin.name} style={{ display: "flex", gap: 12, alignItems: "flex-start", padding: "13px 0", borderBottom: "1px solid var(--border)" }}>
                <div style={{ minWidth: 0, flex: 1 }}>
                  <div style={{ display: "flex", alignItems: "baseline", gap: 8, flexWrap: "wrap" }}>
                    <span style={{ fontFamily: "var(--font-mono)", fontSize: 13, fontWeight: 650, color: "var(--text)" }}>{plugin.name}</span>
                    {plugin.version && <span style={{ fontSize: 10, color: "var(--text-dim)" }}>v{plugin.version}</span>}
                    <PluginUsageBadge plugin={plugin} />
                  </div>
                  <div style={{ marginTop: 5, fontSize: 12, color: "var(--text)", lineHeight: 1.45 }}>
                    {localizedDescription ?? (
                      <span style={{ color: "var(--text-dim)" }}>
                        {translatingNames.has(plugin.name) ? t("i18n.translatingDescription") : t("i18n.noLocalizedDescription")}
                      </span>
                    )}
                    {machineTranslated && (
                      <span style={{ marginLeft: 7, fontSize: 10, color: "var(--text-dim)" }}>{t("i18n.machineTranslated")}</span>
                    )}
                  </div>
                  {showOriginal && (
                    <div style={{ marginTop: 4, fontSize: 11, color: "var(--text-muted)", lineHeight: 1.4 }}>
                      {plugin.description}
                    </div>
                  )}
                  <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 6, fontSize: 10, color: "var(--text-dim)" }}>
                    {plugin.publisher && <span>{plugin.publisher}</span>}
                    <a href={plugin.npmUrl} target="_blank" rel="noopener noreferrer" style={{ color: "var(--accent)", textDecoration: "none" }}>npm ↗</a>
                    {plugin.repositoryUrl && <a href={plugin.repositoryUrl} target="_blank" rel="noopener noreferrer" style={{ color: "var(--accent)", textDecoration: "none" }}>source ↗</a>}
                  </div>
                </div>
                <button
                  type="button"
                  disabled={installed || busy || (scope === "project" && !projectResourcesLoaded)}
                  onClick={() => onInstall(plugin.source)}
                  title={scope === "project" && !projectResourcesLoaded ? t("trust.projectScopeUnavailable") : undefined}
                  style={{ ...buttonStyle(installed || busy), flexShrink: 0, minWidth: 68, color: installed ? "var(--text-dim)" : "white", background: installed ? "none" : "var(--accent)", borderColor: installed ? "var(--border)" : "var(--accent)" }}
                >
                  {busy ? t("i18n.installing") : installed ? t("i18n.installed") : t("i18n.install")}
                </button>
              </div>
            );
          })}
          {hasMore && (
            <button type="button" onClick={() => void load(nextFrom, true)} disabled={loadingMore} style={{ ...buttonStyle(loadingMore), alignSelf: "center", marginTop: 12 }}>
              {loadingMore ? t("i18n.pluginCatalogLoading") : t("i18n.loadMorePlugins")}
            </button>
          )}
        </div>
      )}
    </div>
  );
}

function AddPluginPanel({
  cwd,
  source,
  scope,
  projectResourcesLoaded,
  busy,
  actionError,
  actionMessage,
  onSourceChange,
  onScopeChange,
  onInstall,
  installedPackages,
  onCatalogInstall,
}: {
  cwd: string;
  source: string;
  scope: PluginScope;
  projectResourcesLoaded: boolean;
  busy: boolean;
  actionError: string | null;
  actionMessage: string | null;
  onSourceChange: (value: string) => void;
  onScopeChange: (scope: PluginScope) => void;
  onInstall: () => void;
  installedPackages: PluginPackageInfo[];
  onCatalogInstall: (source: string) => void;
}) {
  const { t } = useI18n();
  const examples = ["npm:@scope/pi-plugin", "git:https://github.com/user/repo", "/absolute/path/to/plugin"];

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 22, maxWidth: 700, minHeight: "100%" }}>
      <PluginCatalogPanel
        scope={scope}
        onScopeChange={onScopeChange}
        projectResourcesLoaded={projectResourcesLoaded}
        installedPackages={installedPackages}
        busySource={busy && source ? source : null}
        actionError={actionError}
        actionMessage={actionMessage}
        onInstall={onCatalogInstall}
      />

      <details style={{ borderTop: "1px solid var(--border)", paddingTop: 14 }}>
        <summary style={{ cursor: "pointer", color: "var(--text-muted)", fontSize: 12, fontWeight: 600 }}>{t("i18n.manualPluginInstall")}</summary>
        <div style={{ display: "flex", flexDirection: "column", gap: 14, marginTop: 14 }}>
      <div style={{ display: "flex", flexDirection: "column", gap: 5 }}>
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 12, flexWrap: "wrap" }}>
          <div style={{ fontSize: 14, fontWeight: 700, color: "var(--text)" }}>
            {t("i18n.addPlugin")}
          </div>
          <a
            href="https://pi.dev/packages"
            target="_blank"
            rel="noopener noreferrer"
            style={{
              display: "inline-flex",
              alignItems: "center",
              gap: 5,
              color: "var(--accent)",
              fontSize: 12,
              textDecoration: "none",
              whiteSpace: "nowrap",
            }}
          >
            <svg width="28" height="28" viewBox="0 0 800 800" aria-hidden="true" focusable="false" style={{ flexShrink: 0 }}>
              <path
                fill="#000"
                fillRule="evenodd"
                d="M165.29 165.29H517.36V400H400V517.36H282.65V634.72H165.29ZM282.65 282.65V400H400V282.65Z"
              />
              <path fill="#000" d="M517.36 400H634.72V634.72H517.36Z" />
            </svg>
            pi.dev/packages
          </a>
        </div>
        <div style={{ fontSize: 12, color: "var(--text-dim)", fontFamily: "var(--font-mono)" }}>
          {installLocation(scope, cwd)}
        </div>
      </div>

      <div style={{ display: "flex", flexDirection: "column", gap: 7 }}>
        <label htmlFor="plugin-source" style={{ fontSize: 12, fontWeight: 600, color: "var(--text-muted)" }}>
          Source
        </label>
        <input
          id="plugin-source"
          value={source}
          onChange={(e) => onSourceChange(e.target.value)}
          onPaste={(e) => {
            const pasted = e.clipboardData.getData("text");
            const normalized = normalizePluginSourceInput(pasted);
            if (normalized === pasted) return;
            e.preventDefault();
            onSourceChange(normalized);
          }}
          onBlur={(e) => onSourceChange(normalizePluginSourceInput(e.currentTarget.value))}
          placeholder="npm:@scope/package"
          style={{
            width: "100%",
            height: 36,
            padding: "0 11px",
            border: "1px solid var(--border)",
            borderRadius: 6,
            background: "var(--bg-panel)",
            color: "var(--text)",
            fontFamily: "var(--font-mono)",
            fontSize: 13,
            outline: "none",
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && source.trim() && !busy) onInstall();
          }}
        />
      </div>

      <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
        <button
          type="button"
          onClick={onInstall}
          disabled={busy || !source.trim()}
          style={{
            ...buttonStyle(busy || !source.trim()),
            background: "var(--accent)",
            color: "white",
            borderColor: "var(--accent)",
          }}
        >
          {busy ? t("i18n.installing") : t("i18n.install")}
        </button>
      </div>

      <div style={{ display: "flex", flexDirection: "column", gap: 7 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-muted)" }}>
          Examples
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          {examples.map((example) => (
            <button
              key={example}
              type="button"
              onClick={() => onSourceChange(example)}
              style={{
                width: "100%",
                minHeight: 30,
                textAlign: "left",
                padding: "6px 9px",
                border: "1px solid var(--border)",
                borderRadius: 6,
                background: "var(--bg-panel)",
                color: "var(--text-dim)",
                cursor: "pointer",
                fontFamily: "var(--font-mono)",
                fontSize: 11,
              }}
              onMouseEnter={(e) => {
                e.currentTarget.style.background = "var(--bg-hover)";
                e.currentTarget.style.color = "var(--text-muted)";
              }}
              onMouseLeave={(e) => {
                e.currentTarget.style.background = "var(--bg-panel)";
                e.currentTarget.style.color = "var(--text-dim)";
              }}
            >
              {example}
            </button>
          ))}
        </div>
      </div>

      {actionError && (
        <div style={{ fontSize: 12, color: "#ef4444", whiteSpace: "pre-wrap" }}>
          {actionError}
        </div>
      )}
        </div>
      </details>
    </div>
  );
}

function PackageDetail({
  pkg,
  cwd,
  busyKey,
  actionError,
  actionMessage,
  sessionId,
  onAction,
  onReloadSession,
}: {
  pkg: PluginPackageInfo;
  cwd: string;
  busyKey: string | null;
  actionError: string | null;
  actionMessage: string | null;
  sessionId: string | null;
  onAction: (action: PluginAction, pkg: PluginPackageInfo) => void;
  onReloadSession: () => void;
}) {
  const { t } = useI18n();
  const key = packageKey(pkg);
  const busy = busyKey?.endsWith(key) ?? false;
  const reloadBusy = busyKey === "reload";
  const enabled = !pkg.disabled;

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 20, maxWidth: 680 }}>
      <div style={{ display: "flex", alignItems: "flex-start", justifyContent: "space-between", gap: 12, minWidth: 0, flexWrap: "wrap" }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 180, flex: 1 }}>
          <Toggle
            enabled={enabled}
            loading={busy || reloadBusy}
            onToggle={() => onAction(pkg.disabled ? "enable" : "disable", pkg)}
            label={pkg.disabled ? t("i18n.enablePackage") : t("i18n.disablePackage")}
          />
          <ScopeTag scope={pkg.scope} />
          {pkg.disabled ? (
            <span
              style={{
                fontSize: 10,
                padding: "1px 5px",
                borderRadius: 3,
                background: "rgba(120,120,120,0.12)",
                color: "var(--text-dim)",
              }}
            >
              {t("i18n.disabled")}
            </span>
          ) : pkg.filtered && (
            <span
              style={{
                fontSize: 10,
                padding: "1px 5px",
                borderRadius: 3,
                background: "rgba(245,158,11,0.12)",
                color: "#d97706",
              }}
            >
              {t("i18n.filtered")}
            </span>
          )}
          <span
            style={{
              fontFamily: "var(--font-mono)",
              fontSize: 12,
              color: "var(--text)",
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {pkg.source}
          </span>
        </div>

        <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
          <button
            onClick={() => onAction("update", pkg)}
            disabled={busy || reloadBusy}
            style={buttonStyle(busy || reloadBusy)}
          >
             {busyKey === `update:${key}` ? t("i18n.updating") : t("i18n.update")}
          </button>
          <button
            onClick={onReloadSession}
            disabled={!sessionId || reloadBusy || busy}
            style={buttonStyle(!sessionId || reloadBusy || busy)}
             title={sessionId ? t("i18n.reloadSession") : t("i18n.openSessionToReload")}
          >
             {reloadBusy ? t("i18n.reloading") : t("i18n.reloadSession")}
          </button>
          <button
            onClick={() => onAction("remove", pkg)}
            disabled={busy || reloadBusy}
            style={buttonStyle(busy || reloadBusy, true)}
          >
             {busyKey === `remove:${key}` ? t("i18n.removing") : t("i18n.remove")}
          </button>
        </div>
      </div>

      <div
        style={{
          display: "grid",
          gridTemplateColumns: "minmax(96px, 130px) minmax(0, 1fr)",
          gap: "9px 14px",
          fontSize: 12,
          lineHeight: 1.45,
        }}
      >
        <div style={{ color: "var(--text-dim)" }}>{t("i18n.status")}</div>
        <div style={{ color: statusColor(pkg.status), textTransform: "capitalize" }}>{pkg.status}</div>
        <div style={{ color: "var(--text-dim)" }}>{t("i18n.version")}</div>
         <div style={{ color: "var(--text-muted)", fontFamily: "var(--font-mono)" }}>{versionSummary(pkg, t)}</div>
        <div style={{ color: "var(--text-dim)" }}>{t("i18n.package")}</div>
        <div style={{ color: "var(--text-muted)", fontFamily: "var(--font-mono)", overflowWrap: "anywhere" }}>
          {pkg.packageName ?? t("i18n.unknown")}
        </div>
        <div style={{ color: "var(--text-dim)" }}>{t("i18n.resources")}</div>
         <div style={{ color: "var(--text-muted)" }}>{resourceSummary(pkg, t)}</div>
        <div style={{ color: "var(--text-dim)" }}>{t("i18n.installedPath")}</div>
        <div
          style={{
            color: pkg.installedPath ? "var(--text-muted)" : "#ef4444",
            fontFamily: "var(--font-mono)",
            overflowWrap: "anywhere",
          }}
        >
          {pkg.installedPath ? shortenPath(pkg.installedPath) : t("i18n.notFound")}
        </div>
        <div style={{ color: "var(--text-dim)" }}>{t("i18n.cwd")}</div>
        <div style={{ color: "var(--text-dim)", fontFamily: "var(--font-mono)", overflowWrap: "anywhere" }}>
          {shortenPath(cwd)}
        </div>
      </div>

      <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
        <div style={{ fontSize: 12, fontWeight: 700, color: "var(--text)" }}>
          {t("i18n.resolvedResources")}
        </div>
        <ResourceList pkg={pkg} />
      </div>

      {actionMessage && (
        <div style={{ fontSize: 12, color: "#16a34a" }}>
          {actionMessage}
        </div>
      )}
      {actionError && (
        <div style={{ fontSize: 12, color: "#ef4444", whiteSpace: "pre-wrap" }}>
          {actionError}
        </div>
      )}
    </div>
  );
}

export function PluginsConfig({
  cwd,
  sessionId,
  embedded = false,
  onClose,
  onReloaded,
}: {
  cwd: string;
  sessionId: string | null;
  embedded?: boolean;
  onClose?: () => void;
  onReloaded?: () => void;
}) {
  const isMobile = useIsMobile();
  const { t } = useI18n();
  const [data, setData] = useState<PluginsResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [addMode, setAddMode] = useState(false);
  const [installSource, setInstallSource] = useState("");
  const [installScope, setInstallScope] = useState<PluginScope>("global");
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionMessage, setActionMessage] = useState<string | null>(null);

  const packages = useMemo(() => data?.packages ?? [], [data?.packages]);
  const selectedPackage = packages.find((pkg) => packageKey(pkg) === selected) ?? null;
  const projectResourcesLoaded = data?.projectResourcesLoaded ?? true;

  const groupedPackages = useMemo(() => {
    return (["project", "global"] as PluginScope[])
      .map((scope) => ({ scope, packages: packages.filter((pkg) => pkg.scope === scope) }))
      .filter((group) => group.packages.length > 0);
  }, [packages]);

  const loadPlugins = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await fetch(`/api/plugins?cwd=${encodeURIComponent(cwd)}`);
      const next = (await res.json()) as PluginsResponse & { error?: string };
      if (!res.ok || next.error) throw new Error(next.error ?? `HTTP ${res.status}`);
      setData(next);
      setAddMode((current) => next.packages.length === 0 || current);
      setSelected((current) => {
        if (current && next.packages.some((pkg) => packageKey(pkg) === current)) return current;
        return next.packages[0] ? packageKey(next.packages[0]) : null;
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, [cwd]);

  useEffect(() => {
    void loadPlugins();
  }, [loadPlugins]);

  const runAction = useCallback(async (action: PluginAction, pkg: PluginPackageInfo) => {
    const key = packageKey(pkg);
    setBusyKey(`${action}:${key}`);
    setActionError(null);
    setActionMessage(null);
    try {
      const res = await fetch("/api/plugins", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ action, source: pkg.source, scope: pkg.scope, cwd }),
      });
      const next = (await res.json()) as PluginsResponse & { error?: string };
      if (!res.ok || next.error) throw new Error(next.error ?? `HTTP ${res.status}`);
      setData(next);
      if (action === "remove") {
        setSelected(next.packages[0] ? packageKey(next.packages[0]) : null);
        if (next.packages.length === 0) setAddMode(true);
        setActionMessage("Package removed.");
      } else {
        const messages: Record<Exclude<PluginAction, "remove">, string> = {
          install: "Package installed.",
          update: "Package updated.",
          disable: "Package disabled.",
          enable: "Package enabled.",
        };
        setActionMessage(messages[action]);
      }
    } catch (err) {
      setActionError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusyKey(null);
    }
  }, [cwd]);

  const installPlugin = useCallback(async (sourceOverride?: string) => {
    const source = normalizePluginSourceInput(sourceOverride ?? installSource).trim();
    if (!source) return;
    setInstallSource(source);
    const key = `${installScope}\0${source}`;
    setBusyKey(`install:${key}`);
    setActionError(null);
    setActionMessage(null);
    try {
      const res = await fetch("/api/plugins", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ action: "install", source, scope: installScope, cwd }),
      });
      const next = (await res.json()) as PluginsResponse & { error?: string };
      if (!res.ok || next.error) throw new Error(next.error ?? `HTTP ${res.status}`);
      setData(next);
      const installed = findInstalledPackage(next.packages, source, installScope);
      setSelected(installed ? packageKey(installed) : key);
      setAddMode(false);
      setInstallSource("");
      setActionMessage("Package installed.");
    } catch (err) {
      setActionError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusyKey(null);
    }
  }, [cwd, installScope, installSource]);

  const reloadSession = useCallback(async () => {
    if (!sessionId) return;
    setBusyKey("reload");
    setActionError(null);
    setActionMessage(null);
    try {
      await sendAgentCommand(sessionId, { type: "reload" });
      onReloaded?.();
      await loadPlugins();
      setActionMessage("Session reloaded.");
    } catch (err) {
      setActionError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusyKey(null);
    }
  }, [loadPlugins, onReloaded, sessionId]);

  const addBusy = busyKey?.startsWith("install:") ?? false;

  return (
    <div
      style={{
        position: embedded ? "relative" : "fixed",
        inset: embedded ? undefined : 0,
        zIndex: embedded ? undefined : 1000,
        width: embedded ? "100%" : undefined,
        height: embedded ? "100%" : undefined,
        background: embedded ? "transparent" : "rgba(0,0,0,0.35)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
      }}
      onClick={(e) => {
        if (!embedded && e.target === e.currentTarget) onClose?.();
      }}
    >
      <div
        style={{
          width: embedded ? "100%" : isMobile ? "calc(100vw - 16px)" : 860,
          maxWidth: embedded ? "none" : "calc(100vw - 16px)",
          height: embedded ? "100%" : isMobile ? "calc(100dvh - 16px)" : "76vh",
          maxHeight: embedded ? "none" : "calc(100dvh - 16px)",
          background: "var(--bg)",
          border: embedded ? "none" : "1px solid var(--border)",
          borderRadius: embedded ? 0 : 8,
          display: "flex",
          flexDirection: "column",
          boxShadow: embedded ? "none" : "0 8px 32px rgba(0,0,0,0.18)",
          overflow: "hidden",
        }}
      >
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            padding: "12px 18px",
            borderBottom: "1px solid var(--border)",
            flexShrink: 0,
          }}
        >
          <div style={{ display: "flex", alignItems: "baseline", gap: 10, minWidth: 0 }}>
            <span style={{ fontSize: 15, fontWeight: 700, color: "var(--text)" }}>
              {t("common.plugins")}
            </span>
            {/* 标题旁不再挂当前项目路径：左侧栏本来就常驻显示它，这里只是噪声。 */}
          </div>
          {!embedded && <button
            onClick={onClose}
            style={{
              background: "none",
              border: "none",
              color: "var(--text-muted)",
              cursor: "pointer",
              fontSize: 20,
              lineHeight: 1,
              padding: "2px 6px",
            }}
          >
            ×
          </button>}
        </div>

        {!projectResourcesLoaded && (
          <div
            role="status"
            style={{
              padding: "8px 18px",
              borderBottom: "1px solid var(--border)",
              background: "var(--bg-panel)",
              color: "var(--text-muted)",
              fontSize: 12,
            }}
          >
            {t("trust.pluginsNotLoaded")}
          </div>
        )}

        <div style={{ flex: 1, display: "flex", flexDirection: isMobile ? "column" : "row", overflow: "hidden" }}>
          <div
            style={{
              width: isMobile ? "100%" : 245,
              maxHeight: isMobile ? "40vh" : undefined,
              borderRight: isMobile ? "none" : "1px solid var(--border)",
              borderBottom: isMobile ? "1px solid var(--border)" : "none",
              display: "flex",
              flexDirection: "column",
              flexShrink: 0,
              background: "var(--bg-panel)",
            }}
          >
            <div style={{ flex: 1, overflowY: "auto", padding: "8px 6px" }}>
              {loading ? (
                <div style={{ padding: "10px 8px", fontSize: 12, color: "var(--text-muted)" }}>
                  Loading...
                </div>
              ) : error ? (
                <div style={{ padding: "10px 8px", fontSize: 11, color: "#ef4444" }}>
                  {error}
                </div>
              ) : packages.length === 0 ? (
                <div style={{ padding: "10px 8px", fontSize: 11, color: "var(--text-dim)" }}>
                  No plugins configured
                </div>
              ) : (
                groupedPackages.map((group) => (
                  <div key={group.scope} style={{ marginBottom: 6 }}>
                    <div
                      style={{
                        padding: "4px 8px 3px",
                        fontSize: 10,
                        fontWeight: 600,
                        color: "var(--text-dim)",
                        textTransform: "uppercase",
                      }}
                    >
                      {group.scope}
                    </div>
                    {group.packages.map((pkg) => {
                      const key = packageKey(pkg);
                      const isSelected = !addMode && selected === key;
                      return (
                        <div
                          key={key}
                          onClick={() => {
                            setSelected(key);
                            setAddMode(false);
                            setActionError(null);
                            setActionMessage(null);
                          }}
                          style={{
                            display: "flex",
                            alignItems: "center",
                            gap: 7,
                            padding: "8px 8px",
                            borderRadius: 5,
                            cursor: "pointer",
                            background: isSelected ? "var(--bg-selected)" : "none",
                          }}
                          onMouseEnter={(e) => {
                            if (!isSelected) e.currentTarget.style.background = "var(--bg-hover)";
                          }}
                          onMouseLeave={(e) => {
                            if (!isSelected) e.currentTarget.style.background = "none";
                          }}
                        >
                          <span
                            style={{
                              flexShrink: 0,
                              width: 7,
                              height: 7,
                              borderRadius: "50%",
                              background: statusColor(pkg.status),
                            }}
                          />
                          <div style={{ minWidth: 0, flex: 1 }}>
                            <div
                              style={{
                                fontSize: 12,
                                fontWeight: isSelected ? 600 : 400,
                                color: "var(--text)",
                                fontFamily: "var(--font-mono)",
                                overflow: "hidden",
                                textOverflow: "ellipsis",
                                whiteSpace: "nowrap",
                              }}
                            >
                              {pkg.source}
                            </div>
                            <div
                              style={{
                                fontSize: 10,
                                color: "var(--text-dim)",
                                overflow: "hidden",
                                textOverflow: "ellipsis",
                                whiteSpace: "nowrap",
                                marginTop: 2,
                              }}
                            >
                              {resourceSummary(pkg, t)}
                            </div>
                            {(pkg.version || pkg.configuredVersion) && (
                              <div
                                style={{
                                  fontSize: 10,
                                  color: "var(--text-dim)",
                                  overflow: "hidden",
                                  textOverflow: "ellipsis",
                                  whiteSpace: "nowrap",
                                  marginTop: 2,
                                }}
                              >
                                 {versionSummary(pkg, t)}
                              </div>
                            )}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                ))
              )}
            </div>
            <div style={{ padding: "8px 6px", borderTop: "1px solid var(--border)", flexShrink: 0 }}>
              <button
                type="button"
                onClick={() => {
                  setAddMode(true);
                  setActionError(null);
                  setActionMessage(null);
                }}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 6,
                  padding: "7px 8px",
                  borderRadius: 5,
                  border: "none",
                  width: "100%",
                  cursor: "pointer",
                  background: addMode ? "var(--bg-selected)" : "none",
                  color: addMode ? "var(--accent)" : "var(--text-dim)",
                  fontSize: 12,
                }}
                onMouseEnter={(e) => {
                  if (!addMode) e.currentTarget.style.background = "var(--bg-hover)";
                }}
                onMouseLeave={(e) => {
                  if (!addMode) e.currentTarget.style.background = "none";
                }}
              >
                <svg
                  width="13"
                  height="13"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                >
                  <line x1="12" y1="5" x2="12" y2="19" />
                  <line x1="5" y1="12" x2="19" y2="12" />
                </svg>
                 {t("i18n.addPlugin")}
              </button>
            </div>
          </div>

          <div style={{ flex: 1, overflowY: "auto", padding: 20 }}>
            {addMode ? (
              <AddPluginPanel
                cwd={cwd}
                source={installSource}
                scope={installScope}
                projectResourcesLoaded={projectResourcesLoaded}
                busy={addBusy}
                actionError={actionError}
                actionMessage={actionMessage}
                onSourceChange={setInstallSource}
                onScopeChange={setInstallScope}
                onInstall={installPlugin}
                installedPackages={packages}
                onCatalogInstall={(source) => {
                  setInstallSource(source);
                  void installPlugin(source);
                }}
              />
            ) : loading ? null : selectedPackage ? (
              <PackageDetail
                key={packageKey(selectedPackage)}
                pkg={selectedPackage}
                cwd={cwd}
                busyKey={busyKey}
                actionError={actionError}
                actionMessage={actionMessage}
                sessionId={sessionId}
                onAction={runAction}
                onReloadSession={reloadSession}
              />
            ) : (
              <div
                style={{
                  height: "100%",
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  color: "var(--text-dim)",
                  fontSize: 13,
                }}
              >
                {t("i18n.selectPackage")}
              </div>
            )}
          </div>
        </div>

        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            gap: 12,
            padding: "10px 18px",
            borderTop: "1px solid var(--border)",
            flexShrink: 0,
          }}
        >
          <div style={{ minWidth: 0, flex: 1, fontSize: 11, color: "var(--text-dim)", overflow: "hidden" }}>
            {data?.diagnostics.length ? (
              <span
                title={data.diagnostics.map((d) => `${d.type}: ${d.source ? `${d.source}: ` : ""}${d.message}`).join("\n")}
                style={{ color: data.diagnostics.some((d) => d.type === "error") ? "#ef4444" : "#d97706" }}
              >
                {data.diagnostics.length} diagnostic{data.diagnostics.length === 1 ? "" : "s"}
              </span>
            ) : (
              <span>
                {data ? `${data.totals.extensions} ext · ${data.totals.skills} skills · ${data.totals.prompts} prompts · ${data.totals.themes} themes` : ""}
              </span>
            )}
          </div>
          <button onClick={() => void loadPlugins()} disabled={loading || busyKey !== null} style={buttonStyle(loading || busyKey !== null)}>
             {t("i18n.refresh")}
          </button>
          {!embedded && <button onClick={onClose} style={buttonStyle(false)}>
             {t("i18n.close")}
          </button>}
        </div>
      </div>
    </div>
  );
}
