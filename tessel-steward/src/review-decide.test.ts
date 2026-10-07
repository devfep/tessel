import { describe, expect, it } from "vitest";

import type { LinkEnv } from "./coordinator-link";
import { sendReview } from "./review-decide";

const KEY = "test-signing-key-not-a-secret";
const REQUEST = { claim: 7, approve: true, note: "felix via review UI: ok" };

type Reaction = (sent: Record<string, unknown>, push: (frame: unknown) => void) => void;

class FakeSocket {
  sent: Array<Record<string, unknown>> = [];
  closed = false;
  private message: ((event: { data: unknown }) => void) | undefined;
  private onClose: (() => void) | undefined;

  constructor(private readonly react: Reaction) {}

  accept(): void {}

  send(data: string): void {
    const sent = JSON.parse(data) as Record<string, unknown>;
    this.sent.push(sent);
    this.react(sent, (frame) => this.push(frame));
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

  push(frame: unknown): void {
    queueMicrotask(() => this.message?.({ data: JSON.stringify(frame) }));
  }

  hangUp(): void {
    this.onClose?.();
  }
}

function link(socket: FakeSocket, head: unknown = 9, summaryStatus = 200) {
  const urls: string[] = [];
  const coordinator = {
    fetch(url: string, init: RequestInit) {
      urls.push(url);
      if (new Headers(init.headers).get("Upgrade") === "websocket") {
        return Promise.resolve({ status: 101, webSocket: socket } as unknown as Response);
      }
      return Promise.resolve(
        Response.json({ summary: {}, head_seq: head }, { status: summaryStatus }),
      );
    },
  } as unknown as Fetcher;
  return { env: { IDENTITY_SIGNING_KEY: KEY, COORDINATOR: coordinator } as LinkEnv, urls };
}

const welcome = { type: "welcome", head: "h", lease_ms: 1, protocol: 1 };

function decidedEvent(overrides: Record<string, unknown> = {}) {
  return {
    type: "event",
    event: {
      seq: 10,
      event: "review_decided",
      claim: 7,
      approve: true,
      note: "n",
      reviewer: "felix",
      ...overrides,
    },
  };
}

function scripted(then: (push: (frame: unknown) => void) => void): Reaction {
  return (sent, push) => {
    if (sent["type"] === "hello") {
      push(welcome);
    } else if (sent["type"] === "review") {
      then(push);
    }
  };
}

describe("sendReview", () => {
  it("sends hello, watch and one review, then reports the logged decision", async () => {
    const socket = new FakeSocket(scripted((push) => push(decidedEvent())));
    const { env } = link(socket);
    const outcome = await sendReview(env, "demo", "felix", REQUEST);
    expect(outcome).toEqual({ outcome: "decided", approve: true, seq: 10 });
    expect(socket.sent).toEqual([
      { type: "hello", agent: "felix", base: "0".repeat(40), protocol: 1 },
      { type: "watch", from_seq: 10 },
      { type: "review", req: 1, claim: 7, approve: true, note: "felix via review UI: ok" },
    ]);
    expect(socket.closed).toBe(true);
  });

  it("watches from the start when the log is empty", async () => {
    const socket = new FakeSocket(scripted((push) => push(decidedEvent())));
    await sendReview(link(socket, null).env, "demo", "felix", REQUEST);
    expect(socket.sent[1]).toEqual({ type: "watch", from_seq: 0 });
  });

  it("reports the approve value the log holds, not the one that was sent", async () => {
    const socket = new FakeSocket(scripted((push) => push(decidedEvent({ approve: false }))));
    expect(await sendReview(link(socket).env, "demo", "felix", REQUEST)).toMatchObject({
      outcome: "decided",
      approve: false,
    });
  });

  it("passes on the coordinator's error and does not call it decided", async () => {
    const socket = new FakeSocket(
      scripted((push) =>
        push({
          type: "error",
          req: 1,
          code: "not_awaiting_review",
          message: "claim 7 is not awaiting review",
        }),
      ),
    );
    expect(await sendReview(link(socket).env, "demo", "felix", REQUEST)).toEqual({
      outcome: "refused",
      code: "not_awaiting_review",
      message: "claim 7 is not awaiting review",
    });
  });

  it.each([
    ["another claim", { claim: 8 }],
    ["another reviewer", { reviewer: "orchestrator" }],
    ["an older position in the log", { seq: 9 }],
  ])("does not take a decision of %s for this one", async (_name, change) => {
    const socket = new FakeSocket(scripted((push) => push(decidedEvent(change))));
    const outcome = await sendReview(link(socket).env, "demo", "felix", REQUEST, 30);
    expect(outcome).toEqual({
      outcome: "unknown",
      reason: "the coordinator did not answer in time",
    });
  });

  it("answers unknown when the coordinator never answers within the wait", async () => {
    const socket = new FakeSocket(scripted(() => {}));
    const outcome = await sendReview(link(socket).env, "demo", "felix", REQUEST, 20);
    expect(outcome.outcome).toBe("unknown");
    expect(socket.closed).toBe(true);
  });

  it("answers unknown when the socket closes before an answer", async () => {
    const socket = new FakeSocket(scripted(() => queueMicrotask(() => socket.hangUp())));
    expect(await sendReview(link(socket).env, "demo", "felix", REQUEST)).toEqual({
      outcome: "unknown",
      reason: "the coordinator closed the connection",
    });
  });

  it("sends nothing when the summary cannot be read", async () => {
    const socket = new FakeSocket(scripted(() => {}));
    await expect(sendReview(link(socket, 9, 500).env, "demo", "felix", REQUEST)).rejects.toThrow(
      "500",
    );
    expect(socket.sent).toEqual([]);
  });
});
