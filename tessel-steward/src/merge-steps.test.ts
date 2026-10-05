import { describe, expect, it } from "vitest";

import { DEPENDENCIES_DECLARED_EXIT_CODE } from "./dependency-check";
import type { GitCommand } from "./merge-commands";
import type { ClaimedScope } from "./merge-coverage";
import { runMerge, type MergeDeps } from "./merge-steps";
import { parseSha, type GitResult, type Sha } from "./merge-types";
import { REVOKE_FAILED_MESSAGE, makeOutcome, type StepOutcome } from "./run-steps";

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
const RACER = sha("5");
const WHOLE_REPO: ClaimedScope[] = [
  { scope: { kind: "dir", path: "" }, mode: "edit_signature" },
  { scope: { kind: "dir", path: "" }, mode: "create" },
];

type GitStep =
  | "clone"
  | "fetch"
  | "base"
  | "exists"
  | "reachable"
  | "mergebase"
  | "rebase"
  | "conflicts"
  | "changed"
  | "head"
  | "push";

function gitStep(command: GitCommand): GitStep {
  const text = command.argv.join(" ");
  const matches: Array<[string, GitStep]> = [
    [" clone ", "clone"],
    [" fetch ", "fetch"],
    ["origin/main^{commit}", "base"],
    ["--quiet", "exists"],
    ["--is-ancestor", "reachable"],
    [" merge-base ", "mergebase"],
    [" rebase ", "rebase"],
    ["--diff-filter=U", "conflicts"],
    ["--name-status", "changed"],
    ["HEAD^{commit}", "head"],
    [" push ", "push"],
  ];
  const found = matches.find(([needle]) => text.includes(needle));
  if (found === undefined) {
    throw new Error(`unexpected command ${text}`);
  }
  return found[1];
}

interface Plan {
  git: Partial<Record<GitStep, { exitCode: number; stdout?: string }>>;
  install: number;
  test: number;
  revokedReads: boolean;
  mainAfterPush: Sha | null;
}

function harness(overrides: Partial<Plan> = {}) {
  const plan: Plan = {
    git: {},
    install: 0,
    test: 0,
    revokedReads: true,
    mainAfterPush: HEAD,
    ...overrides,
  };
  const defaults: Record<GitStep, { exitCode: number; stdout?: string }> = {
    clone: { exitCode: 0 },
    fetch: { exitCode: 0 },
    base: { exitCode: 0, stdout: `${BASE}\n` },
    exists: { exitCode: 0, stdout: `${COMMIT}\n` },
    reachable: { exitCode: 0 },
    mergebase: { exitCode: 0, stdout: `${MERGE_BASE}\n` },
    rebase: { exitCode: 0 },
    conflicts: { exitCode: 0, stdout: "" },
    changed: { exitCode: 0, stdout: "M\0src/a.ts\0" },
    head: { exitCode: 0, stdout: `${HEAD}\n` },
    push: { exitCode: 0 },
  };
  const events: string[] = [];
  const commands: GitCommand[] = [];
  const deps: MergeDeps = {
    pinnedMain: BASE,
    sources: {
      workspace: "/workspace",
      mainRemote: "https://git.example/demo.git",
      forkRemote: "https://git.example/demo--a1.git",
      forkBranch: "main",
    },
    run: async (command): Promise<GitResult> => {
      const step = gitStep(command);
      events.push(`git:${step}`);
      commands.push(command);
      const { exitCode, stdout = "" } = plan.git[step] ?? defaults[step];
      return {
        exitCode,
        stdout,
        stderr: `${step} err`,
        stdoutTruncated: false,
        stderrTruncated: false,
      };
    },
    revokeReadTokens: async () => {
      events.push("revoke-reads");
      return plan.revokedReads;
    },
    runPackageStep: async (step): Promise<StepOutcome> => {
      events.push(`package:${step}`);
      const exitCode = plan[step];
      return makeOutcome(
        step,
        exitCode,
        { text: `${step} out`, truncated: false },
        { text: "", truncated: false },
      );
    },
    withPushAccess: async (update, use) => {
      events.push(`mint-write ${update.base} ${update.head}`);
      try {
        return await use();
      } finally {
        events.push("revoke-write");
      }
    },
    currentMain: async () => {
      events.push("currentMain");
      return plan.mainAfterPush;
    },
  };
  return { deps, events, commands };
}

