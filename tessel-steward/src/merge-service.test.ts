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
const FORK_HEAD = "e".repeat(40);
const SCOPES = [{ scope: { kind: "dir", path: "src" }, mode: "edit_body" }];

function build(
  sources: Record<string, string | null>,
  merge = vi.fn(),
  artifacts?: { get: () => Promise<never> },
) {
  const trial = vi.fn();
  const log = vi.fn(async (_options: { ref: string; limit: number }) => [{ hash: FORK_HEAD }]);
  const getByName = vi.fn((_name: string) => ({ merge, trial }));
  const env = {
    ARTIFACTS: artifacts ?? {
      get: async (name: string) => {
        if (!(name in sources)) {
          throw Object.assign(new Error("not found"), { code: "NOT_FOUND" });
        }
        return {
          info: async () => ({ source: sources[name], defaultBranch: "main" }),
          log,
          [Symbol.dispose]: () => undefined,
        };
      },
    },
    TEST_RUNNER: { getByName },
  } as unknown as Env;
  const Service = MergeService as unknown as new (ctx: unknown, env: Env) => MergeService;
  return { service: new Service({}, env), merge, trial, getByName, log };
}

function post(body: unknown): Request {
  const text = typeof body === "string" ? body : JSON.stringify(body);
  return new Request("https://steward.internal/merge", { method: "POST", body: text });
}

function postTrial(body: unknown): Request {
  const text = typeof body === "string" ? body : JSON.stringify(body);
  return new Request("https://steward.internal/trial", { method: "POST", body: text });
}

