import { describe, expect, it, vi } from "vitest";

import { executeMerge, executeTrial, redactOutcome, redactTrialOutcome } from "./merge-executor";
import { STEP_SECONDS } from "./step-budget";
import { parseSha, type MergeOutcome, type Sha, type TrialOutcome } from "./merge-types";

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
const MAIN_AT_TRIAL = sha("6");
const encoder = new TextEncoder();

interface World {
  forkSource: string | null;
  testExit: number;
  testOutput: string;
  pushExit: number;
  /** Main as the steward reads it before the sandbox starts. */
  mainBefore: Sha;
  /** Main as read after the push. */
  mainNow: Sha;
  /** Text of `tessel.toml` on main, or null for none. */
  tesselToml: string | null;
  installExit: number;
  /** Exit code of the dependency check of a repo without a usable tessel.toml. */
  depsExit: number;
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
    ["cargo test", { exitCode: world.testExit, stdout: world.testOutput }],
    ["pnpm test", { exitCode: world.testExit, stdout: world.testOutput }],
    ["node -e", { exitCode: world.depsExit, stdout: "" }],
    ["cargo fetch", { exitCode: world.installExit, stdout: "" }],
    ["pnpm install", { exitCode: world.installExit, stdout: "" }],
    ["memory.peak", { exitCode: 0, stdout: "123456\n" }],
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
    mainBefore: BASE,
    mainNow: HEAD,
    tesselToml: null,
    installExit: 0,
    depsExit: 0,
    forkHost: "git.example",
    mainReadFails: false,
    changed: "M\0src/a.ts\0",
    ...overrides,
  };
  const events: string[] = [];
  const tokens = new Map<string, string>();
  const ttls = new Map<string, number>();
  const reads: Array<{ repo: string; ref: string; path: string }> = [];
  const starts: unknown[] = [];
  const execUsers: Array<{ command: string; user: string | undefined; cwd: string | undefined }> =
    [];
  let pushed = false;

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
      readFile: async (args: { ref: string; path: string }) => {
        reads.push({ repo: name, ...args });
        return world.tesselToml === null ? null : new Blob([world.tesselToml]);
      },
      log: async () => {
        if (!pushed) {
          return [{ hash: world.mainBefore }];
        }
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
    images: { tests: "lite-image", toolchain: "toolchain-image" },
    get running() {
      return running;
    },
    start: (options: unknown) => {
      starts.push(options);
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
    exec: async (cmd: string[], options?: { user?: string; cwd?: string }) => {
      const argv = cmd.slice(3);
      execUsers.push({ command: argv.join(" "), user: options?.user, cwd: options?.cwd });
      const { exitCode, stdout } = respond(argv, world);
      events.push(`exec ${argv.join(" ")}`);
      if (argv.join(" ").includes(" push ")) {
        pushed = true;
      }
      return {
        stdout: textStream(stdout),
        stderr: textStream(""),
        exitCode: Promise.resolve(exitCode),
      };
    },
  };
  const pushGateway = vi.fn(() => ({ name: "push-gateway" }));
  const exportsStub = {
    MergeReadGateway: () => ({ name: "read-gateway" }),
    MergePushGateway: pushGateway,
  };
  const ctx = { container, exports: exportsStub } as unknown as DurableObjectState;
  const env = { ARTIFACTS: { get: async (name: string) => repo(name) } } as unknown as Env;
  return { ctx, env, events, world, ttls, pushGateway, reads, starts, execUsers };
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
    expect(ttls.get("read demo")).toBeGreaterThanOrEqual(STEP_SECONDS.clone + STEP_SECONDS.fetch);
    expect(ttls.get("read demo--a1")).toBeGreaterThanOrEqual(
      STEP_SECONDS.clone + STEP_SECONDS.fetch,
    );
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

describe("executeTrial", () => {
  const trialRequest = { fork: "demo--a1", main: MAIN_AT_TRIAL, commit: COMMIT };

  it("tests the commit on the given main and tears the sandbox down", async () => {
    const { ctx, env, events } = build();
    const outcome = await executeTrial(ctx, env, "demo", trialRequest);

    expect(outcome).toEqual({ outcome: "clean", base: MAIN_AT_TRIAL, head: HEAD, commit: COMMIT });
    expect(events.at(-1)).toBe("destroy");
    expect(events.filter((event) => event.startsWith("mint read"))).toHaveLength(2);
    expect(events).toContain("revoke demo-read");
    expect(events).toContain("revoke demo--a1-read");
  });

  it("never mints a write token, installs the push gateway or pushes, whatever the tests say", async () => {
    for (const testExit of [0, 1, 124]) {
      const { ctx, env, events, pushGateway } = build({ testExit });
      await executeTrial(ctx, env, "demo", trialRequest);
      expect(events.some((event) => event.startsWith("mint write"))).toBe(false);
      expect(events.some((event) => event.includes(" push "))).toBe(false);
      expect(events).not.toContain("intercept push-gateway");
      expect(pushGateway).not.toHaveBeenCalled();
    }
  });

  it("classifies failing tests as tests_failed, not as a merge result", async () => {
    const { ctx, env } = build({ testExit: 1 });
    expect(await executeTrial(ctx, env, "demo", trialRequest)).toMatchObject({
      outcome: "tests_failed",
      base: MAIN_AT_TRIAL,
      commit: COMMIT,
    });
  });

  it("does not check coverage: a change to any file is tried", async () => {
    const { ctx, env, events } = build({ changed: "A\0docs/new.md\0" });
    expect(await executeTrial(ctx, env, "demo", trialRequest)).toMatchObject({
      outcome: "clean",
    });
    expect(events.some((event) => event.includes("--name-status"))).toBe(false);
  });

  it("refuses a repo that is not a fork of the main repo before creating any token", async () => {
    for (const forkSource of [null, "github:owner/demo", "artifacts:tessel/other"]) {
      const { ctx, env, events } = build({ forkSource });
      await expect(executeTrial(ctx, env, "demo", trialRequest)).rejects.toThrow("not a fork");
      expect(events).toEqual([]);
    }
  });

  it("redacts tokens that the repo's tests print", async () => {
    const { ctx, env } = build({ testExit: 1, testOutput: "using art_v1_secret-demo-read now" });
    const outcome = await executeTrial(ctx, env, "demo", trialRequest);
    expect(JSON.stringify(outcome)).not.toContain("secret-demo-read");
  });

  it("starts a container and mints read tokens of its own on every call, and tears them down", async () => {
    const { ctx, env, events } = build();
    await executeTrial(ctx, env, "demo", trialRequest);
    await executeTrial(ctx, env, "demo", trialRequest);
    expect(events.filter((event) => event === "start")).toHaveLength(2);
    expect(events.filter((event) => event === "destroy")).toHaveLength(2);
    expect(events.filter((event) => event.startsWith("mint read"))).toHaveLength(4);
    expect(events.filter((event) => event.startsWith("revoke"))).toHaveLength(4);
    expect(events.indexOf("destroy")).toBeLessThan(events.lastIndexOf("start"));
  });
});

describe("redactTrialOutcome", () => {
  const leaked = {
    exitCode: 1,
    stdout: "art_v1_aaa",
    stderr: "art_v1_bbb",
    stdoutTruncated: false,
    stderrTruncated: false,
  };

  it("redacts every outcome that carries output", () => {
    const step = { ...leaked, step: "test" as const, passed: false };
    const outcomes: TrialOutcome[] = [
      { outcome: "git_failed", result: leaked },
      { outcome: "tests_failed", base: BASE, head: HEAD, commit: COMMIT, result: step },
      { outcome: "install", base: BASE, head: HEAD, commit: COMMIT, result: step },
      { outcome: "clone", result: step },
    ];
    for (const outcome of outcomes) {
      const text = JSON.stringify(redactTrialOutcome(outcome));
      expect(text).not.toContain("aaa");
      expect(text).not.toContain("bbb");
    }
  });

  it("returns outcomes without output unchanged", () => {
    const clean: TrialOutcome = { outcome: "clean", base: BASE, head: HEAD, commit: COMMIT };
    expect(redactTrialOutcome(clean)).toBe(clean);
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

const GATE = `
instance = "standard-4"

[install]
cargo = true
pnpm = ["tessel-steward"]

[[test]]
argv = ["cargo", "test", "--workspace", "--locked", "--offline"]

[[test]]
dir = "tessel-steward"
argv = ["pnpm", "test"]
`;

describe("a repo with a tessel.toml on main", () => {
  const trialRequest = { fork: "demo--a1", main: MAIN_AT_TRIAL, commit: COMMIT };

  it("starts the toolchain image on the instance it asks for, and legacy repos on lite", async () => {
    const configured = build({ tesselToml: GATE });
    await executeMerge(configured.ctx, configured.env, "demo", request);
    expect(configured.starts).toEqual([
      { image: "toolchain-image", enableInternet: false, instance: "standard-4" },
    ]);

    const legacy = build();
    await executeMerge(legacy.ctx, legacy.env, "demo", request);
    expect(legacy.starts).toEqual([
      { image: "lite-image", enableInternet: false, instance: "lite" },
    ]);
  });

  it("runs every command of a configured gate, git included, as the unprivileged user", async () => {
    const configured = build({ tesselToml: GATE });
    await executeMerge(configured.ctx, configured.env, "demo", request);
    const repoCommands = configured.execUsers.filter(({ command }) => !command.startsWith("cat "));
    expect(repoCommands.length).toBeGreaterThan(10);
    expect(repoCommands.filter(({ user }) => user !== "node")).toEqual([]);

    const legacy = build();
    await executeMerge(legacy.ctx, legacy.env, "demo", request);
    expect(legacy.execUsers.filter(({ user }) => user !== undefined)).toEqual([]);
  });

  it("reads the gate from main at the commit the run is based on, never from the fork", async () => {
    const merge = build({ tesselToml: GATE });
    await executeMerge(merge.ctx, merge.env, "demo", request);
    expect(merge.reads).toEqual([{ repo: "demo", ref: BASE, path: "tessel.toml" }]);

    const trial = build({ tesselToml: GATE });
    await executeTrial(trial.ctx, trial.env, "demo", trialRequest);
    expect(trial.reads).toEqual([{ repo: "demo", ref: MAIN_AT_TRIAL, path: "tessel.toml" }]);
  });

  it("reads the gate before the container starts", async () => {
    const { ctx, env, events, reads } = build({ tesselToml: GATE });
    let readsAtStart = -1;
    const container = ctx.container as unknown as { start: (options: unknown) => void };
    const start = container.start;
    container.start = (options) => {
      readsAtStart = reads.length;
      start(options);
    };
    await executeMerge(ctx, env, "demo", request);
    expect(readsAtStart).toBe(1);
    expect(events).toContain("start");
  });

  it("runs the installs with no network, scripts or lockfile drift, then the trunk's tests", async () => {
    const { ctx, env, events } = build({ tesselToml: GATE });
    expect(await executeMerge(ctx, env, "demo", request)).toMatchObject({ outcome: "merged" });
    const execs = events.filter((event) => event.startsWith("exec "));
    const commands = execs.map((event) => event.slice(5));
    const at = (needle: string) => commands.findIndex((command) => command.includes(needle));
    expect(commands).toContain("cargo fetch --locked --offline");
    expect(commands).toContain("pnpm install --offline --frozen-lockfile --ignore-scripts");
    expect(commands).toContain("cargo test --workspace --locked --offline");
    expect(commands).toContain("pnpm test");
    expect(commands.includes("npm test")).toBe(false);
    expect(at("cargo fetch")).toBeLessThan(at("pnpm install"));
    expect(at("pnpm install")).toBeLessThan(at("cargo test"));
    expect(at("cargo test")).toBeLessThan(at("pnpm test"));
  });

  it("stops at install, as infrastructure, when an install command fails", async () => {
    const { ctx, env, events } = build({ tesselToml: GATE, installExit: 1 });
    const outcome = await executeMerge(ctx, env, "demo", request);
    expect(outcome).toMatchObject({
      outcome: "install",
      result: { step: "install", reason: "install_failed" },
    });
    expect(events.some((event) => event.includes("cargo test"))).toBe(false);
    expect(events.some((event) => event.startsWith("mint write"))).toBe(false);
  });

  it("reports a test step that ran out of time as infrastructure, never as failing tests", async () => {
    for (const testExit of [124, 137]) {
      const merge = build({ tesselToml: GATE, testExit });
      const merged = await executeMerge(merge.ctx, merge.env, "demo", request);
      expect(merged).toMatchObject({
        outcome: "install",
        result: { reason: "timeout", passed: false },
      });
      expect(merge.events.some((event) => event.startsWith("mint write"))).toBe(false);

      const trial = build({ tesselToml: GATE, testExit });
      expect(await executeTrial(trial.ctx, trial.env, "demo", trialRequest)).toMatchObject({
        outcome: "install",
        result: { reason: "timeout" },
      });
    }
  });

  it("keeps a real test failure a test failure, with the measured wall time and peak memory", async () => {
    const { ctx, env } = build({ tesselToml: GATE, testExit: 1 });
    const outcome = await executeMerge(ctx, env, "demo", request);
    expect(outcome).toMatchObject({
      outcome: "tests_failed",
      result: { step: "test", exitCode: 1 },
    });
    const { result } = outcome as Extract<MergeOutcome, { outcome: "tests_failed" }>;
    expect(result.measurement?.peakMemoryBytes).toBe(123456);
    expect(result.measurement?.wallMs).toBeGreaterThanOrEqual(0);
  });

  it("is main_moved, with nothing run, when main is not the commit the gate was read at", async () => {
    const racer = sha("9");
    const { ctx, env, events } = build({ tesselToml: GATE, mainBefore: racer });
    expect(await executeMerge(ctx, env, "demo", request)).toEqual({
      outcome: "main_moved",
      expected: racer,
      actual: BASE,
    });
    expect(events.some((event) => event.includes("cargo"))).toBe(false);
    expect(events.some((event) => event.startsWith("mint write"))).toBe(false);
  });

  it("refuses, at install with a fixed reason, a repo with dependencies whose tessel.toml is invalid", async () => {
    for (const tesselToml of ['instance = "standard-4"\nshell = "sh"\n', "not toml [", ""]) {
      const { ctx, env, starts, events } = build({ tesselToml, depsExit: 3 });
      const outcome = await executeMerge(ctx, env, "demo", request);
      expect(outcome).toMatchObject({
        outcome: "install",
        result: { step: "install", reason: "config", stdout: "" },
      });
      expect(starts).toEqual([{ image: "lite-image", enableInternet: false, instance: "lite" }]);
      expect(events.some((event) => event.includes("npm test"))).toBe(false);
    }
  });

  it("refuses a repo with dependencies and no tessel.toml, as before", async () => {
    const { ctx, env } = build({ depsExit: 3 });
    expect(await executeMerge(ctx, env, "demo", request)).toMatchObject({
      outcome: "install",
      result: { reason: "dependencies" },
    });
  });

  it("runs the dependency check outside the repo, so an unparsable package.json cannot stop node", async () => {
    const { ctx, env, execUsers } = build();
    await executeMerge(ctx, env, "demo", request);
    const check = execUsers.find(({ command }) => command.startsWith("node -e"));
    expect(check?.cwd).toBe("/");
    expect(check?.command.endsWith(" /workspace/package.json")).toBe(true);
  });

  it("still runs npm test for a repo without dependencies and without a tessel.toml", async () => {
    const { ctx, env, events } = build();
    expect(await executeMerge(ctx, env, "demo", request)).toMatchObject({ outcome: "merged" });
    expect(events.some((event) => event.includes("npm test"))).toBe(true);
  });
});
