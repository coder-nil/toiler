"use client";

import McpIcon from "@lobehub/icons/es/MCP/components/Mono";
import { parseAnsiLine, stripAnsi } from "@/lib/ansi";
import {
  TASK_PROGRESS_STATUS_KEY,
  TASK_PROGRESS_WIDGET_KEY,
} from "@/lib/task-progress-view";
import type { ExtensionStatusItem, ExtensionWidgetItem } from "@/lib/types";
import { ExtensionWidgets } from "./ExtensionWidgets";

export function sanitizeExtensionStatusText(text: string): string {
  return text
    .replace(/[\r\n\t]/g, " ")
    .replace(/ +/g, " ")
    .trim();
}

export function formatExtensionStatusLine(statuses: ExtensionStatusItem[]): string {
  return [...statuses]
    .sort((a, b) => a.key.localeCompare(b.key))
    .map(({ text }) => sanitizeExtensionStatusText(text))
    .join(" ");
}

const MCP_STATUS_PREFIX = /^🔌\s*/u;

type ExtensionStatusPart = {
  key: string;
  text: string;
  icon: "mcp" | null;
};

export function formatExtensionStatusParts(statuses: ExtensionStatusItem[]): ExtensionStatusPart[] {
  return [...statuses]
    .sort((a, b) => a.key.localeCompare(b.key))
    .map(({ key, text }) => {
      const sanitized = sanitizeExtensionStatusText(text);
      const plain = stripAnsi(sanitized);
      const isMcpStatus = MCP_STATUS_PREFIX.test(plain);
      return {
        key,
        text: isMcpStatus ? sanitized.replace(MCP_STATUS_PREFIX, "") : sanitized,
        icon: isMcpStatus ? "mcp" : null,
      };
    });
}

function ExtensionStatusText({ parts }: { parts: ExtensionStatusPart[] }) {
  return (
    <span className="extension-status-text">
      {parts.map((part, partIndex) => (
        <span key={part.key} className="extension-status-part">
          {partIndex > 0 && <span aria-hidden="true"> </span>}
          {part.icon === "mcp" && <McpIcon size={14} className="extension-status-icon" aria-hidden="true" />}
          {parseAnsiLine(part.text).map((segment, segmentIndex) => (
            <span key={segmentIndex} style={segment.style}>{segment.text}</span>
          ))}
        </span>
      ))}
    </span>
  );
}

export function ExtensionStatusBar({
  statuses,
  widgets = [],
}: {
  statuses: ExtensionStatusItem[];
  widgets?: ExtensionWidgetItem[];
}) {
  // 任务进度有自己的面板（`TaskProgressPanel`，贴在输入框上方），
  // 这里只留其余扩展的状态与 widget；否则同一条清单会在两处各画一次。
  const visibleStatuses = statuses.filter(({ key }) => key !== TASK_PROGRESS_STATUS_KEY);
  const visibleWidgets = widgets.filter(({ key }) => key !== TASK_PROGRESS_WIDGET_KEY);

  if (visibleStatuses.length === 0 && visibleWidgets.length === 0) return null;

  const statusParts = formatExtensionStatusParts(visibleStatuses);
  const statusLine = statusParts.map(({ text }) => stripAnsi(text)).join(" ");

  return (
    <div
      className={`extension-status-shelf${visibleWidgets.length > 0 ? " has-widgets" : ""}${statusParts.length > 0 ? " has-status" : ""}`}
    >
      {visibleWidgets.length > 0 && <ExtensionWidgets widgets={visibleWidgets} />}
      {statusParts.length > 0 && (
        <div
          role="status"
          className="extension-status-line"
          aria-label={statusLine}
          title={statusLine}
        >
          <ExtensionStatusText parts={statusParts} />
        </div>
      )}
    </div>
  );
}