describe("runMerge", () => {
  it("fetches, revokes the reads, verifies, rebases, tests, then mints and revokes the write token", async () => {
    const { deps, events } = harness();
    const outcome = await runMerge(deps, COMMIT, WHOLE_REPO);
    expect(outcome).toEqual({ outcome: "merged", base: BASE, head: HEAD });
    expect(events).toEqual([
      "git:clone",
      "git:fetch",
      "revoke-reads",
      "git:base",
      "git:exists",
      "git:reachable",
      "git:mergebase",
      "git:rebase",
      "git:head",
      "git:changed",
      "package:install",
      "package:test",
      `mint-write ${BASE} ${HEAD}`,
      "git:push",
      "revoke-write",
      "currentMain",
    ]);
  });

  it("pushes the rebased head with a lease on the main that was cloned", async () => {
    const { deps, commands } = harness();
    await runMerge(deps, COMMIT, WHOLE_REPO);
    const push = commands.find((command) => gitStep(command) === "push");
    expect(push?.argv).toContain(`--force-with-lease=refs/heads/main:${BASE}`);
    expect(push?.argv).toContain(`${HEAD}:refs/heads/main`);
  });

  it("rebases the submitted commit, not the fork tip, onto the cloned main", async () => {
    const { deps, commands } = harness();
    await runMerge(deps, COMMIT, WHOLE_REPO);
    const rebase = commands.find((command) => gitStep(command) === "rebase");
    expect(rebase?.argv.slice(-4)).toEqual(["--onto", BASE, MERGE_BASE, COMMIT]);
  });

  it("never mints a write token when the tests fail", async () => {
    const { deps, events } = harness({ test: 1 });
    const outcome = await runMerge(deps, COMMIT, WHOLE_REPO);
    expect(outcome).toMatchObject({ outcome: "tests_failed", base: BASE, head: HEAD });
    expect(events.some((event) => event.startsWith("mint-write"))).toBe(false);
    expect(events).not.toContain("git:push");
  });

  it("returns the capped test output with a failed test", async () => {
    const { deps } = harness({ test: 1 });
    const outcome = await runMerge(deps, COMMIT, WHOLE_REPO);
    expect(outcome).toMatchObject({
      outcome: "tests_failed",
      result: { step: "test", exitCode: 1, stdout: "test out", passed: false },
    });
  });

  it("reports a test step that timed out as an install outcome, never a test failure", async () => {
    for (const test of [124, 137]) {
      const { deps, events } = harness({ test });
      const outcome = await runMerge(deps, COMMIT, WHOLE_REPO);
      expect(outcome).toMatchObject({
        outcome: "install",
        result: { step: "install", exitCode: test, stdout: "test out", reason: "timeout" },
      });
      expect(events.some((event) => event.startsWith("mint-write"))).toBe(false);
    }
  });

  it("is main_moved, before verifying or rebasing, when the clone finds main elsewhere", async () => {
    const { deps, events } = harness();
    const racer = sha("9");
    const outcome = await runMerge({ ...deps, pinnedMain: racer }, COMMIT, WHOLE_REPO);
    expect(outcome).toEqual({ outcome: "main_moved", expected: racer, actual: BASE });
    expect(events).not.toContain("git:exists");
    expect(events.some((event) => event.startsWith("package:"))).toBe(false);
  });

  it("reports a stopped rebase as a conflict with the unmerged paths, and runs no tests", async () => {
    const { deps, events } = harness({
      git: {
        rebase: { exitCode: 1 },
        conflicts: { exitCode: 0, stdout: "src/a.ts\0docs/b md\0" },
      },
    });
    const outcome = await runMerge(deps, COMMIT, WHOLE_REPO);
    expect(outcome).toEqual({
      outcome: "conflict",
      base: BASE,
      files: ["src/a.ts", "docs/b md"],
    });
    expect(events.some((event) => event.startsWith("package:"))).toBe(false);
  });

  it("does not call a failed rebase a conflict when no file is unmerged", async () => {
    const { deps } = harness({ git: { rebase: { exitCode: 128 }, conflicts: { exitCode: 0 } } });
    expect(await runMerge(deps, COMMIT, WHOLE_REPO)).toMatchObject({
      outcome: "git_failed",
      result: { exitCode: 128 },
    });
  });

  it.each([
    ["the commit object is missing", { exists: { exitCode: 1 } }],
    ["the commit is not an ancestor of the fork's default branch", { reachable: { exitCode: 1 } }],
  ] satisfies Array<[string, Plan["git"]]>)("refuses when %s", async (_label, git) => {
    const { deps, events } = harness({ git });
    expect(await runMerge(deps, COMMIT, WHOLE_REPO)).toEqual({ outcome: "commit_not_in_fork" });
    expect(events).not.toContain("git:rebase");
    expect(events.some((event) => event.startsWith("package:"))).toBe(false);
  });

  it("calls an unexpected git failure while verifying infrastructure, not a missing commit", async () => {
    const { deps } = harness({ git: { reachable: { exitCode: 128 } } });
    expect(await runMerge(deps, COMMIT, WHOLE_REPO)).toMatchObject({ outcome: "git_failed" });
    const second = harness({ git: { exists: { exitCode: 128 } } });
    expect(await runMerge(second.deps, COMMIT, WHOLE_REPO)).toMatchObject({
      outcome: "git_failed",
    });
  });

  it("reports a failed clone or fetch as a clone outcome and verifies nothing", async () => {
    for (const failing of ["clone", "fetch"] as const) {
      const { deps, events } = harness({ git: { [failing]: { exitCode: 128 } } });
      expect(await runMerge(deps, COMMIT, WHOLE_REPO)).toMatchObject({
        outcome: "clone",
        result: { step: "clone", exitCode: 128, passed: false },
      });
      expect(events).toContain("revoke-reads");
      expect(events).not.toContain("git:base");
    }
  });

  it("runs nothing after a failed revocation of the read tokens", async () => {
    const { deps, events } = harness({ revokedReads: false });
    await expect(runMerge(deps, COMMIT, WHOLE_REPO)).rejects.toThrow(REVOKE_FAILED_MESSAGE);
    expect(events).toEqual(["git:clone", "git:fetch", "revoke-reads"]);
  });

  it("reports a repo with dependencies as an install outcome and never pushes", async () => {
    const { deps, events } = harness({ install: DEPENDENCIES_DECLARED_EXIT_CODE });
    expect(await runMerge(deps, COMMIT, WHOLE_REPO)).toMatchObject({
      outcome: "install",
      base: BASE,
      head: HEAD,
      result: { step: "install", passed: false },
    });
    expect(events).not.toContain("package:test");
    expect(events.some((event) => event.startsWith("mint-write"))).toBe(false);
  });

  it("reports a commit main already contains as already_merged without testing or pushing", async () => {
    const { deps, events } = harness({ git: { head: { exitCode: 0, stdout: `${BASE}\n` } } });
    expect(await runMerge(deps, COMMIT, WHOLE_REPO)).toEqual({
      outcome: "already_merged",
      base: BASE,
    });
    expect(events.some((event) => event.startsWith("package:"))).toBe(false);
    expect(events.some((event) => event.startsWith("mint-write"))).toBe(false);
  });

  it("trusts the read of main, not the exit code: exit 0 with main unchanged is push_failed", async () => {
    const { deps } = harness({ git: { push: { exitCode: 0 } }, mainAfterPush: BASE });
    expect(await runMerge(deps, COMMIT, WHOLE_REPO)).toMatchObject({
      outcome: "push_failed",
      base: BASE,
    });
  });

  it("reports merged when the push exits non-zero but main is at head", async () => {
    const { deps } = harness({ git: { push: { exitCode: 1 } }, mainAfterPush: HEAD });
    expect(await runMerge(deps, COMMIT, WHOLE_REPO)).toEqual({
      outcome: "merged",
      base: BASE,
      head: HEAD,
    });
  });

  it("reports main_moved when the push exits 0 but main is at neither base nor head", async () => {
    const { deps } = harness({ git: { push: { exitCode: 0 } }, mainAfterPush: RACER });
    expect(await runMerge(deps, COMMIT, WHOLE_REPO)).toEqual({
      outcome: "main_moved",
      expected: BASE,
      actual: RACER,
    });
  });

  it("does not report merged when the push exits 0 and main cannot be read", async () => {
    const { deps } = harness({ git: { push: { exitCode: 0 } }, mainAfterPush: null });
    expect(await runMerge(deps, COMMIT, WHOLE_REPO)).toMatchObject({ outcome: "push_failed" });
  });

  it("reports main_moved with both shas when the lease rejects the push", async () => {
    const { deps } = harness({ git: { push: { exitCode: 1 } }, mainAfterPush: RACER });
    expect(await runMerge(deps, COMMIT, WHOLE_REPO)).toEqual({
      outcome: "main_moved",
      expected: BASE,
      actual: RACER,
    });
  });

  it("reports push_failed, not main_moved, when the push fails and main is unchanged", async () => {
    const { deps } = harness({ git: { push: { exitCode: 128 } }, mainAfterPush: BASE });
    expect(await runMerge(deps, COMMIT, WHOLE_REPO)).toMatchObject({
      outcome: "push_failed",
      base: BASE,
      head: HEAD,
      result: { exitCode: 128 },
    });
  });

  it("reports push_failed when the push fails and main cannot be read", async () => {
    const { deps } = harness({ git: { push: { exitCode: 128 } }, mainAfterPush: null });
    expect(await runMerge(deps, COMMIT, WHOLE_REPO)).toMatchObject({ outcome: "push_failed" });
  });

  it("revokes the write token when the push throws", async () => {
    const { deps, events } = harness();
    deps.run = async (command) => {
      if (gitStep(command) === "push") {
        throw new Error("exec failed");
      }
      return {
        exitCode: 0,
        stdout: stdoutOf(gitStep(command)),
        stderr: "",
        stdoutTruncated: false,
        stderrTruncated: false,
      };
    };
    await expect(runMerge(deps, COMMIT, WHOLE_REPO)).rejects.toThrow("exec failed");
    expect(events.at(-1)).toBe("revoke-write");
  });

  it.each(["base", "mergebase", "head"] as const)(
    "reports git_failed when %s does not print a sha",
    async (step) => {
      const { deps } = harness({ git: { [step]: { exitCode: 0, stdout: "not a sha\n" } } });
      expect(await runMerge(deps, COMMIT, WHOLE_REPO)).toMatchObject({ outcome: "git_failed" });
    },
  );
});

