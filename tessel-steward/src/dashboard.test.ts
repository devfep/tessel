import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { clearAccessKeyCache } from "./access";
import { AUD, NOW_MS, TEAM, TestSigner, VIEWER, validClaims } from "./access-fixtures";
import { handleDashboard } from "./dashboard";
import steward from "./index";

// test-runner imports "cloudflare:workers", which only the Workers runtime provides.
vi.mock("./test-runner", () => ({ ArtifactsGitGateway: vi.fn(), TestRunner: vi.fn() }));
vi.mock("./merge-gateway", () => ({ MergePushGateway: vi.fn(), MergeReadGateway: vi.fn() }));
vi.mock("./merge-service", () => ({ MergeService: vi.fn() }));

const SIGNING_KEY = "test-signing-key-not-a-secret";
const ORIGIN = "https://steward.example";

interface CoordinatorCall {
  url: string;
  headers: Headers;
}

class FakeUpstream {
  sent: string[] = [];
  closed = false;
  accepted = false;
  private message: ((event: { data: unknown }) => void) | undefined;
  private closeListener: (() => void) | undefined;

  accept(): void {
    this.accepted = true;
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.closed = true;
  }

  addEventListener(type: string, listener: never): void {
    if (type === "message") {
      this.message = listener;
    } else if (type === "close") {
      this.closeListener = listener;
    }
  }

  push(message: unknown): void {
    this.message?.({ data: JSON.stringify(message) });
  }

  end(): void {
    this.closeListener?.();
  }
}

let signer: TestSigner;
let calls: CoordinatorCall[];
let upstream: FakeUpstream;
let summaryStatus: number;

beforeAll(async () => {
  signer = await TestSigner.create();
});

beforeEach(() => {
  clearAccessKeyCache();
  calls = [];
  upstream = new FakeUpstream();
  summaryStatus = 200;
});

function coordinator(): Fetcher {
  return {
    fetch(input: string, init: RequestInit) {
      const headers = new Headers(init.headers);
      calls.push({ url: input, headers });
      if (headers.get("Upgrade") === "websocket") {
        return Promise.resolve({ status: 101, webSocket: upstream } as unknown as Response);
      }
      return Promise.resolve(
        Response.json({ summary: { merges: 2 }, head_seq: 9 }, { status: summaryStatus }),
      );
    },
  } as unknown as Fetcher;
}

function env(overrides: Record<string, unknown> = {}): Env {
  return {
    ACCESS_TEAM_DOMAIN: TEAM,
    ACCESS_AUD: AUD,
    DASHBOARD_VIEWERS: VIEWER,
    IDENTITY_SIGNING_KEY: SIGNING_KEY,
    COORDINATOR: coordinator(),
    ...overrides,
  } as unknown as Env;
}

async function get(
  path: string,
  options: { token?: string | null; headers?: Record<string, string>; method?: string } = {},
  environment: Env = env(),
): Promise<Response> {
  const token = options.token === undefined ? await signer.sign(validClaims()) : options.token;
  const headers = new Headers(options.headers);
  if (token !== null) {
    headers.set("Cf-Access-Jwt-Assertion", token);
  }
  const request = new Request(`${ORIGIN}${path}`, { method: options.method ?? "GET", headers });
  return handleDashboard(request, environment, {
    fetchImpl: signer.certsFetch(),
    now: () => NOW_MS,
  });
}

function decodePayload(authorization: string | null): Record<string, unknown> {
  const payload = (authorization ?? "").replace("Bearer ", "").split(".")[0] ?? "";
  return JSON.parse(atob(payload.replaceAll("-", "+").replaceAll("_", "/"))) as never;
}

describe("sign-in", () => {
  it.each(["/dashboard/demo", "/dashboard/demo/events", "/dashboard/demo/summary"])(
    "answers 503 on %s until Access is configured, and never reaches the coordinator",
    async (path) => {
      for (const missing of ["ACCESS_TEAM_DOMAIN", "ACCESS_AUD"]) {
        const response = await get(path, {}, env({ [missing]: "" }));
        expect(response.status).toBe(503);
        expect(await response.text()).toBe("sign-in not configured");
      }
      expect(calls).toEqual([]);
    },
  );

  it.each(["/dashboard/demo", "/dashboard/demo/events", "/dashboard/demo/summary"])(
    "answers 401 on %s without a token",
    async (path) => {
      expect((await get(path, { token: null })).status).toBe(401);
      expect(calls).toEqual([]);
    },
  );

  it("answers 403 for a valid token whose email is not a viewer", async () => {
    const token = await signer.sign(validClaims({ email: "mallory@example.com" }));
    expect((await get("/dashboard/demo/summary", { token })).status).toBe(403);
    expect(calls).toEqual([]);
  });

  it("answers 401 for a token for another application", async () => {
    const token = await signer.sign(validClaims({ aud: ["other"] }));
    expect((await get("/dashboard/demo", { token })).status).toBe(401);
  });

  it("checks sign-in before it looks at the path", async () => {
    expect((await get("/dashboard/demo/nope", { token: null })).status).toBe(401);
    expect((await get("/dashboard/bad%20name", { token: null })).status).toBe(401);
  });

  it("is reached through the Worker's fetch handler without the admin bearer", async () => {
    const handler = steward.fetch;
    if (handler === undefined) {
      throw new Error("steward has no fetch handler");
    }
    const response = await handler(
      new Request(`${ORIGIN}/dashboard/demo`) as never,
      { STEWARD_ADMIN_TOKEN: "admin", IDENTITY_SIGNING_KEY: SIGNING_KEY } as unknown as Env,
    );
    expect(response.status).toBe(503);
  });
});

