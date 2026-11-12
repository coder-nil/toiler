import type { InlineExtension } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

const SYSTEM_TIME_EXTENSION_NAME = "pi-desktop-system-time";

type SystemTime = {
  date: string;
  time: string;
  weekday: string;
  timeZone: string;
  utcOffset: string;
  iso8601: string;
};

function formatParts(date: Date, options: Intl.DateTimeFormatOptions): Record<string, string> {
  return Object.fromEntries(
    new Intl.DateTimeFormat("en-US", options)
      .formatToParts(date)
      .filter((part) => part.type !== "literal")
      .map((part) => [part.type, part.value]),
  );
}

export function getSystemTime(now = new Date()): SystemTime {
  const dateParts = formatParts(now, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    weekday: "long",
  });
  const timeParts = formatParts(now, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hourCycle: "h23",
  });
  const offsetMinutes = -now.getTimezoneOffset();
  const offsetSign = offsetMinutes >= 0 ? "+" : "-";
  const offsetHours = String(Math.floor(Math.abs(offsetMinutes) / 60)).padStart(2, "0");
  const offsetRemainder = String(Math.abs(offsetMinutes) % 60).padStart(2, "0");

  return {
    date: `${dateParts.year}-${dateParts.month}-${dateParts.day}`,
    time: `${timeParts.hour}:${timeParts.minute}:${timeParts.second}`,
    weekday: dateParts.weekday,
    timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone,
    utcOffset: `${offsetSign}${offsetHours}:${offsetRemainder}`,
    iso8601: now.toISOString(),
  };
}

/** Registers the host clock as an always-available, read-only Pi tool. */
export function createSystemTimeExtension(): InlineExtension {
  return {
    name: SYSTEM_TIME_EXTENSION_NAME,
    hidden: true,
    factory: (pi) => {
      pi.registerTool({
        name: "system_time",
        label: "System Time",
        description: "Get the current date and time from the Pi Desktop host system. Use this before answering questions involving today, tomorrow, yesterday, relative dates, weekdays, deadlines, or the current time.",
        promptSnippet: "read the current host date, time, weekday, and time zone",
        promptGuidelines: [
          "Call system_time before resolving references such as today, tomorrow, yesterday, this week, or a relative deadline. Do not infer the current date from model knowledge.",
        ],
        parameters: Type.Object({}),
        async execute() {
          return {
            content: [{ type: "text", text: JSON.stringify(getSystemTime()) }],
            details: {},
          };
        },
      });
    },
  };
}
