"use client";

import { useEffect, useState } from "react";
import { useI18n } from "@/hooks/useI18n";
import { MOBILE_RADIUS } from "@/lib/mobile-timeline";

/**
 * 手机端的「扩展在等你回答」卡片。
 *
 * `ask_user` 就是走这条路径问问题的：扩展调用 `ctx.ui.select/confirm/input/editor`，
 * 服务端发出 `extension_ui_request` 并一直等，直到有人回答。手机端原来完全不认这个事件，
 * 所以一旦模型在手机上问问题，这一轮就会一直挂着。这张卡片把请求画出来并回传答案，
 * 答案经 `POST /api/agent/[id] { type: "extension_ui_response" }` 回到同一个会话，
 * 因此桌面端与手机端拿到的是同一个答案（谁先答谁生效，另一边的弹窗由
 * `extension_ui_resolved` 收掉）。
 *
 * `custom` 这类任意扩展界面手机上无法渲染，卡片只显示标题并提示去桌面端处理，
 * 但保留取消入口，免得整轮卡死。
 */

export interface MobileUiRequest {
  id: string;
  method: "select" | "confirm" | "input" | "editor" | "custom";
  title: string;
  message?: string;
  options?: string[];
  placeholder?: string;
  prefill?: string;
  sensitive?: boolean;
  lines?: string[];
  /** 服务端给的截止时间（毫秒时间戳）；没有就表示一直等。 */
  expiresAt?: number;
}

type PromptResponse = { value: string } | { confirmed: boolean } | { cancelled: true };

interface MobilePromptCardProps {
  request: MobileUiRequest;
  /** 回答这一条请求；由手机页统一走 agent 命令接口。 */
  onRespond: (request: MobileUiRequest, response: PromptResponse) => void;
  busy?: boolean;
}

