import { describe, expect, it, vi } from "vitest";

import { openRecordRepo, recordDeps } from "./evidence-binding";
import { parseSha, type Sha } from "./merge-types";

function artifactsError(code: string): Error {
  return Object.assign(new Error(code), { code });
}

function sha(character: string): Sha {
  const parsed = parseSha(character.repeat(40));
  if (parsed === undefined) {
    throw new Error("bad test sha");
  }
  return parsed;
}

/** A namespace of repos by name, with `source` per repo, logging every binding call. */
function namespace(existing: Record<string, string | null>) {
  const repos = new Map(Object.entries(existing));
  const calls: string[] = [];
  const handle = (name: string) => ({
    info: async () => ({
      source: repos.get(name) ?? null,
      remote: `https://git.example/${name}.git`,
    }),
    revokeToken: async (token: string) => {
      calls.push(`revoke ${name} ${token}`);
      return true;
    },
    createToken: async (scope: string) => {
      calls.push(`mint ${scope} ${name}`);
      return { id: `${name}-id`, plaintext: "art_v1_secret" };
    },
    [Symbol.dispose]: () => calls.push(`dispose ${name}`),
  });
  const artifacts = {
    get: vi.fn(async (name: string) => {
      calls.push(`get ${name}`);
      if (!repos.has(name)) {
        throw artifactsError("NOT_FOUND");
      }
      return handle(name);
    }),
    create: vi.fn(async (name: string, options: unknown) => {
      calls.push(`create ${name} ${JSON.stringify(options)}`);
      if (repos.has(name)) {
        throw artifactsError("ALREADY_EXISTS");
      }
      repos.set(name, null);
      return { name, token: "art_v1_initial" };
    }),
  };
  return { artifacts: artifacts as unknown as Artifacts, calls, repos };
}

const CREATE_OPTIONS = {
  setDefaultBranch: "main",
  description: "Written by the Tessel steward only",
};

describe("openRecordRepo", () => {
  it("creates the repo with main as its default branch when get says not found", async () => {
    const { artifacts, calls } = namespace({});
    await openRecordRepo(artifacts, "demo--evidence");
    expect(calls).toEqual([
      "get demo--evidence",
      `create demo--evidence ${JSON.stringify(CREATE_OPTIONS)}`,
      "get demo--evidence",
      "revoke demo--evidence art_v1_initial",
      "dispose demo--evidence",
      "get demo--evidence",
    ]);
  });

  it("reuses a repo that exists, creating nothing", async () => {
    const { artifacts, calls } = namespace({ "demo--evidence": null });
    await openRecordRepo(artifacts, "demo--evidence");
    expect(calls).toEqual(["get demo--evidence"]);
  });

  it("gets the repo another merge created between its get and its create", async () => {
    const { artifacts, calls, repos } = namespace({});
    const get = artifacts.get;
    let first = true;
    (artifacts as unknown as { get: unknown }).get = async (name: string) => {
      if (first) {
        first = false;
        repos.set(name, null);
        calls.push(`get ${name}`);
        throw artifactsError("NOT_FOUND");
      }
      return get(name);
    };
    await openRecordRepo(artifacts, "demo--evidence");
    expect(calls.filter((call) => call.startsWith("create"))).toHaveLength(1);
    expect(calls.at(-1)).toBe("get demo--evidence");
  });

  it("refuses a fork of that name: its agent holds write tokens for it", async () => {
    const { artifacts, calls } = namespace({ "demo--evidence": "artifacts:tessel/demo" });
    await expect(openRecordRepo(artifacts, "demo--evidence")).rejects.toThrow("is a fork");
    expect(calls).toContain("dispose demo--evidence");
  });

  it("passes on any other error of get", async () => {
    const { artifacts } = namespace({});
    artifacts.get = async () => {
      throw artifactsError("INTERNAL_ERROR");
    };
    await expect(openRecordRepo(artifacts, "demo--evidence")).rejects.toThrow("INTERNAL_ERROR");
  });
});

function recordSandbox(existing: Record<string, string | null>) {
  const { artifacts, calls } = namespace(existing);
  const gateway = vi.fn(() => ({ name: "record-gateway" }));
  const intercepted: string[] = [];
  const ctx = { exports: { RecordPushGateway: gateway } } as unknown as DurableObjectState;
  const container = {
    interceptOutboundHttps: async (host: string) => {
      intercepted.push(host);
    },
  } as unknown as Container;
  const env = { ARTIFACTS: artifacts } as unknown as Env;
  const fork = {} as ArtifactsRepo;
  const built = recordDeps({ ctx, env, container, host: "git.example", fork, user: {} });
  return { ...built, calls, gateway, intercepted };
}

describe("recordDeps", () => {
  it("pins the gateway to the one update, revoking the token after a use that threw", async () => {
    const { deps, calls, gateway, intercepted } = recordSandbox({ "demo--evidence": null });
    const repo = await deps.openRepo("demo--evidence");
    const update = { old: null, new: sha("e") };
    await expect(
      deps.withRecordAccess(repo, update, async () => {
        throw new Error("container died");
      }),
    ).rejects.toThrow("container died");
    expect(gateway).toHaveBeenCalledWith({
      props: {
        remote: "https://git.example/demo--evidence.git",
        ref: "refs/heads/main",
        old: "0".repeat(40),
        new: sha("e"),
        token: "art_v1_secret",
      },
    });
    expect(intercepted).toEqual(["git.example"]);
    expect(calls.slice(-2)).toEqual([
      "mint write demo--evidence",
      "revoke demo--evidence demo--evidence-id",
    ]);
  });

  it("gives a fork file's size without reading its bytes until asked", async () => {
    const arrayBuffer = vi.fn(async () => new TextEncoder().encode("hello").buffer);
    const fork = {
      readFile: async (args: { ref: string; path: string }) =>
        args.path === "transcripts/7.md" ? { size: 5, arrayBuffer } : null,
    } as unknown as ArtifactsRepo;
    const { deps } = recordDeps({
      ctx: {} as DurableObjectState,
      env: {} as Env,
      container: {} as Container,
      host: "git.example",
      fork,
      user: {},
    });
    expect(await deps.readForkFile("tessel/transcript-7", "transcripts/8.md")).toBeNull();
    const file = await deps.readForkFile("tessel/transcript-7", "transcripts/7.md");
    expect(file?.size).toBe(5);
    expect(arrayBuffer).not.toHaveBeenCalled();
    expect(new TextDecoder().decode(await file?.bytes())).toBe("hello");
  });

  it("refuses a record repo on another git host than the sandbox's", async () => {
    const remote = "https://elsewhere.example/demo--evidence.git";
    const handle = { info: async () => ({ source: null, remote }), [Symbol.dispose]: () => {} };
    const env = { ARTIFACTS: { get: async () => handle } } as unknown as Env;
    const { deps } = recordDeps({
      ctx: {} as DurableObjectState,
      env,
      container: {} as Container,
      host: "git.example",
      fork: {} as ArtifactsRepo,
      user: {},
    });
    await expect(deps.openRepo("demo--evidence")).rejects.toThrow("git host");
  });
});
