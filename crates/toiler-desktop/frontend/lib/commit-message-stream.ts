import type { CommitMessageUpdate } from "./commit-message";

export type CommitMessageEvent =
  | { type: "update"; message: string }
  | { type: "done"; message: string }
  | { type: "error"; error: string };

/**
 * Streams generation progress as newline-delimited JSON so the panel can type
 * the message out while the model is still writing. Failures that happen after
 * the response headers are sent travel as an `error` event rather than an HTTP
 * status, so the client always sees a complete stream.
 */
export function streamCommitMessage(generate: (onUpdate: CommitMessageUpdate) => Promise<string>): Response {
  const encoder = new TextEncoder();
  const body = new ReadableStream<Uint8Array>({
    async start(controller) {
      const send = (event: CommitMessageEvent) => controller.enqueue(encoder.encode(`${JSON.stringify(event)}\n`));
      try {
        const message = await generate((update) => send({ type: "update", message: update }));
        send({ type: "done", message });
      } catch (error) {
        send({ type: "error", error: error instanceof Error ? error.message : String(error) });
      } finally {
        controller.close();
      }
    },
  });
  return new Response(body, {
    headers: {
      "Content-Type": "application/x-ndjson; charset=utf-8",
      "Cache-Control": "no-store, no-transform",
      "X-Accel-Buffering": "no",
    },
  });
}
