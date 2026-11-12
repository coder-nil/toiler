"use client";

import { useState, type CSSProperties } from "react";
import { useI18n } from "@/hooks/useI18n";

export function RevealFileButton({ filePath, sourceSessionId, className, style }: {
  filePath: string;
  sourceSessionId?: string | null;
  className?: string;
  style?: CSSProperties;
}) {
  const { t } = useI18n();
  const [busy, setBusy] = useState(false);
  return (
    <button
      type="button"
      className={className}
      style={style}
      title={t("files.reveal")}
      aria-label={t("files.reveal")}
      disabled={busy}
      onClick={async (event) => {
        event.stopPropagation();
        setBusy(true);
        try {
          const response = await fetch("/api/files/reveal", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ filePath, sessionId: sourceSessionId }),
          });
          if (!response.ok) throw new Error("Reveal failed");
        } catch {
          window.alert(t("files.revealFailed"));
        } finally {
          setBusy(false);
        }
      }}
    >
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <path d="M20 13v6a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h3" />
        <path d="M16 3h6v6M22 3l-9 9" />
      </svg>
    </button>
  );
}
