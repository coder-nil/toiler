/**
 * Read a JSON API response without turning a server-side failure into a
 * `JSON.parse` message.
 *
 * Next.js answers an uncaught error in the request pipeline with
 * `500` + `text/plain "Internal Server Error"`. Calling `response.json()` on
 * that body throws `Unexpected token 'I', "Internal S"... is not valid JSON`,
 * which hides both the status and the fact that the request never reached the
 * route handler. This helper reads the body as text first: valid JSON keeps the
 * familiar behaviour, anything else is reported with its status and body.
 */
export interface ApiJsonOptions {
  /** Extra text appended to a non-JSON error, e.g. where to find server logs. */
  hint?: string;
}

const BODY_PREVIEW_CHARS = 200;

function bodyPreview(text: string): string {
  const collapsed = text.replace(/\s+/g, " ").trim();
  return collapsed.length > BODY_PREVIEW_CHARS
    ? `${collapsed.slice(0, BODY_PREVIEW_CHARS)}…`
    : collapsed;
}

function httpError(response: Response, text: string, hint?: string): Error {
  const preview = bodyPreview(text);
  const detail = preview && !/^(internal server error|error)$/i.test(preview) ? `: ${preview}` : "";
  return new Error(`HTTP ${response.status}${detail}${hint ? ` (${hint})` : ""}`);
}

/**
 * Parse `response` as JSON, throwing an `Error` whose message is safe to show
 * in the UI: `error` fields win, then the HTTP status, then the raw body.
 */
export async function readApiJson<T>(response: Response, options: ApiJsonOptions = {}): Promise<T> {
  const text = await response.text().catch(() => "");
  let data: T | null = null;
  let parsed = false;
  if (text.trim()) {
    try {
      data = JSON.parse(text) as T;
      parsed = true;
    } catch {
      // Not JSON: a proxy, an uncaught server error, or an HTML error page.
    }
  }

  if (!parsed) {
    if (!response.ok) throw httpError(response, text, options.hint);
    throw new Error(text.trim() ? `Unexpected non-JSON response: ${bodyPreview(text)}` : "Empty response");
  }

  const error = (data as { error?: unknown } | null)?.error;
  if (!response.ok || (typeof error === "string" && error)) {
    throw new Error(typeof error === "string" && error ? error : `HTTP ${response.status}`);
  }
  return data as T;
}
