import { beforeEach, describe, expect, it, vi } from "vitest";

import steward from "./index";

// test-runner and merge-gateway import "cloudflare:workers", which only the Workers runtime has.
vi.mock("./test-runner", () => ({ ArtifactsGitGateway: vi.fn(), TestRunner: vi.fn() }));
vi.mock("./merge-gateway", () => ({ MergePushGateway: vi.fn(), MergeReadGateway: vi.fn() }));
vi.mock("./merge-service", () => ({ MergeService: vi.fn() }));

const ADMIN = "admin-token";
const COMMIT = "a".repeat(40);

function build(sources: Record<string, string | null>) {
  const merge = vi.fn(async () => ({ outcome: "commit_not_in_fork" }));
  const env = {
    STEWARD_ADMIN_TOKEN: ADMIN,
    ARTIFACTS: {
      get: async (name: string) => {
        if (!(name in sources)) {
          throw Object.assign(new Error("not found"), { code: "NOT_FOUND" });
        }
        return {
          info: async () => ({ source: sources[name] }),
          [Symbol.dispose]: () => undefined,
        };
      },
    },
    TEST_RUNNER: { getByName: () => ({ merge }) },
  } as unknown as Env;
  return { env, merge };
}

async function post(env: Env, body: unknown, auth: string | null = `Bearer ${ADMIN}`) {
  const request = new Request("https://steward.example/repos/demo/merges", {
    method: "POST",
    headers: auth === null ? {} : { Authorization: auth },
    body: typeof body === "string" ? body : JSON.stringify(body),
  });
  const handler = steward.fetch;
  if (handler === undefined) {
    throw new Error("steward has no fetch handler");
  }
  return handler(request as never, env);
}

describe("POST /repos/<repo>/merges", () => {
  beforeEach(() => {
    Object.defineProperty(crypto.subtle, "timingSafeEqual", {
      value: (a: ArrayBuffer, b: ArrayBuffer) => Buffer.from(a).equals(Buffer.from(b)),
      configurable: true,
    });
    vi.spyOn(console, "error").mockImplementation(() => {});
  });

  it("runs the merge for a fork of the repo and returns the outcome", async () => {
    const { env, merge } = build({ "demo--a1": "artifacts:tessel/demo" });
    const response = await post(env, { fork: "demo--a1", commit: COMMIT });
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ outcome: "commit_not_in_fork" });
    expect(merge).toHaveBeenCalledWith("demo", "demo--a1", COMMIT);
  });

  it("answers 401 without the admin token, and runs nothing", async () => {
    const { env, merge } = build({ "demo--a1": "artifacts:tessel/demo" });
    const response = await post(env, { fork: "demo--a1", commit: COMMIT }, null);
    expect(response.status).toBe(401);
    expect(merge).not.toHaveBeenCalled();
  });

  it.each([
    ["invalid JSON", "{nope"],
    ["a short commit", { fork: "demo--a1", commit: "abc" }],
    ["a fork name with a slash", { fork: "demo/x", commit: COMMIT }],
  ])("answers 400 for %s", async (_label, body) => {
    const { env, merge } = build({ "demo--a1": "artifacts:tessel/demo" });
    expect((await post(env, body)).status).toBe(400);
    expect(merge).not.toHaveBeenCalled();
  });

  it.each([
    ["main itself", "demo", null],
    ["an imported repo", "imported", "github:owner/demo"],
    ["a fork of another repo", "other--a1", "artifacts:tessel/other"],
  ])("answers 400 when the fork is %s", async (_label, fork, source) => {
    const { env, merge } = build({ [fork]: source });
    expect((await post(env, { fork, commit: COMMIT })).status).toBe(400);
    expect(merge).not.toHaveBeenCalled();
  });

  it("answers 404 when the fork does not exist", async () => {
    const { env, merge } = build({});
    expect((await post(env, { fork: "demo--ghost", commit: COMMIT })).status).toBe(404);
    expect(merge).not.toHaveBeenCalled();
  });
});