export function MobilePromptCard({ request, onRespond, busy = false }: MobilePromptCardProps) {
  const { t } = useI18n();
  const [value, setValue] = useState(request.method === "editor" ? request.prefill ?? "" : "");
  // 选择题永远多带一项「自定义答案」：模型给的选项再多也可能不是用户想说的那个词，
  // 没有这条出口时手机上只能取消，整轮就卡在取消上。
  const [customAnswer, setCustomAnswer] = useState(false);

  // 换到另一条请求（同一会话连续问两个问题）时重置输入。
  useEffect(() => {
    setValue(request.method === "editor" ? request.prefill ?? "" : "");
    setCustomAnswer(false);
  }, [request.id, request.method, request.prefill]);

  const options = request.options ?? [];
  const customValue = value.trim();
  const submit = () => {
    if (request.method === "select") {
      if (!customValue) return;
      onRespond(request, { value: customValue });
      return;
    }
    onRespond(request, { value });
  };
  const cancel = () => onRespond(request, { cancelled: true });

  return (
    <div
      role="alertdialog"
      aria-label={request.title}
      style={{
        flexShrink: 0, margin: "0 14px 10px", padding: "10px 12px",
        borderRadius: MOBILE_RADIUS.field, border: "1px solid color-mix(in srgb, var(--accent) 45%, var(--border))",
        background: "color-mix(in srgb, var(--accent) 6%, var(--bg))",
        display: "flex", flexDirection: "column", gap: 8,
      }}
    >
      <div style={{ display: "flex", alignItems: "baseline", gap: 8, minWidth: 0 }}>
        <span style={{ flex: 1, minWidth: 0, fontSize: 13, fontWeight: 600, color: "var(--text)", overflowWrap: "anywhere" }}>
          {request.title}
        </span>
        <span style={{ flexShrink: 0, fontFamily: "var(--font-mono)", fontSize: 10, color: "var(--text-muted)" }}>
          {t("chat.extensionRequest")}
        </span>
      </div>

      {request.method === "confirm" && request.message && (
        <p style={{ margin: 0, fontSize: 13, lineHeight: 1.6, whiteSpace: "pre-wrap", overflowWrap: "anywhere", color: "var(--text-muted)" }}>
          {request.message}
        </p>
      )}

      {request.method === "custom" && (
        <>
          {request.lines && request.lines.length > 0 && (
            <pre style={{ margin: 0, maxHeight: 180, overflow: "auto", padding: "7px 9px", borderRadius: MOBILE_RADIUS.chip, background: "var(--bg-panel)", color: "var(--text-muted)", fontFamily: "var(--font-mono)", fontSize: 11.5, lineHeight: 1.5, whiteSpace: "pre-wrap" }}>
              {request.lines.join("\n")}
            </pre>
          )}
          <span style={{ fontSize: 11, lineHeight: 1.5, color: "var(--text-muted)" }}>{t("mobile.promptCustomHint")}</span>
        </>
      )}

      {request.method === "select" && (
        <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          {options.map((option) => (
            <button
              key={option}
              type="button"
              className="mobile-tap"
              disabled={busy}
              onClick={() => onRespond(request, { value: option })}
              style={{
                minHeight: 40, padding: "8px 11px", borderRadius: MOBILE_RADIUS.chip,
                border: "1px solid var(--border)", background: "var(--bg-panel)",
                color: "var(--text)", fontSize: 13, textAlign: "left", cursor: busy ? "default" : "pointer",
                overflowWrap: "anywhere",
              }}
            >
              {option}
            </button>
          ))}
          {customAnswer ? (
            <input
              type="text"
              autoFocus
              value={value}
              placeholder={t("chat.customAnswer")}
              aria-label={t("chat.customAnswer")}
              autoComplete="off"
              onChange={(event) => setValue(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") submit();
              }}
              style={{
                minHeight: 40, padding: "8px 11px", borderRadius: MOBILE_RADIUS.chip,
                border: "1px solid var(--accent)", background: "var(--bg)",
                color: "var(--text)", outline: "none", fontFamily: "inherit", fontSize: 16,
              }}
            />
          ) : (
            <button
              type="button"
              className="mobile-tap"
              disabled={busy}
              onClick={() => setCustomAnswer(true)}
              style={{
                minHeight: 40, padding: "8px 11px", borderRadius: MOBILE_RADIUS.chip,
                border: "1px dashed var(--border)", background: "none",
                color: "var(--text-muted)",
                fontSize: 13, textAlign: "left", cursor: busy ? "default" : "pointer",
              }}
            >
              {t("chat.customAnswer")}
            </button>
          )}
        </div>
      )}

      {request.method === "input" && (
        <input
          type={request.sensitive ? "password" : "text"}
          value={value}
          placeholder={request.placeholder}
          autoComplete="off"
          onChange={(event) => setValue(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") submit();
          }}
          style={{
            minHeight: 40, padding: "8px 11px", borderRadius: MOBILE_RADIUS.chip,
            border: "1px solid var(--border)", background: "var(--bg-panel)",
            color: "var(--text)", outline: "none", fontFamily: "inherit", fontSize: 16,
          }}
        />
      )}

      {request.method === "editor" && (
        <textarea
          value={value}
          rows={4}
          onChange={(event) => setValue(event.target.value)}
          style={{
            minHeight: 96, padding: "8px 11px", borderRadius: MOBILE_RADIUS.chip,
            border: "1px solid var(--border)", background: "var(--bg-panel)",
            color: "var(--text)", outline: "none", resize: "vertical",
            fontFamily: "var(--font-mono)", fontSize: 16, lineHeight: 1.5,
          }}
        />
      )}

      <div style={{ display: "flex", alignItems: "center", justifyContent: "flex-end", gap: 8 }}>
        <button
          type="button"
          className="mobile-tap"
          disabled={busy}
          onClick={cancel}
          style={{
            minHeight: 36, padding: "0 12px", borderRadius: MOBILE_RADIUS.round,
            border: "1px solid var(--border)", background: "none", color: "var(--text-muted)",
            fontSize: 12, cursor: busy ? "default" : "pointer",
          }}
        >
          {t("chat.cancel")}
        </button>
        {request.method !== "select" && request.method !== "custom" && (
          <button
            type="button"
            className="mobile-tap"
            disabled={busy}
            onClick={() => (request.method === "confirm" ? onRespond(request, { confirmed: true }) : submit())}
            style={{
              minHeight: 36, padding: "0 14px", borderRadius: MOBILE_RADIUS.round,
              border: "none", background: "var(--accent)", color: "var(--bg)",
              fontSize: 12, fontWeight: 600, cursor: busy ? "default" : "pointer",
            }}
          >
            {request.method === "confirm" ? t("chat.confirm") : t("chat.submit")}
          </button>
        )}
        {request.method === "select" && customAnswer && (
          // 点选项就是答案；只有走自定义路径才需要再点一次提交。
          <button
            type="button"
            className="mobile-tap"
            disabled={busy || !customValue}
            onClick={submit}
            style={{
              minHeight: 36, padding: "0 14px", borderRadius: MOBILE_RADIUS.round,
              border: "none", background: "var(--accent)", color: "var(--bg)",
              fontSize: 12, fontWeight: 600,
              cursor: busy || !customValue ? "default" : "pointer",
              opacity: busy || !customValue ? 0.5 : 1,
            }}
          >
            {t("chat.submit")}
          </button>
        )}
      </div>
    </div>
  );
}
