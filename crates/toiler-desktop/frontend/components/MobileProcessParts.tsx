"use client";

import { useState } from "react";
import { ChevronRight, Wrench } from "lucide-react";
import { useI18n } from "@/hooks/useI18n";
import { MOBILE_RADIUS } from "@/lib/mobile-timeline";
import type { MobileToolCall } from "@/lib/mobile-state";

/**
 * 处理详情里的两块「过程」内容：思考过程与工具调用。
 *
 * 两者都住在 `MobileProcessDetails` 折叠组里：运行中该组默认展开（能看着它长），
 * 一轮结束后整组收起，所以这里不再多做一层开关，只负责把内容摆清楚：
 *   - 思考用一道浅色左线 + 「思考」标签，正文用 `--text-muted`；
 *   - 工具调用每条一行（名称 + 摘要），有入参/结果的可以单独展开看细节。
 */

/** 思考过程：随流式一起长，结束后跟着折叠组一起收起。 */
export function MobileThinking({ text }: { text: string }) {
  const { t } = useI18n();

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 3, minWidth: 0, paddingLeft: 9, borderLeft: "2px solid color-mix(in srgb, var(--accent) 32%, var(--border))" }}>
      <span style={{ fontSize: 11, color: "var(--text-muted)" }}>{t("i18n.thinking")}</span>
      <p style={{ margin: 0, fontSize: 12.5, lineHeight: 1.65, whiteSpace: "pre-wrap", overflowWrap: "anywhere", color: "var(--text-muted)" }}>
        {text}
      </p>
    </div>
  );
}

/** 一条工具调用：摘要一行；有入参/结果时可展开。 */
function MobileToolRow({ call }: { call: MobileToolCall }) {
  const [expanded, setExpanded] = useState(false);
  const hasDetail = Boolean(call.input || call.output);
  const nameColor = call.isError ? "var(--danger)" : "var(--text)";

  const row = (
    <>
      <Wrench size={11} aria-hidden="true" style={{ flexShrink: 0 }} />
      <span style={{ flexShrink: 0, fontFamily: "var(--font-mono)", fontSize: 12, color: nameColor }}>{call.name}</span>
      <span style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontFamily: "var(--font-mono)", fontSize: 12, color: "var(--text-muted)" }}>
        {call.hint}
      </span>
      {hasDetail && (
        <ChevronRight
          size={12}
          aria-hidden="true"
          className="mobile-disclosure-chevron"
          style={{ flexShrink: 0, transform: expanded ? "rotate(90deg)" : "none" }}
        />
      )}
    </>
  );

  const rowStyle = {
    display: "flex", alignItems: "center", gap: 6, minWidth: 0,
    minHeight: hasDetail ? 36 : 24, padding: "2px 0",
    color: "var(--text-muted)",
  } as const;

  return (
    <div style={{ minWidth: 0 }}>
      {hasDetail ? (
        <button
          type="button"
          className="mobile-tap"
          aria-expanded={expanded}
          onClick={() => setExpanded((value) => !value)}
          style={{ ...rowStyle, width: "100%", border: "none", background: "none", fontSize: 12, textAlign: "left", cursor: "pointer" }}
        >
          {row}
        </button>
      ) : (
        <div style={rowStyle}>{row}</div>
      )}
      {expanded && hasDetail && (
        <div style={{ margin: "4px 0 6px", borderRadius: MOBILE_RADIUS.chip, border: "1px solid var(--border)", overflow: "hidden" }}>
          {call.input && (
            <pre style={{ margin: 0, padding: "7px 9px", maxHeight: 220, overflow: "auto", background: "var(--bg-subtle)", color: "var(--text-muted)", fontFamily: "var(--font-mono)", fontSize: 11.5, lineHeight: 1.5, whiteSpace: "pre-wrap", wordBreak: "break-all" }}>
              {call.input}
            </pre>
          )}
          {call.output && (
            <pre style={{ margin: 0, padding: "7px 9px", maxHeight: 280, overflow: "auto", background: "var(--bg)", color: call.isError ? "var(--danger)" : "var(--text-muted)", borderTop: call.input ? "1px solid var(--border)" : "none", fontFamily: "var(--font-mono)", fontSize: 11.5, lineHeight: 1.5, whiteSpace: "pre-wrap", wordBreak: "break-all" }}>
              {call.output}
            </pre>
          )}
        </div>
      )}
    </div>
  );
}

/** 一条助手条目里的全部工具调用，左侧一道细线把它们和正文区分开。 */
export function MobileToolList({ calls }: { calls: readonly MobileToolCall[] }) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 2, minWidth: 0, paddingLeft: 9, borderLeft: "2px solid var(--border)" }}>
      {calls.map((call) => <MobileToolRow key={call.id} call={call} />)}
    </div>
  );
}
