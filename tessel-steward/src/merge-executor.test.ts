import { describe, expect, it, vi } from "vitest";

import { NETWORK_TIMEOUT_SECONDS } from "./merge-commands";
import { executeMerge, redactOutcome } from "./merge-executor";
import { parseSha, type MergeOutcome, type Sha } from "./merge-types";

function sha(character: string): Sha {
  const parsed = parseSha(character.repeat(40));
  if (parsed === undefined) {
    throw new Error("bad test sha");
  }
  return parsed;
}

const BASE = sha("1");
const MERGE_BASE = sha("2");
const COMMIT = sha("3");
const HEAD = sha("4");
const encoder = new TextEncoder();

interface World {
  forkSource: string | null;
  testExit: number;
  testOutput: string;
  pushExit: number;
  mainNow: Sha;
  forkHost: string;
  mainReadFails: boolean;
  changed: string;
}

function textStream(text: string): ReadableStream<Uint8Array> {
  return new ReadableStream({
    start(controller) {
      controller.enqueue(encoder.encode(text));
      controller.close();
    },
  });
}

/** Answers what git, node and npm would print, from the argv the executor sends. */
function respond(argv: string[], world: World): { exitCode: number; stdout: string } {
  const text = argv.join(" ");
  const answers: Array<[string, { exitCode: number; stdout: string }]> = [
    ["origin/main^{commit}", { exitCode: 0, stdout: `${BASE}\n` }],
    ["--name-status", { exitCode: 0, stdout: world.changed }],
    ["merge-base --is-ancestor", { exitCode: 0, stdout: "" }],
    [" merge-base ", { exitCode: 0, stdout: `${MERGE_BASE}\n` }],
    ["HEAD^{commit}", { exitCode: 0, stdout: `${HEAD}\n` }],
    ["npm test", { exitCode: world.testExit, stdout: world.testOutput }],
    [" push ", { exitCode: world.pushExit, stdout: "" }],
    ["--quiet", { exitCode: 0, stdout: `${COMMIT}\n` }],
  ];
  return answers.find(([needle]) => text.includes(needle))?.[1] ?? { exitCode: 0, stdout: "" };
}

function build(overrides: Partial<World> = {}) {
  const world: World = {
    forkSource: "artifacts:tessel/demo",
    testExit: 0,
    testOutput: "ok",
    pushExit: 0,
    mainNow: HEAD,
    forkHost: "git.example",
    mainReadFails: false,
    changed: "M\0src/a.ts\0",
    ...overrides,
  };
  const events: string[] = [];
  const tokens = new Map<string, string>();
  const ttls = new Map<string, number>();

  function repo(name: string) {
    return {
      info: async () => ({
        remote: `https://${name === "demo" ? "git.example" : world.forkHost}/git/tessel/${name}.git`,
        defaultBranch: "main",
        source: name === "demo" ? null : world.forkSource,
      }),
      createToken: async (scope: string, ttl: number) => {
        const id = `${name}-${scope}`;
        tokens.set(id, `art_v1_secret-${id}`);
        events.push(`mint ${scope} ${name}`);
        ttls.set(`${scope} ${name}`, ttl);
        return { id, plaintext: `art_v1_secret-${id}`, scope };
      },
      revokeToken: async (id: string) => {
        events.push(`revoke ${id}`);
        return true;
      },
      log: async () => {
        if (world.mainReadFails) {
          throw new Error("artifacts unavailable");
        }
        return [{ hash: world.mainNow }];
      },
      [Symbol.dispose]: () => undefined,
    };
  }

  let running = false;
  const container = {
    images: { tests: "image" },
    get running() {
      return running;
    },
    start: () => {
      running = true;
      events.push("start");
    },
    destroy: async () => {
      running = false;
      events.push("destroy");
    },
    interceptOutboundHttps: async (_host: string, gateway: { name: string }) => {
      events.push(`intercept ${gateway.name}`);
    },
    exec: async (cmd: string[]) => {
      const argv = cmd.slice(3);
      const { exitCode, stdout } = respond(argv, world);
      events.push(`exec ${argv.join(" ")}`);
      return {
        stdout: textStream(stdout),
        stderr: textStream(""),
        exitCode: Promise.resolve(exitCode),
      };
    },
  };
  const exportsStub = {
    MergeReadGateway: () => ({ name: "read-gateway" }),
    MergePushGateway: () => ({ name: "push-gateway" }),
  };
  const ctx = { container, exports: exportsStub } as unknown as DurableObjectState;
  const env = { ARTIFACTS: { get: async (name: string) => repo(name) } } as unknown as Env;
  return { ctx, env, events, world, ttls };
}

const request = {
  fork: "demo--a1",
  commit: COMMIT,
  scopes: [{ scope: { kind: "dir" as const, path: "src" }, mode: "edit_body" as const }],
};

