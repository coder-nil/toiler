"use client";

import { useCallback, useEffect, useState } from "react";
import { Info } from "lucide-react";
import { useI18n } from "@/hooks/useI18n";
import { copyText } from "@/lib/clipboard";
import type { ChangelogRelease, ChangelogSectionKind } from "@/data/changelog-types";
import {
  APPLICATION_VERSION,
  CHANGELOG,
  PI_VERSION,
  changelogToMarkdown,
  currentRelease,
} from "@/lib/changelog";

/**
 * 版本信息图标（无边框的 ⓘ）。侧边栏标题行翻出版本号时显示，点击后由上层
 * 打开 `AboutDialog`——图标本身不持有对话框状态。设置里的「关于」行不用它，
 * 那一行整行可点，避免同一行出现两个入口。
 */
export function AboutButton({ onClick, size = 20 }: { onClick: () => void; size?: number }) {
  const { t } = useI18n();

  return (
    <button
      type="button"
      onClick={onClick}
      title={t("about.title")}
      aria-label={t("about.title")}
      style={{
        flexShrink: 0,
        display: "inline-flex", alignItems: "center", justifyContent: "center",
        width: size, height: size, padding: 0,
        border: "none", borderRadius: 5,
        background: "transparent", color: "var(--text-muted)",
        cursor: "pointer",
      }}
      onMouseEnter={(event) => { event.currentTarget.style.color = "var(--text)"; }}
      onMouseLeave={(event) => { event.currentTarget.style.color = "var(--text-muted)"; }}
    >
      <Info size={Math.round(size * 0.8)} strokeWidth={2} aria-hidden="true" />
    </button>
  );
}

const SECTION_LABEL_KEYS: Record<ChangelogSectionKind, string> = {
  added: "about.sectionAdded",
  changed: "about.sectionChanged",
  fixed: "about.sectionFixed",
  removed: "about.sectionRemoved",
  deprecated: "about.sectionDeprecated",
  security: "about.sectionSecurity",
  other: "about.sectionOther",
};

