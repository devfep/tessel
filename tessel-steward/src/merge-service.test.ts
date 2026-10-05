import { beforeEach, describe, expect, it, vi } from "vitest";

// "cloudflare:workers" only exists in the Workers runtime.
vi.mock("cloudflare:workers", () => ({
  WorkerEntrypoint: class {
    readonly env: unknown;
    constructor(_ctx: unknown, env: unknown) {
      this.env = env;
    }
  },
}));

import { MergeService } from "./merge-service";

const COMMIT = "b".repeat(40);
const SCOPES = [{ scope: { kind: "dir", path: "src" }, mode: "edit_body" }];

function build(
  sources: Record<string, string | null>,
  merge = vi.fn(),
  artifacts?: { get: () => Promise<never> },
) {
  const env = {
    ARTIFACTS: artifacts ?? {
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
  const Service = MergeService as unknown as new (ctx: unknown, env: Env) => MergeService;
  return { service: new Service({}, env), merge };
}

function post(body: unknown): Request {
  const text = typeof body === "string" ? body : JSON.stringify(body);
  return new Request("https://steward.internal/merge", { method: "POST", body: text });
}

describe("MergeService", () => {
  beforeEach(() => {
    vi.spyOn(console, "error").mockImplementation(() => {});
  });

  it("merges the fork's commit and returns the outcome as JSON", async () => {
    const merge = vi.fn(async () => ({ outcome: "already_merged", base: COMMIT }));
    const { service } = build({ "demo--a1": "artifacts:tessel/demo" }, merge);
    const response = await service.fetch(
      post({ repo: "demo", fork: "demo--a1", commit: COMMIT, scopes: SCOPES }),
    );
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ outcome: "already_merged", base: COMMIT });
    expect(merge).toHaveBeenCalledWith("demo", "demo--a1", COMMIT, SCOPES);
  });

  it("refuses a fork that is not a fork of the repo without merging", async () => {
    const { service, merge } = build({ "other--a1": "artifacts:tessel/other" });
    const response = await service.fetch(
      post({ repo: "demo", fork: "other--a1", commit: COMMIT, scopes: SCOPES }),
    );
    expect(response.status).toBe(400);
    expect(merge).not.toHaveBeenCalled();
  });

  it.each([
    ["a body that is not JSON", "not json"],
    ["a body without a repo", { fork: "demo--a1", commit: COMMIT, scopes: SCOPES }],
    [
      "a repo that is not a name",
      { repo: "../x", fork: "demo--a1", commit: COMMIT, scopes: SCOPES },
    ],
    [
      "a commit that is not a sha",
      { repo: "demo", fork: "demo--a1", commit: "main", scopes: SCOPES },
    ],
    [
      "a request from an old coordinator, without scopes",
      { repo: "demo", fork: "demo--a1", commit: COMMIT },
    ],
  ])("answers 400 for %s", async (_label, body) => {
    const { service, merge } = build({ "demo--a1": "artifacts:tessel/demo" });
    const response = await service.fetch(post(body));
    expect(response.status).toBe(400);
    expect(merge).not.toHaveBeenCalled();
  });

  it("answers 404 when the fork or the repo does not exist, so the coordinator does not retry", async () => {
    const { service, merge } = build({});
    const response = await service.fetch(
      post({ repo: "demo", fork: "demo--ghost", commit: COMMIT, scopes: SCOPES }),
    );
    expect(response.status).toBe(404);
    expect(merge).not.toHaveBeenCalled();
  });

  it("answers 502 for an Artifacts failure that is not NOT_FOUND", async () => {
    const failing = {
      get: async () => {
        throw Object.assign(new Error("down"), { code: "UPSTREAM_UNAVAILABLE" });
      },
    };
    const { service } = build({}, vi.fn(), failing);
    const response = await service.fetch(
      post({ repo: "demo", fork: "demo--a1", commit: COMMIT, scopes: SCOPES }),
    );
    expect(response.status).toBe(502);
  });

  it("answers 405 to anything but POST", async () => {
    const { service } = build({});
    const response = await service.fetch(new Request("https://steward.internal/merge"));
    expect(response.status).toBe(405);
  });

  it("answers 502 with a fixed message when the merge throws, without leaking the reason", async () => {
    const merge = vi.fn(async () => {
      throw new Error("boom art_v1_secret");
    });
    const { service } = build({ "demo--a1": "artifacts:tessel/demo" }, merge);
    const response = await service.fetch(
      post({ repo: "demo", fork: "demo--a1", commit: COMMIT, scopes: SCOPES }),
    );
    expect(response.status).toBe(502);
    expect(JSON.stringify(await response.json())).not.toContain("secret");
  });
});
