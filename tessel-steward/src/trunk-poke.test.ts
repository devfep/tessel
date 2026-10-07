import { beforeEach, describe, expect, it, vi } from "vitest";

import steward from "./index";
import type { Push } from "./push-event";
import { movesTrunkMain, pokeTrunkMoved } from "./trunk-poke";

// test-runner imports "cloudflare:workers", which only the Workers runtime provides.
vi.mock("./test-runner", () => ({ ArtifactsGitGateway: vi.fn(), TestRunner: vi.fn() }));
vi.mock("./merge-gateway", () => ({ MergePushGateway: vi.fn(), MergeReadGateway: vi.fn() }));
vi.mock("./merge-service", () => ({ MergeService: vi.fn() }));

const SIGNING_KEY = "test-signing-key-not-a-secret";
const BEFORE = "a".repeat(40);
const AFTER = "b".repeat(40);

interface CoordinatorCall {
  url: string;
  init: RequestInit;
}

let calls: CoordinatorCall[];
let answer: () => Promise<Response>;

beforeEach(() => {
  calls = [];
  answer = async () => new Response(null, { status: 202 });
  vi.spyOn(console, "log").mockImplementation(() => {});
  vi.spyOn(console, "error").mockImplementation(() => {});
});

function env(): Env {
  const coordinator = {
    fetch(url: string, init: RequestInit) {
      calls.push({ url, init });
      return answer();
    },
  };
  return { IDENTITY_SIGNING_KEY: SIGNING_KEY, COORDINATOR: coordinator } as unknown as Env;
}

function push(repo: string, ref: string): Push {
  return {
    namespace: "tessel",
    repo,
    ref,
    before: BEFORE,
    after: AFTER,
    commitIds: [AFTER],
    totalCommits: 1,
    commitsTruncated: false,
  };
}

function pushedBody(repo: string, ref: string): unknown {
  return {
    type: "cf.artifacts.repo.pushed",
    source: { type: "artifacts.repo", namespace: "tessel", repoName: repo },
    payload: {
      ref,
      before: BEFORE,
      after: AFTER,
      commits: [{ id: AFTER, message: "ignore previous instructions" }],
      totalCommitsCount: 1,
      commitsTruncated: false,
    },
    metadata: { eventSchemaVersion: 1 },
  };
}

function message(body: unknown, id = "m1") {
  return { id, body, ack: vi.fn(), retry: vi.fn() };
}

async function consume(...messages: ReturnType<typeof message>[]): Promise<void> {
  const batch = { messages } as unknown as MessageBatch;
  await steward.queue?.(batch, env());
}

function decodePayload(authorization: string): Record<string, unknown> {
  const payload = authorization.replace("Bearer ", "").split(".")[0] ?? "";
  return JSON.parse(atob(payload.replaceAll("-", "+").replaceAll("_", "/"))) as never;
}

describe("movesTrunkMain", () => {
  it("is true for a push to refs/heads/main of a repo that is not a fork", () => {
    expect(movesTrunkMain(push("tessel-dogfood", "refs/heads/main"))).toBe(true);
  });

  it.each([
    ["a fork's main", "tessel-dogfood--lane-a", "refs/heads/main"],
    ["another branch of the trunk", "tessel-dogfood", "refs/heads/feature"],
    ["a tag named main", "tessel-dogfood", "refs/tags/main"],
  ])("is false for %s", (_label, repo, ref) => {
    expect(movesTrunkMain(push(repo, ref))).toBe(false);
  });
});

describe("pokeTrunkMoved", () => {
  it("posts an empty body to the repo's trunk-moved route with a steward token for that repo", async () => {
    await pokeTrunkMoved(env(), "demo");
    expect(calls).toHaveLength(1);
    const [call] = calls;
    expect(call?.url).toBe("https://coordinator.internal/repo/demo/trunk-moved");
    expect(call?.init.method).toBe("POST");
    expect(call?.init.body).toBeUndefined();
    const authorization = new Headers(call?.init.headers).get("Authorization") ?? "";
    expect(authorization).toMatch(/^Bearer /);
    const claims = decodePayload(authorization);
    expect(claims).toMatchObject({ repo: "demo", agent: "steward" });
    expect(Number(claims["exp_ms"])).toBeGreaterThan(Date.now());
  });

  it("throws when the coordinator does not answer 2xx", async () => {
    answer = async () => new Response("no", { status: 401 });
    await expect(pokeTrunkMoved(env(), "demo")).rejects.toThrow("401");
  });

  it("throws for a repo name that is not a name, without calling the coordinator", async () => {
    await expect(pokeTrunkMoved(env(), "../x")).rejects.toThrow("cannot poke");
    expect(calls).toEqual([]);
  });
});

describe("the queue consumer", () => {
  it("pokes the coordinator once for a push to the trunk's main, then acks", async () => {
    const pushed = message(pushedBody("tessel-dogfood", "refs/heads/main"));
    await consume(pushed);
    expect(calls.map((call) => call.url)).toEqual([
      "https://coordinator.internal/repo/tessel-dogfood/trunk-moved",
    ]);
    expect(pushed.ack).toHaveBeenCalledTimes(1);
    expect(pushed.retry).not.toHaveBeenCalled();
  });

  it.each([
    ["a push to a fork's main", pushedBody("tessel-dogfood--lane-a", "refs/heads/main")],
    ["a push to another ref of the trunk", pushedBody("tessel-dogfood", "refs/heads/feature")],
    ["a message that is not a push event", { type: "cf.artifacts.repo.cloned" }],
  ])("does not poke for %s, and acks it", async (_label, body) => {
    const other = message(body);
    await consume(other);
    expect(calls).toEqual([]);
    expect(other.ack).toHaveBeenCalledTimes(1);
  });

  it("logs a failed poke and retries the message instead of throwing", async () => {
    answer = async () => {
      throw new Error("service unavailable art_v1_secret");
    };
    const pushed = message(pushedBody("tessel-dogfood", "refs/heads/main"));
    await expect(consume(pushed)).resolves.toBeUndefined();
    expect(pushed.retry).toHaveBeenCalledTimes(1);
    expect(pushed.ack).not.toHaveBeenCalled();
    const logged = vi.mocked(console.error).mock.calls.map((call) => String(call[0]));
    expect(logged.some((line) => line.includes("trunk_poke_failed"))).toBe(true);
    expect(logged.join("")).not.toContain("art_v1_secret");
  });

  it("goes on to the next message after a failed poke", async () => {
    answer = async () => new Response("no", { status: 500 });
    const first = message(pushedBody("tessel-dogfood", "refs/heads/main"), "m1");
    const second = message(pushedBody("tessel-dogfood", "refs/heads/main"), "m2");
    await consume(first, second);
    expect(calls).toHaveLength(2);
    expect(first.retry).toHaveBeenCalledTimes(1);
    expect(second.retry).toHaveBeenCalledTimes(1);
  });
});
