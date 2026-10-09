import { afterEach, describe, expect, it, vi } from "vitest";

import { KEEPALIVE_MS, relayWatch, type UpstreamSocket } from "./sse-relay";

type Listener = (event: { data: unknown }) => void;

class FakeSocket implements UpstreamSocket {
  sent: string[] = [];
  closed = false;
  private listeners = new Map<string, Listener[]>();

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.closed = true;
  }

  addEventListener(type: string, listener: Listener): void {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }

  emit(type: string, data?: unknown): void {
    for (const listener of this.listeners.get(type) ?? []) {
      listener({ data });
    }
  }

  frame(message: unknown): void {
    this.emit("message", JSON.stringify(message));
  }
}

function event(seq: number, extra: Record<string, unknown> = {}): unknown {
  return { type: "event", event: { seq, at_ms: 1, run: "r", event: "agent_connected", ...extra } };
}

async function readAll(stream: ReadableStream<Uint8Array>): Promise<string> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let text = "";
  for (;;) {
    const { done, value } = await reader.read();
    if (done) {
      return text;
    }
    text += decoder.decode(value);
  }
}

function open(
  fromSeq = 0,
  maxQueued?: number,
): { socket: FakeSocket; stream: ReadableStream<Uint8Array> } {
  const socket = new FakeSocket();
  return { socket, stream: relayWatch(() => Promise.resolve(socket), fromSeq, maxQueued) };
}

async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe("relayWatch", () => {
  it("sends exactly one frame, a watch from the requested seq, and no hello", async () => {
    const { socket, stream } = open(7);
    const read = readAll(stream);
    await settle();
    expect(socket.sent.map((frame) => JSON.parse(frame))).toEqual([{ type: "watch", from_seq: 7 }]);
    socket.emit("close");
    await read;
  });

  it("relays events as SSE frames whose id is the seq, then ends when upstream closes", async () => {
    const { socket, stream } = open();
    const read = readAll(stream);
    await settle();
    socket.frame(event(0));
    socket.frame(event(1, { agent: "a1" }));
    socket.emit("close");
    const text = await read;
    expect(text).toContain("retry: 2000");
    expect(text).toContain(`id: 0\ndata: {"seq":0,`);
    expect(text).toContain(`id: 1\ndata: {"seq":1,`);
    expect(socket.closed).toBe(true);
  });

  it("drops events at or below the seq it already relayed, and below the resume point", async () => {
    const { socket, stream } = open(5);
    const read = readAll(stream);
    await settle();
    socket.frame(event(3));
    socket.frame(event(5));
    socket.frame(event(5));
    socket.frame(event(6));
    socket.emit("close");
    const ids = [...(await read).matchAll(/^id: (\d+)$/gm)].map((m) => m[1]);
    expect(ids).toEqual(["5", "6"]);
  });

  it("ignores frames that are not events and frames that are not JSON", async () => {
    const { socket, stream } = open();
    const read = readAll(stream);
    await settle();
    socket.emit("message", "not json");
    socket.emit("message", new ArrayBuffer(1));
    socket.frame({ type: "granted" });
    socket.frame({ type: "event", event: { seq: "x" } });
    socket.emit("close");
    expect(await read).not.toContain("id:");
  });

  it("reports a coordinator error as an upstream-error frame and ends", async () => {
    const { socket, stream } = open();
    const read = readAll(stream);
    await settle();
    socket.frame({ type: "error", code: "unsupported_protocol", message: "too old" });
    const text = await read;
    expect(text).toContain(`event: upstream-error\ndata: "too old"`);
    expect(socket.closed).toBe(true);
  });

  it("reports a failed connection and ends", async () => {
    const stream = relayWatch(() => Promise.reject(new Error("refused with 401")), 0);
    expect(await readAll(stream)).toContain("event: upstream-error");
  });

  it("closes the upstream socket when the browser goes away", async () => {
    const { socket, stream } = open();
    const reader = stream.getReader();
    await reader.read();
    await settle();
    await reader.cancel();
    expect(socket.closed).toBe(true);
  });

  it("closes a socket that connects after the browser already went away", async () => {
    const socket = new FakeSocket();
    const { promise: gate, resolve: release } = Promise.withResolvers<void>();
    const stream = relayWatch(async () => {
      await gate;
      return socket;
    }, 0);
    const reader = stream.getReader();
    await reader.cancel();
    release();
    await settle();
    expect(socket.closed).toBe(true);
    expect(socket.sent).toEqual([]);
  });

  it("closes the stream instead of buffering when the browser does not read", async () => {
    const { socket, stream } = open(0, 3);
    await settle();
    for (let seq = 0; seq < 20; seq++) {
      socket.frame(event(seq));
    }
    expect(socket.closed).toBe(true);
    const ids = [...(await readAll(stream)).matchAll(/^id: (\d+)$/gm)];
    expect(ids.length).toBeLessThan(3);
  });
});

describe("relayWatch keepalive", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("sends a comment frame every 30 seconds and stops when the stream ends", async () => {
    vi.useFakeTimers();
    const { socket, stream } = open();
    const reader = stream.getReader();
    await vi.advanceTimersByTimeAsync(0);
    const decoder = new TextDecoder();
    const read = async (): Promise<string> => decoder.decode((await reader.read()).value);
    expect(await read()).toContain("retry:");
    await vi.advanceTimersByTimeAsync(KEEPALIVE_MS);
    expect(await read()).toBe(": ping\n\n");
    await vi.advanceTimersByTimeAsync(KEEPALIVE_MS);
    expect(await read()).toBe(": ping\n\n");
    socket.emit("close");
    expect((await reader.read()).done).toBe(true);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("stops the timer when the browser cancels", async () => {
    vi.useFakeTimers();
    const { socket, stream } = open();
    const reader = stream.getReader();
    await vi.advanceTimersByTimeAsync(0);
    expect(vi.getTimerCount()).toBe(1);
    await reader.cancel();
    expect(vi.getTimerCount()).toBe(0);
    expect(socket.closed).toBe(true);
  });
});
