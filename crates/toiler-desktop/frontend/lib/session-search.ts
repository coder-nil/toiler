import { createReadStream } from "fs";
import { createInterface } from "readline";
import type { SessionInfo } from "./types";

export interface SessionSearchMatch {
  sessionId: string;
  snippet: string;
}

const DEFAULT_LIMIT = 80;
const MAX_LIMIT = 200;
const SEARCH_CONCURRENCY = 8;

function normalizeText(value: string): string {
  return value.replace(/\s+/g, " ").trim();
}

function snippetFor(text: string, normalizedQuery: string): string | null {
  const normalized = normalizeText(text);
  const index = normalized.toLocaleLowerCase().indexOf(normalizedQuery);
  if (index < 0) return null;

  const radius = 58;
  const start = Math.max(0, index - radius);
  const end = Math.min(normalized.length, index + normalizedQuery.length + radius);
  return `${start > 0 ? "…" : ""}${normalized.slice(start, end)}${end < normalized.length ? "…" : ""}`;
}

function textBlocks(content: unknown): string[] {
  if (typeof content === "string") return [content];
  if (!Array.isArray(content)) return [];
  return content.flatMap((block) => {
    if (!block || typeof block !== "object") return [];
    const value = block as Record<string, unknown>;
    return value.type === "text" && typeof value.text === "string" ? [value.text] : [];
  });
}

function searchableEntryText(value: unknown): string[] {
  if (!value || typeof value !== "object") return [];
  const entry = value as Record<string, unknown>;

  if (entry.type === "compaction" && typeof entry.summary === "string") return [entry.summary];
  if (entry.type !== "message" || !entry.message || typeof entry.message !== "object") return [];

  const message = entry.message as Record<string, unknown>;
  if (message.role === "user" || message.role === "assistant") {
    return textBlocks(message.content);
  }
  if (message.role === "custom" && message.display !== false) {
    return textBlocks(message.content);
  }
  return [];
}

async function searchSessionFile(
  session: SessionInfo,
  normalizedQuery: string,
  signal?: AbortSignal,
): Promise<SessionSearchMatch | null> {
  if (signal?.aborted) return null;
  for (const text of [session.name, session.firstMessage, session.cwd]) {
    if (!text) continue;
    const snippet = snippetFor(text, normalizedQuery);
    if (snippet) return { sessionId: session.id, snippet };
  }

  if (!session.path || session.transient) return null;

  try {
    const input = createReadStream(session.path, { encoding: "utf8", signal });
    const lines = createInterface({
      input,
      crlfDelay: Infinity,
    });
    for await (const line of lines) {
      if (signal?.aborted) {
        lines.close();
        input.destroy();
        return null;
      }
      if (!line.includes("message") && !line.includes("compaction")) continue;
      let entry: unknown;
      try {
        entry = JSON.parse(line);
      } catch {
        continue;
      }
      for (const text of searchableEntryText(entry)) {
        const snippet = snippetFor(text, normalizedQuery);
        if (snippet) {
          lines.close();
          input.destroy();
          return { sessionId: session.id, snippet };
        }
      }
    }
  } catch {
    // A session may be removed while a search is running. Skip stale files.
  }
  return null;
}

export async function searchSessions(
  sessions: readonly SessionInfo[],
  query: string,
  requestedLimit = DEFAULT_LIMIT,
  signal?: AbortSignal,
): Promise<SessionSearchMatch[]> {
  const normalizedQuery = normalizeText(query).toLocaleLowerCase();
  if (!normalizedQuery) return [];

  const limit = Math.max(1, Math.min(MAX_LIMIT, Math.trunc(requestedLimit) || DEFAULT_LIMIT));
  const ordered = [...sessions].sort((a, b) => b.modified.localeCompare(a.modified));
  const matches: Array<{ session: SessionInfo; match: SessionSearchMatch }> = [];
  let nextIndex = 0;

  const worker = async () => {
    while (!signal?.aborted && matches.length < limit) {
      const index = nextIndex++;
      if (index >= ordered.length) return;
      const session = ordered[index];
      const match = await searchSessionFile(session, normalizedQuery, signal);
      if (match) matches.push({ session, match });
    }
  };

  await Promise.all(
    Array.from({ length: Math.min(SEARCH_CONCURRENCY, ordered.length) }, () => worker()),
  );

  return matches
    .sort((a, b) => b.session.modified.localeCompare(a.session.modified))
    .slice(0, limit)
    .map(({ match }) => match);
}