function stdoutOf(step: GitStep): string {
  if (step === "head") {
    return `${HEAD}\n`;
  }
  return step === "changed" ? "M\0src/a.ts\0" : `${BASE}\n`;
}

describe("runMerge coverage check (invariant 11)", () => {
  const FILE_ONLY: ClaimedScope[] = [
    { scope: { kind: "file", path: "src/a.ts" }, mode: "edit_body" },
  ];

  it("reads the diff of the rebased range, after the rebase and before any repo code", async () => {
    const { deps, events, commands } = harness();
    await runMerge(deps, COMMIT, WHOLE_REPO);
    const changed = commands.find((command) => gitStep(command) === "changed");
    expect(changed?.argv).toContain(`${BASE}..${HEAD}`);
    expect(events.indexOf("git:changed")).toBeGreaterThan(events.indexOf("git:head"));
    expect(events.indexOf("git:changed")).toBeLessThan(events.indexOf("package:install"));
  });

  it("reads the diff without external diff drivers, with renames and NUL separators", async () => {
    const { deps, commands } = harness();
    await runMerge(deps, COMMIT, WHOLE_REPO);
    const changed = commands.find((command) => gitStep(command) === "changed");
    expect(changed?.argv).toEqual(
      expect.arrayContaining(["--name-status", "-z", "-M", "--no-ext-diff"]),
    );
  });

  it("rejects a change outside the claim as uncovered, running no tests and minting no token", async () => {
    const { deps, events } = harness({
      git: { changed: { exitCode: 0, stdout: "M\0src/a.ts\0A\0src/other.ts\0" } },
    });
    const outcome = await runMerge(deps, COMMIT, FILE_ONLY);
    expect(outcome).toEqual({
      outcome: "uncovered",
      base: BASE,
      head: HEAD,
      files: ["src/other.ts"],
      total: 1,
    });
    expect(events.some((event) => event.startsWith("package:"))).toBe(false);
    expect(events.some((event) => event.startsWith("mint-write"))).toBe(false);
    expect(events).not.toContain("git:push");
  });

  it("merges when the claim covers every changed file", async () => {
    const { deps } = harness();
    expect(await runMerge(deps, COMMIT, FILE_ONLY)).toMatchObject({ outcome: "merged" });
  });

  it("names at most 50 files and counts them all", async () => {
    const records = Array.from({ length: 120 }, (_, i) => `A\0new/f${i}.ts\0`).join("");
    const { deps } = harness({ git: { changed: { exitCode: 0, stdout: records } } });
    const outcome = await runMerge(deps, COMMIT, FILE_ONLY);
    expect(outcome).toMatchObject({ outcome: "uncovered", total: 120 });
    expect(outcome.outcome === "uncovered" ? outcome.files : []).toHaveLength(50);
  });

  it("does not look at the diff when the rebase left nothing to add", async () => {
    const { deps, events } = harness({ git: { head: { exitCode: 0, stdout: `${BASE}\n` } } });
    expect(await runMerge(deps, COMMIT, [])).toEqual({ outcome: "already_merged", base: BASE });
    expect(events).not.toContain("git:changed");
  });

  it.each([
    ["a failed diff", { exitCode: 128, stdout: "" }],
    ["a cut-off diff", { exitCode: 0, stdout: "M\0src/a.ts\0", truncated: true }],
    ["an unknown status", { exitCode: 0, stdout: "X\0src/a.ts\0" }],
    ["a record without a path", { exitCode: 0, stdout: "M\0" }],
  ])("reports git_failed, not covered, for %s", async (_label, changed) => {
    const { deps, events } = harness();
    const run = deps.run;
    deps.run = async (command) => {
      const result = await run(command);
      if (gitStep(command) !== "changed") {
        return result;
      }
      const { truncated = false, ...rest } = changed as typeof changed & { truncated?: boolean };
      return { ...result, ...rest, stdoutTruncated: truncated };
    };
    expect(await runMerge(deps, COMMIT, WHOLE_REPO)).toMatchObject({ outcome: "git_failed" });
    expect(events.some((event) => event.startsWith("package:"))).toBe(false);
  });
});