describe("MergeService /trial", () => {
  const MAIN = "c".repeat(40);

  beforeEach(() => {
    vi.spyOn(console, "error").mockImplementation(() => {});
  });

  it("runs a trial of the fork's commit on the given main, and never a merge", async () => {
    const clean = { outcome: "clean", base: MAIN, head: COMMIT, commit: COMMIT };
    const outcome = { before: clean, after: clean };
    const { service, merge, trial } = build({ "demo--a1": "artifacts:tessel/demo" });
    trial.mockResolvedValue(outcome.before);
    const response = await service.fetch(
      postTrial({ repo: "demo", fork: "demo--a1", before: MAIN, main: MAIN, commit: COMMIT }),
    );
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual(outcome);
    expect(trial).toHaveBeenCalledWith("demo", "demo--a1", MAIN, COMMIT);
    expect(merge).not.toHaveBeenCalled();
  });

  it("runs the baseline and the new main in two test runners, each with a name of its own", async () => {
    const other = "d".repeat(40);
    const clean = (base: string) => ({ outcome: "clean", base, head: COMMIT, commit: COMMIT });
    const { service, trial, getByName } = build({ "demo--a1": "artifacts:tessel/demo" });
    trial.mockImplementation(async (_repo, _fork, main: string) => clean(main));
    const response = await service.fetch(
      postTrial({ repo: "demo", fork: "demo--a1", before: other, main: MAIN, commit: COMMIT }),
    );
    expect(await response.json()).toEqual({ before: clean(other), after: clean(MAIN) });
    expect(trial.mock.calls.map((call) => call[2])).toEqual([other, MAIN]);
    const names = getByName.mock.calls.map((call) => call[0]);
    expect(names).toHaveLength(2);
    expect(new Set(names).size).toBe(2);
  });

  it("reads the fork's head once when the request names no commit, and tries it on both sides", async () => {
    const other = "d".repeat(40);
    const { service, trial, log } = build({ "demo--a1": "artifacts:tessel/demo" });
    trial.mockResolvedValue({ outcome: "commit_not_in_fork" });
    await service.fetch(postTrial({ repo: "demo", fork: "demo--a1", before: other, main: MAIN }));
    expect(log).toHaveBeenCalledTimes(1);
    expect(log).toHaveBeenCalledWith({ ref: "main", limit: 1 });
    expect(trial.mock.calls).toEqual([
      ["demo", "demo--a1", other, FORK_HEAD],
      ["demo", "demo--a1", MAIN, FORK_HEAD],
    ]);
  });

  it("does not read the fork's head when the request names a commit", async () => {
    const { service, trial, log } = build({ "demo--a1": "artifacts:tessel/demo" });
    trial.mockResolvedValue({ outcome: "commit_not_in_fork" });
    await service.fetch(
      postTrial({ repo: "demo", fork: "demo--a1", before: MAIN, main: MAIN, commit: COMMIT }),
    );
    expect(log).not.toHaveBeenCalled();
    expect(trial).toHaveBeenCalledWith("demo", "demo--a1", MAIN, COMMIT);
  });

  it("answers 502 when the fork's head cannot be read, and runs no trial", async () => {
    const { service, trial, log } = build({ "demo--a1": "artifacts:tessel/demo" });
    log.mockResolvedValue([]);
    const response = await service.fetch(
      postTrial({ repo: "demo", fork: "demo--a1", before: MAIN, main: MAIN }),
    );
    expect(response.status).toBe(502);
    expect(trial).not.toHaveBeenCalled();
  });

  it("starts the baseline and the main trial before either has answered", async () => {
    const other = "d".repeat(40);
    const { service, trial } = build({ "demo--a1": "artifacts:tessel/demo" });
    const answers: Array<(outcome: unknown) => void> = [];
    trial.mockImplementation(() => new Promise((resolve) => answers.push(resolve)));
    const pending = service.fetch(
      postTrial({ repo: "demo", fork: "demo--a1", before: other, main: MAIN, commit: COMMIT }),
    );
    await vi.waitFor(() => expect(trial).toHaveBeenCalledTimes(2));
    expect(answers).toHaveLength(2);
    for (const answer of answers) {
      answer({ outcome: "clean", base: MAIN, head: COMMIT, commit: COMMIT });
    }
    expect((await pending).status).toBe(200);
  });

  it("keeps no main result when the baseline is not clean, even if main passed", async () => {
    const other = "d".repeat(40);
    const clean = { outcome: "clean", base: MAIN, head: COMMIT, commit: COMMIT };
    const failing = { outcome: "tests_failed", base: other, head: COMMIT, commit: COMMIT };
    const { service, trial } = build({ "demo--a1": "artifacts:tessel/demo" });
    trial.mockImplementation(async (_repo, _fork, main: string) =>
      main === other ? failing : clean,
    );
    const response = await service.fetch(
      postTrial({ repo: "demo", fork: "demo--a1", before: other, main: MAIN, commit: COMMIT }),
    );
    expect(await response.json()).toEqual({ before: failing, after: null });
  });

  it("refuses a fork that is not a fork of the repo without running a trial", async () => {
    const { service, trial } = build({ "other--a1": "artifacts:tessel/other" });
    const response = await service.fetch(
      postTrial({ repo: "demo", fork: "other--a1", before: MAIN, main: MAIN, commit: COMMIT }),
    );
    expect(response.status).toBe(400);
    expect(trial).not.toHaveBeenCalled();
  });

  it.each([
    ["a body that is not JSON", "not json"],
    ["a body without a main", { repo: "demo", fork: "demo--a1", before: MAIN, commit: COMMIT }],
    ["a body without a baseline", { repo: "demo", fork: "demo--a1", main: MAIN }],
    ["a main that is a ref", { repo: "demo", fork: "demo--a1", main: "main" }],
    [
      "a commit that is not a sha",
      { repo: "demo", fork: "demo--a1", before: MAIN, main: MAIN, commit: "x" },
    ],
  ])("answers 400 for %s", async (_label, body) => {
    const { service, trial } = build({ "demo--a1": "artifacts:tessel/demo" });
    expect((await service.fetch(postTrial(body))).status).toBe(400);
    expect(trial).not.toHaveBeenCalled();
  });

  it("answers 404 for a missing fork, and 502 with a fixed message when the trial throws", async () => {
    const missing = build({});
    const gone = await missing.service.fetch(
      postTrial({ repo: "demo", fork: "demo--ghost", before: MAIN, main: MAIN }),
    );
    expect(gone.status).toBe(404);

    const { service, trial } = build({ "demo--a1": "artifacts:tessel/demo" });
    trial.mockRejectedValue(new Error("boom art_v1_secret"));
    const response = await service.fetch(
      postTrial({ repo: "demo", fork: "demo--a1", before: MAIN, main: MAIN }),
    );
    expect(response.status).toBe(502);
    expect(JSON.stringify(await response.json())).not.toContain("secret");
  });
});

describe("MergeService", () => {
  beforeEach(() => {
    vi.spyOn(console, "error").mockImplementation(() => {});
  });

  it("merges the fork's commit and returns the outcome as JSON", async () => {
    const merge = vi.fn(async () => ({ outcome: "already_merged", base: COMMIT }));
    const { service, trial } = build({ "demo--a1": "artifacts:tessel/demo" }, merge);
    const response = await service.fetch(
      post({ repo: "demo", fork: "demo--a1", commit: COMMIT, scopes: SCOPES }),
    );
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ outcome: "already_merged", base: COMMIT });
    expect(merge).toHaveBeenCalledWith("demo", "demo--a1", COMMIT, SCOPES, false);
    expect(trial).not.toHaveBeenCalled();
  });

  it("never lets a request body turn on the admin merge that may change the gate", async () => {
    const merge = vi.fn(async () => ({ outcome: "gate_changed" }));
    const { service } = build({ "demo--a1": "artifacts:tessel/demo" }, merge);
    for (const adminMerge of [true, "true", 1]) {
      await service.fetch(
        post({ repo: "demo", fork: "demo--a1", commit: COMMIT, scopes: SCOPES, adminMerge }),
      );
    }
    expect(merge).toHaveBeenCalledTimes(3);
    for (const call of merge.mock.calls as unknown[][]) {
      expect(call[4]).toBe(false);
    }
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
