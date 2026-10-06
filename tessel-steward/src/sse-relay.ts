/** The part of a WebSocket the relay uses; the Workers WebSocket and test fakes both fit. */
export interface UpstreamSocket {
  send(data: string): void;
  close(): void;
  addEventListener(type: "message", listener: (event: { data: unknown }) => void): void;
  addEventListener(type: "close" | "error", listener: () => void): void;
}

const RECONNECT_DELAY_MS = 2000;
/** Under the edge's idle limit (about 100 s), so a quiet log does not drop the stream. */
export const KEEPALIVE_MS = 30_000;
/** Frames queued for a browser that is not reading before the stream is closed. */
export const MAX_QUEUED_FRAMES = 1024;

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
 * A `Watch` needs no `Hello`, so none is sent: the coordinator logs an `AgentConnected` event for
 * every hello, and page loads must not write to the evidence log. A comment frame is sent every
 * `KEEPALIVE_MS`. If the browser stops reading and `maxQueued` frames pile up, the stream is
 * closed instead of buffering without bound; the browser resumes from its last id.
 *
 * @param connect Opens and accepts the coordinator socket.
 * @param fromSeq First `seq` to replay.
 * @param maxQueued Frames that may wait unread.
 */
export function relayWatch(
  connect: () => Promise<UpstreamSocket>,
  fromSeq: number,
  maxQueued = MAX_QUEUED_FRAMES,
): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  let upstream: UpstreamSocket | undefined;
  let finished = false;
  let keepalive: ReturnType<typeof setInterval> | undefined;

  function stopKeepalive(): void {
    if (keepalive !== undefined) {
      clearInterval(keepalive);
    }
  }

  function finish(controller: ReadableStreamDefaultController<Uint8Array>): void {
    if (finished) {
      return;
    }
    finished = true;
    stopKeepalive();
    upstream?.close();
    controller.close();
  }

  return new ReadableStream<Uint8Array>(
    {
      async start(controller) {
        const send = (text: string): void => {
          if ((controller.desiredSize ?? 1) <= 0) {
            finish(controller);
            return;
          }
          controller.enqueue(encoder.encode(text));
        };
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
          if (type === "event") {
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
        upstream.send(JSON.stringify({ type: "watch", from_seq: fromSeq }));
        keepalive = setInterval(() => send(": ping\n\n"), KEEPALIVE_MS);
      },
      cancel() {
        finished = true;
        stopKeepalive();
        upstream?.close();
      },
    },
    { highWaterMark: maxQueued },
  );
}