/** `CHANGELOG.md` 的条目本身就是 Markdown；这里只渲染行内代码与粗体。 */
function ChangelogItem({ text }: { text: string }) {
  const parts = text.split(/(`[^`]+`)/g);
  return (
    <li style={{ marginBottom: 5, lineHeight: 1.6 }}>
      {parts.map((part, index) => (
        part.startsWith("`") && part.endsWith("`") && part.length > 2
          ? (
            <code
              key={index}
              style={{
                padding: "1px 4px", borderRadius: 4,
                background: "var(--bg-hover)", color: "var(--text)",
                fontFamily: "var(--font-mono)", fontSize: "0.92em",
              }}
            >
              {part.slice(1, -1)}
            </code>
          )
          : <span key={index}>{part}</span>
      ))}
    </li>
  );
}

function ReleaseBlock({ release, current, defaultOpen }: { release: ChangelogRelease; current: boolean; defaultOpen: boolean }) {
  const { t } = useI18n();
  const [open, setOpen] = useState(defaultOpen);

  useEffect(() => { setOpen(defaultOpen); }, [defaultOpen]);

  return (
    <div style={{ borderTop: "1px solid var(--border)" }}>
      <button
        type="button"
        onClick={() => setOpen((value) => !value)}
        aria-expanded={open}
        style={{
          width: "100%", display: "flex", alignItems: "center", gap: 8,
          padding: "9px 2px", border: "none", background: "none",
          color: "var(--text)", cursor: "pointer", textAlign: "left",
        }}
      >
        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" style={{ flexShrink: 0, color: "var(--text-dim)", transform: open ? "rotate(90deg)" : "none", transition: "transform 0.15s" }}>
          <path d="m9 18 6-6-6-6" />
        </svg>
        <span style={{ fontFamily: "var(--font-mono)", fontSize: 12, fontWeight: 650 }}>v{release.version}</span>
        {current && (
          <span style={{ padding: "1px 6px", borderRadius: 999, background: "rgba(37,99,235,0.12)", color: "var(--accent)", fontSize: 10, fontWeight: 600 }}>
            {t("about.currentTag")}
          </span>
        )}
        <span style={{ marginLeft: "auto", color: "var(--text-dim)", fontSize: 10, flexShrink: 0 }}>{release.date}</span>
      </button>
      {open && (
        <div style={{ padding: "0 2px 12px 21px" }}>
          {release.sections.map((section) => (
            <div key={section.title} style={{ marginBottom: 10 }}>
              <div style={{ marginBottom: 4, color: "var(--text-muted)", fontSize: 10, fontWeight: 700, letterSpacing: "0.04em", textTransform: "uppercase" }}>
                {t(SECTION_LABEL_KEYS[section.kind])}
              </div>
              <ul style={{ margin: 0, paddingLeft: 16, color: "var(--text-muted)", fontSize: 11.5 }}>
                {section.items.map((item, index) => <ChangelogItem key={index} text={item} />)}
              </ul>
            </div>
          ))}
          {release.sections.length === 0 && (
            <div style={{ color: "var(--text-dim)", fontSize: 11 }}>{t("about.emptyRelease")}</div>
          )}
        </div>
      )}
    </div>
  );
}

export function AboutDialog({ onClose }: { onClose: () => void }) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);
  const releases = CHANGELOG;
  const currentVersion = currentRelease()?.version ?? APPLICATION_VERSION;

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      onClose();
    };
    document.addEventListener("keydown", handleKeyDown, true);
    return () => document.removeEventListener("keydown", handleKeyDown, true);
  }, [onClose]);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1800);
    return () => clearTimeout(timer);
  }, [copied]);

  const handleCopy = useCallback(async () => {
    const text = [
      `Toiler v${APPLICATION_VERSION}`,
      // 不再列 `pi v${PI_VERSION}`：toiler 的后端是 rpi 而不是 pi，
      // 两个版本写在一起会让人以为界面版本来自 pi。
      `platform ${typeof navigator === "undefined" ? "unknown" : navigator.platform}`,
      "",
      changelogToMarkdown(releases),
    ].join("\n");
    await copyText(text).catch(() => { /* 剪贴板被拒时保留对话框，用户可手动选中 */ });
    setCopied(true);
  }, [releases]);

  const content = (
    <section
      role="dialog"
      aria-modal="true"
      aria-labelledby="about-dialog-title"
      style={{
        width: "min(660px, 100%)",
        maxHeight: "min(640px, calc(100dvh - 32px))",
        display: "flex", flexDirection: "column", overflow: "hidden",
        border: "1px solid var(--border)", borderRadius: 10,
        background: "var(--bg)", boxShadow: "0 14px 40px rgba(0,0,0,0.24)",
      }}
    >
        <header style={{ display: "flex", alignItems: "center", gap: 8, padding: "0 14px", height: 48, borderBottom: "1px solid var(--border)", flexShrink: 0 }}>
          <span id="about-dialog-title" style={{ fontSize: 15, fontWeight: 700 }}>{t("about.title")}</span>
          <button
            type="button"
            onClick={onClose}
            title={t("settings.close")}
            aria-label={t("settings.close")}
            style={{ marginLeft: "auto", display: "flex", alignItems: "center", justifyContent: "center", width: 28, height: 28, padding: 0, border: "none", borderRadius: 6, background: "none", color: "var(--text-muted)", cursor: "pointer", fontSize: 20, lineHeight: 1 }}
          >
            ×
          </button>
        </header>

        <div style={{ padding: "12px 16px", borderBottom: "1px solid var(--border)", flexShrink: 0 }}>
          <div style={{ display: "flex", alignItems: "baseline", gap: 8 }}>
            <span style={{ fontSize: 17, fontWeight: 700 }}>Toiler</span>
            <span style={{ fontFamily: "var(--font-mono)", fontSize: 12, color: "var(--text-muted)" }}>v{APPLICATION_VERSION}</span>
          </div>
          <div style={{ marginTop: 6, display: "flex", flexWrap: "wrap", gap: "2px 14px", color: "var(--text-dim)", fontSize: 11 }}>
            <span>{t("about.piVersion")} <span style={{ fontFamily: "var(--font-mono)", color: "var(--text-muted)" }}>v{PI_VERSION}</span></span>
            <span>{t("about.platform")} <span style={{ fontFamily: "var(--font-mono)", color: "var(--text-muted)" }}>{typeof navigator === "undefined" ? "unknown" : navigator.platform}</span></span>
          </div>
        </div>

        <div style={{ flex: 1, minHeight: 0, overflowY: "auto", padding: "10px 16px 16px" }}>
          <div style={{ display: "flex", alignItems: "center", gap: 8, margin: "2px 0 6px" }}>
            <span style={{ color: "var(--text)", fontSize: 12, fontWeight: 650 }}>{t("about.releaseNotes")}</span>
            <button
              type="button"
              onClick={() => void handleCopy()}
              style={{ marginLeft: "auto", display: "inline-flex", alignItems: "center", gap: 5, minHeight: 27, padding: "0 9px", border: "1px solid var(--border)", borderRadius: 6, background: "var(--bg-hover)", color: "var(--text)", cursor: "pointer", fontSize: 11 }}
            >
              {copied ? t("about.copied") : t("about.copyAll")}
            </button>
          </div>
          {releases.length === 0 && (
            <div style={{ padding: "18px 2px", color: "var(--text-dim)", fontSize: 11 }}>{t("about.empty")}</div>
          )}
          {releases.map((release) => (
            <ReleaseBlock
              key={release.version}
              release={release}
              current={release.version === currentVersion}
              /* 只默认展开当前版本，历史版本点开才看。 */
              defaultOpen={release.version === currentVersion}
            />
          ))}
          <div style={{ marginTop: 12, color: "var(--text-dim)", fontSize: 10, paddingLeft: 2 }}>
            {t("about.historyHint", { count: releases.length })}
          </div>
        </div>
    </section>
  );

  return (
    <div
      role="presentation"
      onClick={(event) => { if (event.target === event.currentTarget) onClose(); }}
      style={{
        position: "fixed", inset: 0, zIndex: 1200,
        display: "flex", alignItems: "center", justifyContent: "center",
        padding: 16, background: "rgba(0,0,0,0.32)",
      }}
    >
      {content}
    </div>
  );
}
