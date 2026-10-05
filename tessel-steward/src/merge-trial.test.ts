import { describe, expect, it } from "vitest";

import type { GitCommand } from "./merge-commands";
import { runTrial, type TrialDeps } from "./merge-steps";
import { parseSha, type GitResult, type Sha } from "./merge-types";
import { REVOKE_FAILED_MESSAGE, makeOutcome, type StepOutcome } from "./run-steps";

function sha(character: string): Sha {
  const parsed = parseSha(character.repeat(40));
  if (parsed === undefined) {
    throw new Error("bad test sha");
  }
  return parsed;
}

const MAIN = sha("6");
const MERGE_BASE = sha("2");
const COMMIT = sha("3");
const HEAD = sha("4");

type GitStep =
  | "clone"
  | "fetch"
  | "onmain"
  | "exists"
  | "reachable"
  | "mergebase"
  | "rebase"
  | "conflicts"
  | "head"
  | "other";

function gitStep(command: GitCommand): GitStep {
  const text = command.argv.join(" ");
  const matches: Array<[string, GitStep]> = [
    [" clone ", "clone"],
    [" fetch ", "fetch"],
    ["refs/remotes/origin/main", "onmain"],
    ["--quiet", "exists"],
    ["--is-ancestor", "reachable"],
    [" merge-base ", "mergebase"],
    [" rebase ", "rebase"],
    ["--diff-filter=U", "conflicts"],
    ["HEAD^{commit}", "head"],
  ];
  return matches.find(([needle]) => text.includes(needle))?.[1] ?? "other";
}

interface Plan {
  git: Partial<Record<GitStep, { exitCode: number; stdout?: string }>>;
  install: number;
  test: number;
  revokedReads: boolean;
}

function harness(overrides: Partial<Plan> = {}) {
  const plan: Plan = { git: {}, install: 0, test: 0, revokedReads: true, ...overrides };
  const defaults: Record<GitStep, { exitCode: number; stdout?: string }> = {
    clone: { exitCode: 0 },
    fetch: { exitCode: 0 },
    onmain: { exitCode: 0 },
    exists: { exitCode: 0, stdout: `${COMMIT}\n` },
    reachable: { exitCode: 0 },
    mergebase: { exitCode: 0, stdout: `${MERGE_BASE}\n` },
    rebase: { exitCode: 0 },
    conflicts: { exitCode: 0, stdout: "" },
    head: { exitCode: 0, stdout: `${HEAD}\n` },
    other: { exitCode: 0 },
  };
  const events: string[] = [];
  const commands: GitCommand[] = [];
  const deps: TrialDeps = {
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
      return { exitCode, stdout, stderr: "", stdoutTruncated: false, stderrTruncated: false };
    },
    revokeReadTokens: async () => {
      events.push("revoke-reads");
      return plan.revokedReads;
    },
    runPackageStep: async (step): Promise<StepOutcome> => {
      events.push(`package:${step}`);
      return makeOutcome(
        step,
        plan[step],
        { text: `${step} out`, truncated: false },
        { text: "", truncated: false },
      );
    },
  };
  return { deps, events, commands };
}

