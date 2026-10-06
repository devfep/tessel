/** The part of a WebSocket the relay uses; the Workers WebSocket and test fakes both fit. */
export interface UpstreamSocket {
  send(data: string): void;
  close(): void;
  addEventListener(type: "message", listener: (event: { data: unknown }) => void): void;
  addEventListener(type: "close" | "error", listener: () => void): void;
}

/** The coordinator's `Hello` for a read-only watcher. The base commit is never consulted. */
export const DASHBOARD_HELLO = JSON.stringify({
  type: "hello",
  agent: "dashboard",
  base: "0000000000000000000000000000000000000000",
});

const RECONNECT_DELAY_MS = 2000;

function frame(fields: { id?: number; event?: string; data: string }): string {
  const idLine = fields.id === undefined ? "" : `id: ${fields.id}\n`;
  const eventLine = fields.event === undefined ? "" : `event: ${fields.event}\n`;
  return `${idLine}${eventLine}data: ${fields.data}\n\n`;
}

function parseFrame(data: unknown): Record<string, unknown> | undefined {
  if (typeof data !== "string") {
    return undefined;
  }
  try {
    const value: unknown = JSON.parse(data);
    if (typeof value === "object" && value !== null && !Array.isArray(value)) {
      return value as Record<string, unknown>;
    }
  } catch {
    // Not JSON: dropped, like any frame the dashboard does not understand.
  }
  return undefined;
}

/**
 * Opens one coordinator `Watch` and turns its events into a Server-Sent Events body. Each event
 * is sent with `id` set to its `seq`, so a browser that reconnects with `Last-Event-ID` resumes
 * at `fromSeq = id + 1`. The upstream socket lives exactly as long as the returned stream: it is
 * closed when the reader cancels, and the stream ends when the upstream closes.
 *
 * @param connect Opens and accepts the coordinator socket.
 * @param fromSeq First `seq` to replay.
 */
export function relayWatch(
  connect: () => Promise<UpstreamSocket>,
  fromSeq: number,
): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  let upstream: UpstreamSocket | undefined;
  let finished = false;

  function finish(controller: ReadableStreamDefaultController<Uint8Array>): void {
    if (finished) {
      return;
    }
    finished = true;
    upstream?.close();
    controller.close();
  }

  return new ReadableStream<Uint8Array>({
    async start(controller) {
      const send = (text: string): void => controller.enqueue(encoder.encode(text));
      send(`retry: ${RECONNECT_DELAY_MS}\n\n`);
      let nextSeq = fromSeq;
      try {
        upstream = await connect();
      } catch (error) {
        send(frame({ event: "upstream-error", data: JSON.stringify(String(error)) }));
        finish(controller);
        return;
      }
      if (finished) {
        upstream.close();
        return;
      }
      upstream.addEventListener("message", ({ data }) => {
        const message = parseFrame(data);
        const type = message?.["type"];
        if (finished || message === undefined) {
          return;
        }
        if (type === "welcome") {
          upstream?.send(JSON.stringify({ type: "watch", from_seq: fromSeq }));
        } else if (type === "event") {
          const event = message["event"] as { seq?: unknown } | null;
          const seq = event?.seq;
          if (typeof seq === "number" && seq >= nextSeq) {
            nextSeq = seq + 1;
            send(frame({ id: seq, data: JSON.stringify(event) }));
          }
        } else if (type === "error") {
          send(frame({ event: "upstream-error", data: JSON.stringify(message["message"]) }));
          finish(controller);
        }
      });
      upstream.addEventListener("close", () => finish(controller));
      upstream.addEventListener("error", () => finish(controller));
      upstream.send(DASHBOARD_HELLO);
    },
    cancel() {
      finished = true;
      upstream?.close();
    },
  });
}
