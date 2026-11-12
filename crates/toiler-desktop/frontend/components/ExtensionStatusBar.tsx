"use client";

import McpIcon from "@lobehub/icons/es/MCP/components/Mono";
import { parseAnsiLine, stripAnsi } from "@/lib/ansi";
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
const TASK_PROGRESS_STATUS_KEY = "task-progress";
const TASK_PROGRESS_WIDGET_KEY = "任务进度";

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
  if (statuses.length === 0 && widgets.length === 0) return null;

  const taskStatusParts = formatExtensionStatusParts(
    statuses.filter(({ key }) => key === TASK_PROGRESS_STATUS_KEY),
  );
  const trailingStatusParts = formatExtensionStatusParts(
    statuses.filter(({ key }) => key !== TASK_PROGRESS_STATUS_KEY),
  );
  const taskStatusLine = taskStatusParts.map(({ text }) => stripAnsi(text)).join(" ");
  const trailingStatusLine = trailingStatusParts.map(({ text }) => stripAnsi(text)).join(" ");
  const hasTaskProgressWidget = taskStatusParts.length > 0
    && widgets.some(({ key }) => key === TASK_PROGRESS_WIDGET_KEY);
  const taskStatusContent = taskStatusParts.length > 0 ? (
    <span
      role="status"
      className="extension-task-status-line"
      aria-label={taskStatusLine}
      title={taskStatusLine}
    >
      <ExtensionStatusText parts={taskStatusParts} />
    </span>
  ) : null;

  return (
    <div
      className={`extension-status-shelf${widgets.length > 0 ? " has-widgets" : ""}${statuses.length > 0 ? " has-status" : ""}`}
    >
      {widgets.length > 0 && (
        <ExtensionWidgets
          widgets={widgets}
          trailingContent={hasTaskProgressWidget ? taskStatusContent : undefined}
        />
      )}
      {taskStatusContent && !hasTaskProgressWidget && (
        <div className="extension-task-status-fallback">
          {taskStatusContent}
        </div>
      )}
      {trailingStatusParts.length > 0 && (
        <div
          role="status"
          className="extension-status-line"
          aria-label={trailingStatusLine}
          title={trailingStatusLine}
        >
          <ExtensionStatusText parts={trailingStatusParts} />
        </div>
      )}
    </div>
  );
}
