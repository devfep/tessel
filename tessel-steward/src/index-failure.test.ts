import { beforeEach, describe, expect, it, vi } from "vitest";

import steward from "./index";

// test-runner imports "cloudflare:workers", which only the Workers runtime provides.
vi.mock("./test-runner", () => ({ ArtifactsGitGateway: vi.fn(), TestRunner: vi.fn() }));
vi.mock("./merge-gateway", () => ({ MergePushGateway: vi.fn(), MergeReadGateway: vi.fn() }));
vi.mock("./merge-service", () => ({ MergeService: vi.fn() }));

const ADMIN = "admin-token";
const LEAKY_MESSAGE = "git remote https://x:art_v1_SECRET123.abc@git.example/r.git refused";

function environment(failWith: Error): Env {
  return {
    STEWARD_ADMIN_TOKEN: ADMIN,
    ARTIFACTS: {
      get: async () => {
        throw failWith;
      },
    },
  } as unknown as Env;
}

async function readTokenRequest(env: Env): Promise<Response> {
  const request = new Request("https://steward.example/repos/tessel/read-tokens", {
    method: "POST",
    headers: { Authorization: `Bearer ${ADMIN}` },
  });
  const handler = steward.fetch;
  if (handler === undefined) {
    throw new Error("steward has no fetch handler");
  }
  return handler(request as never, env);
}

describe("a dashboard request that fails", () => {
  it("logs the error with any token redacted and answers 502", async () => {
    const logged = vi.spyOn(console, "error").mockImplementation(() => {});
    const env = {
      get ACCESS_TEAM_DOMAIN(): string {
        throw new Error(LEAKY_MESSAGE);
      },
    } as unknown as Env;
    const handler = steward.fetch;
    if (handler === undefined) {
      throw new Error("steward has no fetch handler");
    }
    const response = await handler(
      new Request("https://steward.example/dashboard/demo") as never,
      env,
    );
    expect(response.status).toBe(502);
    expect(logged).toHaveBeenCalledTimes(1);
    const line = String(logged.mock.calls[0]?.[0]);
    expect(line).toContain("dashboard_failed");
    expect(line).toContain("refused");
    expect(line).not.toContain("SECRET123");
    logged.mockRestore();
  });
});

describe("a route that fails", () => {
  beforeEach(() => {
    // Node's SubtleCrypto lacks the Workers-only timingSafeEqual that the admin check uses.
    Object.defineProperty(crypto.subtle, "timingSafeEqual", {
      value: (a: ArrayBuffer, b: ArrayBuffer) => Buffer.from(a).equals(Buffer.from(b)),
      configurable: true,
    });
  });

  it("returns and logs the error message with any token redacted", async () => {
    const logged = vi.spyOn(console, "error").mockImplementation(() => {});
    const response = await readTokenRequest(environment(new Error(LEAKY_MESSAGE)));
    const body = await response.text();
    expect(response.status).toBe(500);
    expect(body).toContain("git remote");
    expect(body).toContain("refused");
    expect(body).not.toContain("SECRET123");
    expect(logged).toHaveBeenCalledTimes(1);
    const line = String(logged.mock.calls[0]?.[0]);
    expect(line).toContain("route_failed");
    expect(line).toContain("refused");
    expect(line).not.toContain("SECRET123");
    logged.mockRestore();
  });
});
