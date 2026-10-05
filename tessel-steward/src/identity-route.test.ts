import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { IDENTITY_TTL_MS } from "./identity";
import steward from "./index";

// test-runner imports "cloudflare:workers", which only the Workers runtime provides.
vi.mock("./test-runner", () => ({ ArtifactsGitGateway: vi.fn(), TestRunner: vi.fn() }));
vi.mock("./merge-gateway", () => ({ MergePushGateway: vi.fn(), MergeReadGateway: vi.fn() }));

const NOW_MS = 1_790_000_000_000;
const ADMIN = "admin-token";
const SIGNING_KEY = "test-signing-key-not-a-secret";

function env(overrides: Record<string, string | undefined> = {}): Env {
  return {
    STEWARD_ADMIN_TOKEN: ADMIN,
    IDENTITY_SIGNING_KEY: SIGNING_KEY,
    ...overrides,
  } as unknown as Env;
}

function post(path: string, auth: string | null = `Bearer ${ADMIN}`): Request {
  const headers = auth === null ? {} : { Authorization: auth };
  return new Request(`https://steward.example${path}`, { method: "POST", headers });
}

async function call(request: Request, environment: Env): Promise<Response> {
  const handler = steward.fetch;
  if (handler === undefined) {
    throw new Error("steward has no fetch handler");
  }
  return handler(request as never, environment);
}

describe("POST /repos/<repo>/agents/<agent>/identity", () => {
  beforeEach(() => {
    // Node's SubtleCrypto lacks the Workers-only timingSafeEqual that the admin check uses.
    Object.defineProperty(crypto.subtle, "timingSafeEqual", {
      value: (a: ArrayBuffer, b: ArrayBuffer) => Buffer.from(a).equals(Buffer.from(b)),
      configurable: true,
    });
    vi.useFakeTimers();
    vi.setSystemTime(NOW_MS);
    vi.spyOn(console, "error").mockImplementation(() => {});
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it.each([
    ["no Authorization header", null],
    ["a wrong admin token", "Bearer wrong"],
  ])("refuses %s with 401", async (_label, auth) => {
    const response = await call(post("/repos/demo/agents/a1/identity", auth), env());
    expect(response.status).toBe(401);
  });

  it.each([
    ["an agent with a space", "/repos/demo/agents/a%20b/identity"],
    ["an agent with a leading dash", "/repos/demo/agents/-a/identity"],
    ["an overlong agent", `/repos/demo/agents/${"a".repeat(129)}/identity`],
    ["a repo with a leading dot", "/repos/.demo/agents/a1/identity"],
    ["a repo with a percent escape", "/repos/de%6Do/agents/a1/identity"],
  ])("refuses %s with 400", async (_label, path) => {
    const response = await call(post(path), env());
    expect(response.status).toBe(400);
  });

  it("returns the token, names and an expiry 24 hours from now", async () => {
    const response = await call(post("/repos/demo/agents/a1/identity"), env());
    expect(response.status).toBe(201);
    const body = (await response.json()) as Record<string, unknown>;
    expect(Object.keys(body).toSorted()).toEqual(["agent", "expires_at_ms", "repo", "token"]);
    expect(body["agent"]).toBe("a1");
    expect(body["repo"]).toBe("demo");
    expect(body["expires_at_ms"]).toBe(NOW_MS + IDENTITY_TTL_MS);
    expect(body["token"]).toMatch(/^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]{43}$/);
  });

  it.each([
    ["empty", ""],
    ["missing", undefined],
  ])("fails closed with 500 and no token when the signing key is %s", async (_label, key) => {
    const response = await call(
      post("/repos/demo/agents/a1/identity"),
      env({ IDENTITY_SIGNING_KEY: key }),
    );
    expect(response.status).toBe(500);
    expect(await response.text()).not.toContain('token":');
  });
});
