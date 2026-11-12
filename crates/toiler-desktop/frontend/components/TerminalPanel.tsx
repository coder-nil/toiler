"use client";

import { useEffect, useRef, useState } from "react";
import type { Terminal } from "@xterm/xterm";
import { useI18n } from "@/hooks/useI18n";
import "@xterm/xterm/css/xterm.css";

interface Props {
  cwd: string;
  visible: boolean;
  onMinimize: () => void;
  onClose: () => void;
}

export function TerminalPanel({ cwd, visible, onMinimize, onClose }: Props) {
  const { t } = useI18n();
  const host = useRef<HTMLDivElement>(null);
  const terminal = useRef<Terminal | null>(null);
  const fitNow = useRef<(() => void) | null>(null);
  const [height, setHeight] = useState(260);
  const [error, setError] = useState<string | null>(null);
  const [exited, setExited] = useState(false);
  const [generation, setGeneration] = useState(0);
  const [desktop, setDesktop] = useState(true);
  const drag = useRef<{ y: number; height: number } | null>(null);

  useEffect(() => {
    if (!window.__PI_WEB_DESKTOP__) {
      setDesktop(false);
      return;
    }
    let disposed = false;
    let id: string | null = null;
    let cleanup = () => {};
    const start = async () => {
      const [{ Terminal }, { FitAddon }, { invoke }, { listen }] = await Promise.all([
        import("@xterm/xterm"), import("@xterm/addon-fit"),
        import("@tauri-apps/api/core"), import("@tauri-apps/api/event"),
      ]);
      if (disposed || !host.current) return;
      const term = new Terminal({
        cursorBlink: true, fontSize: 13, scrollback: 5000,
        fontFamily: 'Menlo, Monaco, Consolas, "Liberation Mono", monospace',
        theme: { background: "#111418", foreground: "#dbe2ea", cursor: "#dbe2ea" },
      });
      terminal.current = term;
      const fit = new FitAddon();
      term.loadAddon(fit);
      term.open(host.current);
      const report = (reason: unknown) => { if (!disposed) setError(String(reason)); };
      const encoder = new TextEncoder();
      let writes = Promise.resolve();
      const input = term.onData((data) => {
        if (!id || disposed) return;
        const terminalId = id;
        writes = writes.then(() => invoke<void>("terminal_write", {
          terminalId, data: Array.from(encoder.encode(data)),
        })).catch(report);
      });
      const binaryInput = term.onBinary((data) => {
        if (!id || disposed) return;
        const terminalId = id;
        writes = writes.then(() => invoke<void>("terminal_write", {
          terminalId, data: Array.from(data, (character) => character.charCodeAt(0) & 255),
        })).catch(report);
      });
      // Register listeners before starting the shell: its first prompt can arrive
      // before the start command resolves with the terminal id.
      const pending: { terminalId: string; data?: number[] }[] = [];
      const receive = (event: { terminalId: string; data?: number[] }) => {
        if (disposed) return;
        if (!id) { pending.push(event); return; }
        if (event.terminalId !== id) return;
        if (event.data) term.write(new Uint8Array(event.data));
        else { term.options.disableStdin = true; setExited(true); }
      };
      const unlisteners: (() => void)[] = [];
      let observer: ResizeObserver | null = null;
      cleanup = () => {
        observer?.disconnect();
        unlisteners.forEach((unlisten) => unlisten());
        input.dispose();
        binaryInput.dispose();
        fitNow.current = null;
        terminal.current = null;
        term.dispose();
      };
      const unlistenOutput = await listen<{ terminalId: string; data: number[] }>("terminal-output", ({ payload }) => receive(payload));
      if (disposed) { unlistenOutput(); return; }
      unlisteners.push(unlistenOutput);
      const unlistenExit = await listen<{ terminalId: string }>("terminal-exit", ({ payload }) => receive(payload));
      if (disposed) { unlistenExit(); return; }
      unlisteners.push(unlistenExit);
      const resize = () => {
        if (disposed || !host.current?.clientWidth || !host.current.clientHeight) return;
        fit.fit();
        if (id) void invoke("terminal_resize", { terminalId: id, cols: term.cols, rows: term.rows }).catch(() => {});
      };
      resize();
      id = await invoke<string>("terminal_start", { cwd, cols: term.cols, rows: term.rows });
      if (disposed) { await invoke("terminal_close", { terminalId: id }); return; }
      pending.splice(0).forEach(receive);
      fitNow.current = resize;
      observer = new ResizeObserver(resize);
      observer.observe(host.current!);
      resize();
      term.focus();
    };
    setError(null);
    setExited(false);
    void start().catch((reason) => {
      cleanup();
      if (!disposed) setError(String(reason));
    });
    return () => {
      disposed = true;
      cleanup();
      if (id) void import("@tauri-apps/api/core")
        .then(({ invoke }) => invoke("terminal_close", { terminalId: id })).catch(() => {});
    };
  }, [cwd, generation]);

  useEffect(() => {
    if (!visible) return;
    const frame = requestAnimationFrame(() => { fitNow.current?.(); terminal.current?.focus(); });
    return () => cancelAnimationFrame(frame);
  }, [visible]);

  const clampHeight = (value: number) => Math.max(120, Math.min(value, window.innerHeight * 0.6));
  return (
    <section className="desktop-terminal-panel" aria-label={t("console.title")} style={{ display: visible ? "flex" : "none", flexDirection: "column", flexShrink: 0, height, maxHeight: "60%", minHeight: 120, background: "#111418", overflow: "hidden" }}
      onKeyDown={(event) => event.stopPropagation()}>
      <style>{`.desktop-terminal-panel > div > button { cursor: pointer; min-width: 28px; height: 28px; padding: 0 6px; border: none; border-radius: 4px; background: transparent; color: var(--text-muted); } .desktop-terminal-panel > div > button:hover { background: var(--bg-hover); color: var(--text); }`}</style>
      <div role="separator" tabIndex={0} aria-label={t("console.resize")} aria-orientation="horizontal" aria-valuenow={height} aria-valuemin={120}
        onPointerDown={(event) => { drag.current = { y: event.clientY, height }; event.currentTarget.setPointerCapture(event.pointerId); }}
        onPointerMove={(event) => { if (drag.current) setHeight(clampHeight(drag.current.height + drag.current.y - event.clientY)); }}
        onPointerUp={() => { drag.current = null; }} onPointerCancel={() => { drag.current = null; }}
        onKeyDown={(event) => { if (["ArrowUp", "ArrowDown"].includes(event.key)) { event.preventDefault(); setHeight(clampHeight(height + (event.key === "ArrowUp" ? 20 : -20))); } }}
        style={{ height: 5, flexShrink: 0, cursor: "row-resize", touchAction: "none", background: "var(--border)" }} />
      <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "5px 10px", background: "var(--bg-panel)", color: "var(--text-muted)", fontSize: 12 }}>
        <span>{t("console.title")}</span>
        <span title={cwd} style={{ flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontFamily: "var(--font-mono)", fontSize: 11 }}>{cwd}</span>
        {(exited || error) && <button type="button" onClick={() => setGeneration((value) => value + 1)}>{t("console.restart")}</button>}
        <button type="button" onClick={() => terminal.current?.clear()} title={t("console.clear")} aria-label={t("console.clear")}>⌫</button>
        <button type="button" onClick={onMinimize} title={t("console.minimize")} aria-label={t("console.minimize")}>—</button>
        <button type="button" onClick={onClose} title={t("console.close")} aria-label={t("console.close")}>×</button>
      </div>
      {!desktop && <div style={{ padding: 16, color: "#adb8c5" }}>{t("console.desktopOnly")}</div>}
      {error && <div role="alert" style={{ padding: 8, color: "#fca5a5" }}>
        {/not allowed|plugin not found|command .*not found/i.test(error) && (
          <p style={{ margin: "0 0 8px" }}>{t("console.backendUnavailable")}</p>
        )}
        {error}
      </div>}
      {exited && <div role="status" style={{ padding: 8, color: "#adb8c5" }}>{t("console.exited")}</div>}
      <div ref={host} style={{ flex: 1, minHeight: 0, padding: "4px 8px", overflow: "hidden" }} />
    </section>
  );
}
