import type { FetchFunction } from "@earendil-works/pi-ai";

const TERMINAL_EVENTS = new Set(["response.completed", "response.incomplete", "response.failed"]);

function isTerminalFrame(frame: string): boolean {
  const data = frame.split(/\r\n|\r|\n/)
    .filter((line) => line.startsWith("data:"))
    .map((line) => line.slice(5).trimStart()).join("\n");
  try {
    const event = JSON.parse(data);
    return TERMINAL_EVENTS.has(event?.type);
  } catch {
    return false;
  }
}

/** Some Responses gateways send a terminal event but leave the SSE body open. */
export function responsesTerminalFetch(fetchImpl: FetchFunction = globalThis.fetch): FetchFunction {
  return async (input, init) => {
    const response = await fetchImpl(input, init);
    if (!response.ok || !response.body || !response.headers.get("content-type")?.includes("text/event-stream")) {
      return response;
    }
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    const encoder = new TextEncoder();
    let pending = "";
    const body = new ReadableStream<Uint8Array>({
      async pull(controller) {
        try {
          while (true) {
            const { value, done } = await reader.read();
            pending += decoder.decode(value, { stream: !done });
            let emitted = false;
            let boundary: RegExpExecArray | null;
            while ((boundary = /\r\n\r\n|\n\n|\r\r/.exec(pending))) {
              const end = boundary.index + boundary[0].length;
              const frame = pending.slice(0, boundary.index);
              controller.enqueue(encoder.encode(pending.slice(0, end)));
              emitted = true;
              pending = pending.slice(end);
              if (isTerminalFrame(frame)) {
                controller.close();
                // Do not abort the request signal: the SDK would mark success aborted.
                void reader.cancel().catch(() => {});
                return;
              }
            }
            if (done) {
              if (pending) controller.enqueue(encoder.encode(pending));
              controller.close();
              return;
            }
            if (emitted) return;
          }
        } catch (error) {
          controller.error(error);
          void reader.cancel().catch(() => {});
        }
      },
      cancel(reason) { return reader.cancel(reason); },
    });
    return new Response(body, {
      status: response.status,
      statusText: response.statusText,
      headers: response.headers,
    });
  };
}
