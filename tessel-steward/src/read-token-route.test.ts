import { beforeEach, describe, expect, it, vi } from "vitest";

import steward from "./index";

// test-runner imports "cloudflare:workers", which only the Workers runtime provides.
vi.mock("./test-runner", () => ({ ArtifactsGitGateway: vi.fn(), TestRunner: vi.fn() }));
vi.mock("./merge-gateway", () => ({ MergePushGateway: vi.fn(), MergeReadGateway: vi.fn() }));
vi.mock("./merge-service", () => ({ MergeService: vi.fn() }));

const ADMIN = "admin-token";

function environment(created: Array<[string, number | undefined]>): Env {
  const handle = {
    info: async () => ({ remote: "https://git.example/git/tessel/tessel.git", source: null }),
    createToken: async (scope: string, ttl?: number) => {
      created.push([scope, ttl]);
      return { id: "t1", plaintext: "art_v1_secret", scope, expiresAt: "2026-10-05T12:00:00Z" };
    },
    [Symbol.dispose]: () => undefined,
  };
  return {
    STEWARD_ADMIN_TOKEN: ADMIN,
    ARTIFACTS: { get: async () => handle },
  } as unknown as Env;
}

async function call(auth: string | null, env: Env): Promise<Response> {
  const headers = auth === null ? {} : { Authorization: auth };
  const request = new Request("https://steward.example/repos/tessel/read-tokens", {
    method: "POST",
    headers,
  });
  const handler = steward.fetch;
  if (handler === undefined) {
    throw new Error("steward has no fetch handler");
  }
  return handler(request as never, env);
}

describe("POST /repos/<repo>/read-tokens", () => {
  beforeEach(() => {
    // Node's SubtleCrypto lacks the Workers-only timingSafeEqual that the admin check uses.
    Object.defineProperty(crypto.subtle, "timingSafeEqual", {
      value: (a: ArrayBuffer, b: ArrayBuffer) => Buffer.from(a).equals(Buffer.from(b)),
      configurable: true,
    });
  });

  it("mints a short read token for the repo, main repo included", async () => {
    const created: Array<[string, number | undefined]> = [];
    const response = await call(`Bearer ${ADMIN}`, environment(created));
    expect(response.status).toBe(201);
    expect(await response.json()).toEqual({
      repo: "tessel",
      remote: "https://git.example/git/tessel/tessel.git",
      token: "art_v1_secret",
      scope: "read",
      expiresAt: "2026-10-05T12:00:00Z",
    });
    expect(created).toEqual([["read", 600]]);
  });

  it("refuses a request without the admin token and mints nothing", async () => {
    for (const auth of [null, "Bearer wrong"]) {
      const created: Array<[string, number | undefined]> = [];
      const response = await call(auth, environment(created));
      expect(response.status).toBe(401);
      expect(created).toEqual([]);
    }
  });
});
