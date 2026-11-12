type AgentDiagnosticLevel = "error" | "warn";

const MAX_STRING_LENGTH = 2_000;
const MAX_COLLECTION_LENGTH = 50;

function redactSensitiveText(value: string): string {
  return value
    .replace(/\bsk-[A-Za-z0-9_-]{8,}\b/g, "sk-<redacted>")
    .replace(/\bBearer\s+[A-Za-z0-9._~+/=-]{8,}/gi, "Bearer <redacted>")
    .replace(/([?&](?:api[_-]?key|access[_-]?token|token)=)[^&\s]+/gi, "$1<redacted>");
}

function sanitizeString(value: string): string {
  const redacted = redactSensitiveText(value);
  if (redacted.length <= MAX_STRING_LENGTH) return redacted;
  return `${redacted.slice(0, MAX_STRING_LENGTH)}...[truncated ${redacted.length - MAX_STRING_LENGTH} chars]`;
}

function sanitizeValue(value: unknown, depth = 0): unknown {
  if (typeof value === "string") return sanitizeString(value);
  if (value === null || typeof value === "number" || typeof value === "boolean") return value;
  if (value === undefined) return undefined;
  if (depth >= 3) return "[nested value omitted]";
  if (Array.isArray(value)) {
    return value.slice(0, MAX_COLLECTION_LENGTH).map((item) => sanitizeValue(item, depth + 1));
  }
  if (typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value)
        .slice(0, MAX_COLLECTION_LENGTH)
        .map(([key, item]) => [key, sanitizeValue(item, depth + 1)]),
    );
  }
  return sanitizeString(String(value));
}

export function diagnosticErrorMessage(error: unknown): string {
  return sanitizeString(error instanceof Error ? error.message : String(error));
}

export function logAgentDiagnostic(
  level: AgentDiagnosticLevel,
  event: string,
  details: Record<string, unknown> = {},
): void {
  const payload = sanitizeValue({
    timestamp: new Date().toISOString(),
    event,
    ...details,
  });
  const line = `[pi-desktop] agent_diagnostic ${JSON.stringify(payload)}`;
  if (level === "error") console.error(line);
  else console.warn(line);
}