describe("the page", () => {
  it("serves one self-contained page under a nonce-bound policy", async () => {
    const response = await get("/dashboard/demo");
    const html = await response.text();
    const policy = response.headers.get("Content-Security-Policy") ?? "";
    const nonce = /script-src 'nonce-([^']+)'/.exec(policy)?.[1] ?? "";
    expect(response.status).toBe(200);
    expect(response.headers.get("Content-Type")).toContain("text/html");
    expect(nonce).not.toBe("");
    expect(html).toContain(`<script nonce="${nonce}">`);
    expect(html).not.toMatch(/<script[^>]*\ssrc=/);
    expect(html).not.toMatch(/<link|https?:\/\//);
    expect(policy).toContain("default-src 'none'");
  });

  it("uses a new nonce on every response", async () => {
    const [first, second] = await Promise.all([get("/dashboard/demo"), get("/dashboard/demo")]);
    expect(first.headers.get("Content-Security-Policy")).not.toBe(
      second.headers.get("Content-Security-Policy"),
    );
  });
});

describe("routing", () => {
  it.each([
    ["an unknown leaf", "/dashboard/demo/nope", "GET", 404],
    ["a nested path", "/dashboard/demo/events/1", "GET", 404],
    ["no repo", "/dashboard", "GET", 404],
    ["a write method", "/dashboard/demo", "POST", 405],
    ["a write method on events", "/dashboard/demo/events", "DELETE", 405],
    ["an invalid repo name", "/dashboard/bad%20name", "GET", 400],
  ])("answers %s with %i", async (_label, path, method, status) => {
    expect((await get(path, { method })).status).toBe(status);
  });
});

describe("/summary", () => {
  it("proxies the coordinator's summary with a short-lived dashboard token for that repo", async () => {
    const response = await get("/dashboard/demo/summary");
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ summary: { merges: 2 }, head_seq: 9 });
    expect(calls).toHaveLength(1);
    expect(calls[0]?.url).toBe("https://coordinator.internal/repo/demo/summary");
    const claims = decodePayload(calls[0]?.headers.get("Authorization") ?? null);
    expect(claims).toMatchObject({ v: 1, repo: "demo", agent: "dashboard" });
    expect(Number(claims["exp_ms"]) - Date.now()).toBeLessThanOrEqual(60_000);
  });

  it("answers 502 when the coordinator refuses", async () => {
    summaryStatus = 401;
    const response = await get("/dashboard/demo/summary");
    expect(response.status).toBe(502);
    expect(await response.text()).toContain("401");
  });
});

async function stream(headers: Record<string, string> = {}): Promise<string> {
  const response = await get("/dashboard/demo/events", { headers });
  expect(response.headers.get("Content-Type")).toBe("text/event-stream");
  const read = response.text();
  await vi.waitFor(() => expect(upstream.sent).toHaveLength(1));
  upstream.push({ type: "welcome", head: "h", lease_ms: 1, protocol: 1 });
  upstream.push({ type: "event", event: { seq: 12, at_ms: 1, run: "r", event: "merged" } });
  upstream.end();
  return read;
}

describe("/events", () => {
  it("opens one watch socket with the dashboard identity and relays events", async () => {
    const text = await stream();
    expect(calls).toHaveLength(1);
    expect(calls[0]?.url).toBe("https://coordinator.internal/repo/demo/ws");
    expect(calls[0]?.headers.get("Upgrade")).toBe("websocket");
    expect(decodePayload(calls[0]?.headers.get("Authorization") ?? null)).toMatchObject({
      repo: "demo",
      agent: "dashboard",
    });
    expect(upstream.accepted).toBe(true);
    expect(JSON.parse(upstream.sent[0] ?? "")).toMatchObject({ type: "hello", agent: "dashboard" });
    expect(JSON.parse(upstream.sent[1] ?? "")).toEqual({ type: "watch", from_seq: 0 });
    expect(text).toContain(`id: 12\ndata: {"seq":12,`);
    expect(upstream.closed).toBe(true);
  });

  it("resumes after Last-Event-ID", async () => {
    await stream({ "Last-Event-ID": "41" });
    expect(JSON.parse(upstream.sent[1] ?? "")).toEqual({ type: "watch", from_seq: 42 });
  });

  it.each(["abc", "-3", "1.5", ""])("replays from the start for Last-Event-ID %j", async (id) => {
    await stream({ "Last-Event-ID": id });
    expect(JSON.parse(upstream.sent[1] ?? "")).toEqual({ type: "watch", from_seq: 0 });
  });

  it("tells the browser when the coordinator refuses the watch", async () => {
    const refusing = env({
      COORDINATOR: {
        fetch: () => Promise.resolve(new Response("no", { status: 401 })),
      } as unknown as Fetcher,
    });
    const response = await get("/dashboard/demo/events", {}, refusing);
    expect(await response.text()).toContain("event: upstream-error");
  });
});