describe("runTrial", () => {
  it("fetches, revokes the reads, pins main, verifies, rebases onto the given main, then tests", async () => {
    const { deps, events } = harness();
    const outcome = await runTrial(deps, MAIN, COMMIT);
    expect(outcome).toEqual({ outcome: "clean", base: MAIN, head: HEAD, commit: COMMIT });
    expect(events).toEqual([
      "git:clone",
      "git:fetch",
      "revoke-reads",
      "git:onmain",
      "git:exists",
      "git:reachable",
      "git:mergebase",
      "git:rebase",
      "git:head",
      "package:install",
      "package:test",
    ]);
  });

  it("rebases the commit onto the main it was given, not onto the main it cloned", async () => {
    const { deps, commands } = harness();
    await runTrial(deps, MAIN, COMMIT);
    const rebase = commands.find((command) => gitStep(command) === "rebase");
    expect(rebase?.argv.slice(-4)).toEqual(["--onto", MAIN, MERGE_BASE, COMMIT]);
    const pin = commands.find((command) => gitStep(command) === "onmain");
    expect(pin?.argv).toContain(MAIN);
  });

  it("never runs a push, whatever the tests say", async () => {
    for (const test of [0, 1, 124]) {
      const { deps, commands } = harness({ test });
      await runTrial(deps, MAIN, COMMIT);
      const verbs = commands.flatMap((command) => command.argv);
      expect(verbs).not.toContain("push");
      expect(verbs.some((arg) => arg.startsWith("--force-with-lease"))).toBe(false);
    }
  });

  it("has no way to push or to mint a write token in its dependencies", () => {
    const { deps } = harness();
    expect(Object.keys(deps).toSorted()).toEqual([
      "revokeReadTokens",
      "run",
      "runPackageStep",
      "sources",
    ]);
  });

  it("is clean when the rebased commit passes the tests", async () => {
    const { deps } = harness();
    expect(await runTrial(deps, MAIN, COMMIT)).toMatchObject({ outcome: "clean" });
  });

  it("reports failing tests with the capped output, and 124 as a test failure of the step", async () => {
    const { deps } = harness({ test: 124 });
    expect(await runTrial(deps, MAIN, COMMIT)).toMatchObject({
      outcome: "tests_failed",
      base: MAIN,
      head: HEAD,
      commit: COMMIT,
      result: { step: "test", exitCode: 124, stdout: "test out", passed: false },
    });
  });

  it("reports a stopped rebase as a conflict with the unmerged paths, and runs no tests", async () => {
    const { deps, events } = harness({
      git: { rebase: { exitCode: 1 }, conflicts: { exitCode: 0, stdout: "src/a.ts\0b.md\0" } },
    });
    expect(await runTrial(deps, MAIN, COMMIT)).toEqual({
      outcome: "conflict",
      base: MAIN,
      commit: COMMIT,
      files: ["src/a.ts", "b.md"],
    });
    expect(events.some((event) => event.startsWith("package:"))).toBe(false);
  });

  it("reports a commit that is not on the fork as commit_not_in_fork, and runs nothing after", async () => {
    for (const git of [{ exists: { exitCode: 1 } }, { reachable: { exitCode: 1 } }]) {
      const { deps, events } = harness({ git });
      expect(await runTrial(deps, MAIN, COMMIT)).toEqual({ outcome: "commit_not_in_fork" });
      expect(events).not.toContain("git:rebase");
      expect(events.some((event) => event.startsWith("package:"))).toBe(false);
    }
  });

  it("reports nothing_to_test when the replay leaves main unchanged, instead of testing main", async () => {
    const { deps, events } = harness({ git: { head: { exitCode: 0, stdout: `${MAIN}\n` } } });
    expect(await runTrial(deps, MAIN, COMMIT)).toEqual({
      outcome: "nothing_to_test",
      base: MAIN,
      commit: COMMIT,
    });
    expect(events.some((event) => event.startsWith("package:"))).toBe(false);
  });

  it("reports a dependency refusal as install, not as a test failure", async () => {
    const { deps } = harness({ install: 10 });
    const outcome = await runTrial(deps, MAIN, COMMIT);
    expect(outcome).toMatchObject({ outcome: "install", base: MAIN, head: HEAD });
  });

  it("reports a failed clone or fetch as clone", async () => {
    for (const step of ["clone", "fetch"] as const) {
      const { deps, events } = harness({ git: { [step]: { exitCode: 128 } } });
      expect(await runTrial(deps, MAIN, COMMIT)).toMatchObject({ outcome: "clone" });
      expect(events).not.toContain("git:onmain");
    }
  });

  it("reports a main that is not on main's history as main_unreachable, and tries nothing", async () => {
    for (const exitCode of [1, 128]) {
      const { deps, events } = harness({ git: { onmain: { exitCode } } });
      expect(await runTrial(deps, MAIN, COMMIT)).toEqual({
        outcome: "main_unreachable",
        main: MAIN,
      });
      expect(events).not.toContain("git:rebase");
    }
  });

  it("reports a git failure that is not a conflict as git_failed", async () => {
    const { deps } = harness({ git: { rebase: { exitCode: 1 }, conflicts: { exitCode: 0 } } });
    expect(await runTrial(deps, MAIN, COMMIT)).toMatchObject({ outcome: "git_failed" });
  });

  it("runs nothing from the repo when a read token cannot be revoked", async () => {
    const { deps, events } = harness({ revokedReads: false });
    await expect(runTrial(deps, MAIN, COMMIT)).rejects.toThrow(REVOKE_FAILED_MESSAGE);
    expect(events).not.toContain("git:onmain");
    expect(events.some((event) => event.startsWith("package:"))).toBe(false);
  });
});
