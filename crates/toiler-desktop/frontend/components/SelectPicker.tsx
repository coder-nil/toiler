"use client";

import { useEffect, useId, useRef, useState, type CSSProperties } from "react";

type SelectPickerProps = {
  options: readonly string[];
  value: string;
  placeholder: string;
  ariaLabel: string;
  onChange: (value: string) => void;
  disabled?: boolean;
  placement?: "above" | "below";
  emptyOptionLabel?: string;
  style?: CSSProperties;
};

export function SelectPicker({ options, value, placeholder, ariaLabel, onChange, disabled = false, placement = "below", emptyOptionLabel, style }: SelectPickerProps) {
  const [open, setOpen] = useState(false);
  const pickerRef = useRef<HTMLDivElement>(null);
  const listboxId = useId();
  const inactive = disabled || options.length === 0;

  useEffect(() => {
    const close = (event: MouseEvent) => {
      if (!pickerRef.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, []);

  const select = (nextValue: string) => {
    onChange(nextValue);
    setOpen(false);
  };

  return (
    <div ref={pickerRef} style={{ position: "relative", minWidth: 0, flex: 1 }}>
      <button type="button" role="combobox" aria-controls={listboxId} aria-expanded={open} aria-haspopup="listbox" aria-label={ariaLabel} disabled={inactive} onClick={() => setOpen((current) => !current)} onKeyDown={(event) => { if (event.key === "Escape") setOpen(false); }} onMouseEnter={(event) => { if (!inactive && !open) event.currentTarget.style.background = "var(--bg-hover)"; }} onMouseLeave={(event) => { if (!open) event.currentTarget.style.background = "var(--bg-panel)"; }} style={{ width: "100%", height: 30, display: "flex", alignItems: "center", gap: 7, padding: "0 8px", border: "none", borderRadius: 5, background: open ? "var(--bg-hover)" : "var(--bg-panel)", color: value ? "var(--text)" : "var(--text-dim)", cursor: inactive ? "not-allowed" : "pointer", opacity: inactive ? .58 : 1, fontFamily: "var(--font-mono)", fontSize: 12, textAlign: "left", ...style }}>
        <span style={{ minWidth: 0, flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{value || placeholder}</span>
        <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" style={{ flexShrink: 0, transform: open ? "rotate(180deg)" : "none", transition: "transform .12s" }}><polyline points="2 3.5 5 6.5 8 3.5" /></svg>
      </button>
      {open && <div id={listboxId} role="listbox" aria-label={ariaLabel} style={{ position: "absolute", right: 0, ...(placement === "above" ? { bottom: "calc(100% + 4px)" } : { top: "calc(100% + 4px)" }), left: 0, zIndex: 1201, maxHeight: 192, overflowY: "auto", padding: 4, border: "1px solid var(--border)", borderRadius: 6, background: "var(--bg-panel)", boxShadow: "0 6px 18px rgba(0,0,0,.16)" }}>
        {emptyOptionLabel && <PickerOption value="" label={emptyOptionLabel} selected={!value} onSelect={select} />}
        {options.map((option) => <PickerOption key={option} value={option} label={option} selected={option === value} onSelect={select} />)}
      </div>}
    </div>
  );
}

function PickerOption({ value, label, selected, onSelect }: { value: string; label: string; selected: boolean; onSelect: (value: string) => void }) {
  return <button type="button" role="option" aria-selected={selected} onClick={() => onSelect(value)} style={{ width: "100%", height: 28, padding: "0 8px", border: "none", borderRadius: 4, background: selected ? "var(--bg-hover)" : "transparent", color: "var(--text)", cursor: "pointer", fontFamily: "var(--font-mono)", fontSize: 12, textAlign: "left" }} onMouseEnter={(event) => { event.currentTarget.style.background = "var(--bg-hover)"; }} onMouseLeave={(event) => { event.currentTarget.style.background = selected ? "var(--bg-hover)" : "transparent"; }}>{label}</button>;
}
