"use client";

import { useEffect, useState, type ReactNode } from "react";
import { ChevronRight } from "lucide-react";
import { useI18n } from "@/hooks/useI18n";

/**
 * 手机端的「处理详情」折叠组。
 *
 * 文案和交互与桌面 `ChatWindow` 的 `ProcessDetailsGroup` 保持一致
 * （`chat.processDetails` / `chat.message(s)` / `chat.toolCall(s)` + 同一个 chevron，
 * 摘要用 ` · ` 连接），默认状态不同：手机屏小，**在跑的时候展开、跑完收起**。
 * 正在跑的那一轮传 `running`，跑完的那一刻这组自己折回去；历史轮默认收起。
 * 折叠组里没有任何内容时不会渲染这一行。
 */

interface MobileProcessDetailsProps {
  /** 折起来的消息条数；为 0 时摘要里不出现「N 条消息」。 */
  messageCount: number;
  /** 工具调用次数；为 0 时摘要里不出现「N 次工具调用」。 */
  toolCallCount: number;
  /** 默认是否展开。历史轮一律默认收起，运行中的那一轮不用传——传 `running` 就够了。 */
  defaultExpanded?: boolean;
  /** 这一轮是否还在跑：在跑就展开，跑完（true→false）就收起。 */
  running?: boolean;
  children: ReactNode;
}

export function MobileProcessDetails({
  messageCount,
  toolCallCount,
  defaultExpanded = false,
  running = false,
  children,
}: MobileProcessDetailsProps) {
  const { t } = useI18n();
  const [expanded, setExpanded] = useState(defaultExpanded || running);

  // 这就是「结束的时候收起」：只在 running 跳变时动手。
  // 用户手动展开的历史轮（running 一直是 false）不会被他新发的一轮收走。
  useEffect(() => {
    setExpanded(running);
  }, [running]);

  const parts = [t("chat.processDetails")];
  // 桌面会无条件带上消息条数；手机上「0 条消息」读起来是错的，所以两边都为 0 时只留标题。
  if (messageCount > 0) parts.push(`${messageCount} ${t(messageCount === 1 ? "chat.message" : "chat.messages")}`);
  if (toolCallCount > 0) parts.push(`${toolCallCount} ${t(toolCallCount === 1 ? "chat.toolCall" : "chat.toolCalls")}`);

  return (
    <div style={{ display: "flex", flexDirection: "column", minWidth: 0 }}>
      <button
        type="button"
        className="mobile-tap"
        aria-expanded={expanded}
        onClick={() => setExpanded((value) => !value)}
        title={expanded ? t("chat.collapseProcess") : t("chat.expandProcess")}
        style={{
          display: "flex", alignItems: "center", gap: 6, alignSelf: "flex-start", maxWidth: "100%",
          minHeight: 40, padding: "0 6px 0 0", marginLeft: -2,
          border: "none", background: "none", color: "var(--text-muted)",
          cursor: "pointer", fontSize: 12, textAlign: "left",
        }}
      >
        <ChevronRight
          size={13}
          aria-hidden="true"
          className="mobile-disclosure-chevron"
          style={{ flexShrink: 0, transform: expanded ? "rotate(90deg)" : "none" }}
        />
        <span style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          {parts.join(" · ")}
        </span>
      </button>
      {expanded && (
        <div style={{ display: "flex", flexDirection: "column", gap: 12, marginTop: 8 }}>
          {children}
        </div>
      )}
    </div>
  );
}
