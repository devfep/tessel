import { describe, expect, it } from "vitest";

import type { LinkEnv } from "./coordinator-link";
import { readEvents } from "./review-log";

class FakeSocket {
  sent: unknown[] = [];
  closed = false;
  private message: ((event: { data: unknown }) => void) | undefined;
  private onClose: (() => void) | undefined;

  constructor(
    private readonly frames: unknown[],
    private readonly hangUp = false,
  ) {}

  accept(): void {}

  send(data: string): void {
    this.sent.push(JSON.parse(data));
    for (const frame of this.frames) {
      queueMicrotask(() => this.message?.({ data: JSON.stringify(frame) }));
    }
    if (this.hangUp) {
      queueMicrotask(() => this.onClose?.());
    }
  }

  close(): void {
    this.closed = true;
  }

  addEventListener(type: string, listener: never): void {
    if (type === "message") {
      this.message = listener;
    } else if (type === "close") {
      this.onClose = listener;
    }
  }
}

function env(socket: FakeSocket, head: unknown, status = 200): LinkEnv {
  return {
    IDENTITY_SIGNING_KEY: "k",
    COORDINATOR: {
      fetch(_url: string, init: RequestInit) {
        if (new Headers(init.headers).get("Upgrade") === "websocket") {
          return Promise.resolve({ status: 101, webSocket: socket } as unknown as Response);
        }
        return Promise.resolve(Response.json({ head_seq: head }, { status }));
      },
    } as unknown as Fetcher,
  } as LinkEnv;
}

const frame = (seq: number) => ({ type: "event", event: { seq, event: "merged", claim: seq } });

describe("readEvents", () => {
  it("replays the log from the start up to the summary's last event, then closes", async () => {
    const socket = new FakeSocket([frame(0), frame(1), frame(2), frame(3)]);
    const events = await readEvents(env(socket, 2), "demo", "dashboard");
    expect(events).toEqual([frame(0).event, frame(1).event, frame(2).event]);
    expect(socket.sent).toEqual([{ type: "watch", from_seq: 0 }]);
    expect(socket.closed).toBe(true);
  });

  it("opens no socket for an empty log", async () => {
    const socket = new FakeSocket([]);
    expect(await readEvents(env(socket, null), "demo", "dashboard")).toEqual([]);
    expect(socket.sent).toEqual([]);
  });

  it("fails when the summary is refused", async () => {
    await expect(readEvents(env(new FakeSocket([]), 1, 503), "demo", "dashboard")).rejects.toThrow(
      "503",
    );
  });

  it("fails when the log is not complete in time", async () => {
    await expect(
      readEvents(env(new FakeSocket([frame(0)]), 5), "demo", "dashboard", 20),
    ).rejects.toThrow("timed out");
  });

  it("fails when the socket closes before the last event", async () => {
    await expect(readEvents(env(new FakeSocket([], true), 5), "demo", "dashboard")).rejects.toThrow(
      "closed",
    );
  });

  it("fails on a coordinator error frame", async () => {
    const error = { type: "error", req: null, code: "malformed", message: "no" };
    await expect(readEvents(env(new FakeSocket([error]), 5), "demo", "dashboard")).rejects.toThrow(
      "refused the watch",
    );
  });
});
