"use client";

import { useId, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { GitCommitHorizontal, RotateCcw } from "lucide-react";
import { useI18n } from "@/hooks/useI18n";
import { shortRef, type GitHistorySummary } from "@/lib/git-history";
import styles from "./GitHistoryActions.module.css";

export interface HistoryActionAnchor {
  trigger: HTMLButtonElement;
  x: number;
  y: number;
}

export interface CommitActionAnchor extends HistoryActionAnchor {
  commit: GitHistorySummary;
}

export interface CommitActionTarget {
  commit: GitHistorySummary;
  branch: string;
  head: string;
}

const RESET_MODES = ["soft", "mixed", "hard", "keep"] as const;
export type GitResetMode = typeof RESET_MODES[number];

export function CommitActionMenu({ anchor, canWrite, onClose, onCherryPick, onReset }: {
  anchor: CommitActionAnchor;
  canWrite: boolean;
  onClose: (restoreFocus: boolean) => void;
  onCherryPick: () => void;
  onReset: () => void;
}) {
  const { t } = useI18n();
  const isMerge = anchor.commit.parents.length > 1;
  return <HistoryActionMenu anchor={anchor} label={t("gitHistory.commitActions")} onClose={onClose}>
    <button type="button" role="menuitem" tabIndex={-1} disabled={!canWrite || isMerge} onClick={onCherryPick} title={isMerge ? t("gitHistory.cherryPickMerge") : undefined}>
      <GitCommitHorizontal size={14} aria-hidden="true" />{t("gitHistory.cherryPick")}
    </button>
    <button type="button" role="menuitem" tabIndex={-1} disabled={!canWrite} onClick={onReset}>
      <RotateCcw size={14} aria-hidden="true" />{t("gitHistory.resetMenu")}
    </button>
    {!canWrite && <p className={styles.menuHint}>{t("gitHistory.checkoutRequired")}</p>}
  </HistoryActionMenu>;
}

export function HistoryActionMenu({ anchor, label, wide, onClose, children }: {
  anchor: HistoryActionAnchor;
  label: string;
  wide?: boolean;
  onClose: (restoreFocus: boolean) => void;
  children: ReactNode;
}) {
  const menu = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const element = menu.current;
    if (!element) return;
    const { width, height } = element.getBoundingClientRect();
    element.style.left = `${Math.max(6, Math.min(anchor.x, window.innerWidth - width - 6))}px`;
    element.style.top = `${Math.max(6, Math.min(anchor.y, window.innerHeight - height - 6))}px`;
    (element.querySelector<HTMLButtonElement>("button:not(:disabled)") ?? element).focus({ preventScroll: true });
    const outside = (event: PointerEvent) => { if (!element.contains(event.target as Node)) onClose(false); };
    const dismiss = () => onClose(true);
    const onScroll = (event: Event) => {
      // 超长分支名需要在菜单内部滚动，只有外部滚动才关闭菜单。
      if (event.target !== element) dismiss();
    };
    document.addEventListener("pointerdown", outside, true);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", dismiss);
    return () => {
      document.removeEventListener("pointerdown", outside, true);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", dismiss);
    };
  }, [anchor, onClose]);
  return createPortal(
    <div ref={menu} className={styles.menu} data-wide={wide || undefined} role="menu" tabIndex={-1} aria-label={label}
      data-git-history-action onContextMenu={(event) => event.preventDefault()} onKeyDown={(event) => {
        if (event.key === "Escape" || event.key === "Tab") {
          event.preventDefault();
          event.stopPropagation();
          onClose(true);
          return;
        }
        if (!["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) return;
        event.preventDefault();
        const items = Array.from(menu.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? []);
        const current = items.findIndex((item) => item === document.activeElement);
        const next = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1 : (current + (event.key === "ArrowUp" ? -1 : 1) + items.length) % items.length;
        items[next]?.focus();
      }}>
      {children}
    </div>, document.body,
  );
}

export function CommitResetDialog({ target, busy, error, onCancel, onConfirm }: {
  target: CommitActionTarget;
  busy: boolean;
  error: string | null;
  onCancel: () => void;
  onConfirm: (mode: GitResetMode) => void;
}) {
  const { t } = useI18n();
  const titleId = useId();
  const dialog = useRef<HTMLDialogElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const [mode, setMode] = useState<GitResetMode>("mixed");
  useLayoutEffect(() => {
    const element = dialog.current;
    element?.showModal();
    cancel.current?.focus({ preventScroll: true });
    return () => { if (element?.open) element.close(); };
  }, []);

  return createPortal(
    <dialog ref={dialog} className={styles.dialog} data-git-history-action aria-labelledby={titleId}
      onKeyDown={(event) => { if (event.key === "Escape") event.stopPropagation(); }}
      onCancel={(event) => { event.preventDefault(); if (!busy) onCancel(); }}>
      <form onSubmit={(event) => { event.preventDefault(); if (!busy) onConfirm(mode); }}>
        <h2 id={titleId}>{t("gitHistory.resetTitle")}</h2>
        <p className={styles.target}>
          <strong>{shortRef(target.branch)} → <code title={target.commit.hash}>{target.commit.hash.slice(0, 8)}</code></strong>
          <span>{target.commit.subject || t("gitHistory.noSubject")}</span>
          {target.commit.author && <span className={styles.muted}>{target.commit.author}</span>}
        </p>
        <fieldset disabled={busy}>
          <legend>{t("gitHistory.resetIntro")}</legend>
          {RESET_MODES.map((item) => <label className={styles.mode} key={item}>
            <input type="radio" name="reset-mode" value={item} checked={mode === item} onChange={() => setMode(item)} />
            <span><strong>{t(`gitHistory.resetMode.${item}`)}</strong><small>{t(`gitHistory.resetNote.${item}`)}</small></span>
          </label>)}
        </fieldset>
        {mode === "hard" && <p className={styles.warning}>{t("gitHistory.resetWarning")}</p>}
        {error && <p className={styles.error} role="alert">{error}</p>}
        <div className={styles.actions}>
          <button ref={cancel} type="button" disabled={busy} onClick={onCancel}>{t("sidebar.cancel")}</button>
          <button type="submit" data-primary data-danger={mode === "hard"} disabled={busy}>
            {t(busy ? "gitHistory.resetBusy" : "gitHistory.reset")}
          </button>
        </div>
      </form>
    </dialog>, document.body,
  );
}
