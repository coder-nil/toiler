"use client";

import { FormEvent, KeyboardEvent, useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "@/hooks/useI18n";
import { executeSimulatedConsoleCommand, type SimulatedConsoleOutputKind } from "@/lib/simulated-console";

type ConsoleEntryKind = SimulatedConsoleOutputKind | "input" | "system";

interface ConsoleEntry {
  id: string;
  kind: ConsoleEntryKind;
  text: string;
  prompt?: string;
}

interface Props {
  cwd: string | null;
  sessionId: string | null;
  sessionName?: string | null;
}

function TerminalGlyph() {
  return (
    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <polyline points="4 17 10 11 4 5" />
      <line x1="12" y1="19" x2="20" y2="19" />
    </svg>
  );
}

export function SimulatedConsole({ cwd, sessionId, sessionName }: Props) {
  const { t } = useI18n();
  const [entries, setEntries] = useState<ConsoleEntry[]>([]);
  const [input, setInput] = useState("");
  const [history, setHistory] = useState<string[]>([]);
  const [historyIndex, setHistoryIndex] = useState<number | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const entryCounterRef = useRef(0);
  const contextLabel = useMemo(() => cwd ?? t("console.noProject"), [cwd, t]);
  const sessionLabel = sessionName ?? sessionId ?? t("console.noSession");

  const nextId = () => {
    entryCounterRef.current += 1;
    return `console-entry-${entryCounterRef.current}`;
  };

  const appendSystemLine = (text: string) => {
    setEntries((current) => [...current, { id: nextId(), kind: "system", text }]);
  };

  useEffect(() => {
    appendSystemLine(t("console.ready"));
  // Run only on mount; locale changes should not replay the welcome line.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    appendSystemLine(t("console.context", { cwd: contextLabel }));
  // Context switches should add a visible marker; translation changes are handled by the label text at that moment.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [contextLabel]);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, [entries]);

  const prompt = cwd ? "$" : ">";

  const runCommand = (rawCommand: string) => {
    const command = rawCommand.trim();
    if (!command) return;
    const nextHistory = [...history, command].slice(-80);
    const result = executeSimulatedConsoleCommand(command, {
      cwd,
      history: nextHistory,
      sessionId,
      sessionName,
    });
    setHistory(nextHistory);
    setHistoryIndex(null);
    setInput("");

    if (result.clear) {
      setEntries([]);
      return;
    }

    setEntries((current) => [
      ...current,
      { id: nextId(), kind: "input", text: command, prompt },
      ...result.lines.map((line) => ({ id: nextId(), kind: line.kind, text: line.text })),
    ]);
  };

  const handleSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    runCommand(input);
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
    if (history.length === 0) return;
    event.preventDefault();

    if (event.key === "ArrowUp") {
      const nextIndex = historyIndex === null ? history.length - 1 : Math.max(0, historyIndex - 1);
      setHistoryIndex(nextIndex);
      setInput(history[nextIndex]);
      return;
    }

    if (historyIndex === null) return;
    const nextIndex = historyIndex + 1;
    if (nextIndex >= history.length) {
      setHistoryIndex(null);
      setInput("");
      return;
    }
    setHistoryIndex(nextIndex);
    setInput(history[nextIndex]);
  };

  return (
    <div
      style={{
        display: "grid",
        gridTemplateRows: "auto minmax(0, 1fr) auto",
        height: "100%",
        minHeight: 0,
        background: "var(--bg)",
      }}
    >
      <div
        style={{
          display: "grid",
          gridTemplateColumns: "minmax(0, 1fr) auto auto",
          gap: 8,
          alignItems: "center",
          minHeight: 42,
          padding: "8px 10px",
          borderBottom: "1px solid var(--border)",
          background: "var(--bg-panel)",
        }}
      >
        <div style={{ minWidth: 0, display: "grid", gap: 2 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6, minWidth: 0, color: "var(--text)", fontSize: 12, fontWeight: 650 }}>
            <TerminalGlyph />
            <span>{t("console.title")}</span>
          </div>
          <div style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", color: "var(--text-dim)", fontFamily: "var(--font-mono)", fontSize: 11 }} title={contextLabel}>
            {contextLabel}
          </div>
        </div>
        <button
          type="button"
          onClick={() => runCommand("help")}
          title={t("console.help")}
          aria-label={t("console.help")}
          style={{
            width: 30,
            height: 28,
            border: "1px solid var(--border)",
            borderRadius: 4,
            background: "transparent",
            color: "var(--text-muted)",
            cursor: "pointer",
            fontSize: 12,
            fontWeight: 700,
          }}
        >
          ?
        </button>
        <button
          type="button"
          onClick={() => setEntries([])}
          title={t("console.clear")}
          aria-label={t("console.clear")}
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            width: 30,
            height: 28,
            border: "1px solid var(--border)",
            borderRadius: 4,
            background: "transparent",
            color: "var(--text-muted)",
            cursor: "pointer",
          }}
        >
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M3 6h18" />
            <path d="M8 6V4h8v2" />
            <path d="M6 6l1 14h10l1-14" />
          </svg>
        </button>
      </div>
      <div
        ref={scrollRef}
        role="log"
        aria-label={t("console.output")}
        style={{
          minHeight: 0,
          overflow: "auto",
          padding: "12px",
          background: "linear-gradient(180deg, #101418 0%, #0b0f12 100%)",
          color: "#dbe7ef",
          fontFamily: "var(--font-mono)",
          fontSize: 12,
          lineHeight: 1.55,
        }}
      >
        {entries.length === 0 ? (
          <div style={{ color: "#6f7f89", fontStyle: "italic" }}>{t("console.empty")}</div>
        ) : entries.map((entry) => (
          <div
            key={entry.id}
            style={{
              display: "flex",
              gap: 8,
              minWidth: 0,
              color: entry.kind === "error" ? "#fca5a5" : entry.kind === "system" ? "#8dd5a2" : "inherit",
              overflowWrap: "anywhere",
              whiteSpace: "pre-wrap",
            }}
          >
            {entry.kind === "input" ? (
              <span style={{ color: "#8dd5a2", flexShrink: 0 }}>{entry.prompt}</span>
            ) : (
              <span style={{ color: "#46525b", flexShrink: 0 }}>{entry.kind === "system" ? "*" : " "}</span>
            )}
            <span>{entry.text}</span>
          </div>
        ))}
      </div>
      <form
        onSubmit={handleSubmit}
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          padding: "9px 10px",
          borderTop: "1px solid var(--border)",
          background: "var(--bg-panel)",
          fontFamily: "var(--font-mono)",
        }}
      >
        <span style={{ color: "var(--accent)", fontWeight: 700, flexShrink: 0 }}>{prompt}</span>
        <input
          value={input}
          onChange={(event) => setInput(event.target.value)}
          onKeyDown={handleKeyDown}
          autoCapitalize="off"
          autoComplete="off"
          autoCorrect="off"
          spellCheck={false}
          placeholder={t("console.placeholder")}
          aria-label={t("console.prompt")}
          style={{
            flex: 1,
            minWidth: 0,
            height: 30,
            border: "1px solid var(--border)",
            borderRadius: 4,
            background: "var(--bg)",
            color: "var(--text)",
            font: "inherit",
            outline: "none",
            padding: "0 8px",
          }}
        />
        <div style={{ maxWidth: 120, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", color: "var(--text-dim)", fontSize: 11 }} title={sessionLabel}>
          {sessionLabel}
        </div>
      </form>
    </div>
  );
}