describe("executeMerge", () => {
  it("merges, minting the write token only after the tests ran and revoking it after the push", async () => {
    const { ctx, env, events } = build();
    const outcome = await executeMerge(ctx, env, "demo", request);

    expect(outcome).toEqual({ outcome: "merged", base: BASE, head: HEAD });
    const at = (needle: string) => events.findIndex((event) => event.includes(needle));
    expect(at("mint write")).toBeGreaterThan(at("npm test"));
    expect(at("postBuffer")).toBeGreaterThan(at("mint write"));
    expect(at("revoke demo-write")).toBeGreaterThan(at("postBuffer"));
    expect(events.filter((event) => event.startsWith("mint write"))).toHaveLength(1);
  });

  it("has revoked both read tokens before the first command that is not a fetch", async () => {
    const { ctx, env, events } = build();
    await executeMerge(ctx, env, "demo", request);
    const firstLocal = events.findIndex((event) => event.includes("rev-parse"));
    expect(events.indexOf("revoke demo-read")).toBeLessThan(firstLocal);
    expect(events.indexOf("revoke demo--a1-read")).toBeLessThan(firstLocal);
  });

  it("installs the push gateway only after the tests", async () => {
    const { ctx, env, events } = build();
    await executeMerge(ctx, env, "demo", request);
    expect(events.indexOf("intercept read-gateway")).toBeLessThan(events.indexOf("start"));
    expect(events.indexOf("intercept push-gateway")).toBeGreaterThan(
      events.findIndex((event) => event.includes("npm test")),
    );
  });

  it("mints no write token and never pushes when the tests fail", async () => {
    const { ctx, env, events } = build({ testExit: 1 });
    const outcome = await executeMerge(ctx, env, "demo", request);
    expect(outcome.outcome).toBe("tests_failed");
    expect(events.some((event) => event.startsWith("mint write"))).toBe(false);
    expect(events.some((event) => event.includes(" push "))).toBe(false);
    expect(events).toContain("destroy");
  });

  it("rejects an uncovered change before the install and the tests, minting no write token", async () => {
    const { ctx, env, events } = build({ changed: "M\0src/a.ts\0A\0docs/new.md\0" });
    const outcome = await executeMerge(ctx, env, "demo", request);

    expect(outcome).toEqual({
      outcome: "uncovered",
      base: BASE,
      head: HEAD,
      files: ["docs/new.md"],
      total: 1,
    });
    expect(events.some((event) => event.includes("npm"))).toBe(false);
    expect(events.some((event) => event.startsWith("mint write"))).toBe(false);
    expect(events).not.toContain("intercept push-gateway");
    expect(events.some((event) => event.includes(" push "))).toBe(false);
    expect(events).toContain("destroy");
  });

  it("refuses a repo that is not a fork of the main repo before creating any token", async () => {
    for (const forkSource of [null, "github:owner/demo", "artifacts:tessel/other"]) {
      const { ctx, env, events } = build({ forkSource });
      await expect(executeMerge(ctx, env, "demo", request)).rejects.toThrow("not a fork");
      expect(events).toEqual([]);
    }
  });

  it("revokes the write token and destroys the container when the push cannot start", async () => {
    const { ctx, env, events } = build();
    const exec = (ctx.container as unknown as { exec: (cmd: string[]) => Promise<unknown> }).exec;
    (ctx.container as unknown as { exec: unknown }).exec = async (cmd: string[]) => {
      if (cmd.join(" ").includes(" push ")) {
        throw new Error("container died");
      }
      return exec(cmd);
    };
    await expect(executeMerge(ctx, env, "demo", request)).rejects.toThrow("container died");
    expect(events).toContain("revoke demo-write");
    expect(events.at(-1)).toBe("destroy");
  });

  it("redacts tokens that the repo's tests print", async () => {
    const { ctx, env } = build({ testExit: 1, testOutput: "using art_v1_secret-demo-read now" });
    const outcome = await executeMerge(ctx, env, "demo", request);
    expect(JSON.stringify(outcome)).not.toContain("secret-demo-read");
    expect(outcome).toMatchObject({ outcome: "tests_failed" });
  });

  it("mints read tokens that outlive the clone and the fetch, and a short write token", async () => {
    const { ctx, env, ttls } = build();
    await executeMerge(ctx, env, "demo", request);
    expect(ttls.get("read demo")).toBeGreaterThanOrEqual(2 * NETWORK_TIMEOUT_SECONDS);
    expect(ttls.get("read demo--a1")).toBeGreaterThanOrEqual(2 * NETWORK_TIMEOUT_SECONDS);
    expect(ttls.get("write demo")).toBe(60);
  });

  it("refuses a fork on another git host before creating any token", async () => {
    const { ctx, env, events } = build({ forkHost: "evil.example" });
    await expect(executeMerge(ctx, env, "demo", request)).rejects.toThrow("same git host");
    expect(events).toEqual([]);
  });

  it("does not report merged when the push exits 0 but main did not move", async () => {
    const { ctx, env } = build({ mainNow: BASE });
    expect(await executeMerge(ctx, env, "demo", request)).toMatchObject({
      outcome: "push_failed",
    });
  });

  it("reports push_failed, never merged or main_moved, when main cannot be read", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    const { ctx, env } = build({ mainReadFails: true });
    expect(await executeMerge(ctx, env, "demo", request)).toMatchObject({
      outcome: "push_failed",
    });
  });

  it("reports main_moved when the push is rejected and main has changed", async () => {
    const racer = sha("9");
    const { ctx, env } = build({ pushExit: 1, mainNow: racer });
    expect(await executeMerge(ctx, env, "demo", request)).toEqual({
      outcome: "main_moved",
      expected: BASE,
      actual: racer,
    });
  });
});

describe("redactOutcome", () => {
  const leaked = {
    exitCode: 1,
    stdout: "art_v1_aaa",
    stderr: "art_v1_bbb",
    stdoutTruncated: false,
    stderrTruncated: false,
  };

  it("redacts every outcome that carries output", () => {
    const outcomes: MergeOutcome[] = [
      { outcome: "git_failed", result: leaked },
      { outcome: "push_failed", base: BASE, head: HEAD, result: leaked },
      { outcome: "clone", result: { ...leaked, step: "clone", passed: false } },
    ];
    for (const outcome of outcomes) {
      const text = JSON.stringify(redactOutcome(outcome));
      expect(text).not.toContain("aaa");
      expect(text).not.toContain("bbb");
    }
  });

  it("returns outcomes without output unchanged", () => {
    const merged: MergeOutcome = { outcome: "merged", base: BASE, head: HEAD };
    expect(redactOutcome(merged)).toBe(merged);
  });
});
