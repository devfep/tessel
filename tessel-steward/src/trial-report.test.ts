import { describe, expect, it, vi } from "vitest";

import { parseSha, type Sha, type TrialOutcome } from "./merge-types";
import { reportTrial } from "./trial-report";

function sha(character: string): Sha {
  const parsed = parseSha(character.repeat(40));
  if (parsed === undefined) {
    throw new Error("bad test sha");
  }
  return parsed;
}

const BEFORE = sha("1");
const MAIN = sha("2");
const COMMIT = sha("3");
const FORK_HEAD = sha("4");
const request = { fork: "demo--a1", before: BEFORE, main: MAIN };

function clean(base: Sha, commit: Sha = COMMIT): TrialOutcome {
  return { outcome: "clean", base, head: sha("9"), commit };
}

describe("reportTrial", () => {
  it("runs the baseline first and the new main second, each as a run of its own", async () => {
    const runner = vi.fn(async (main: Sha) => clean(main));
    const report = await reportTrial({ ...request, commit: COMMIT }, runner);
    expect(runner.mock.calls).toEqual([
      [BEFORE, COMMIT],
      [MAIN, COMMIT],
    ]);
    expect(report).toEqual({ before: clean(BEFORE), after: clean(MAIN) });
  });

  it("tries on main the commit the baseline run resolved, not the fork head of that moment", async () => {
    const runner = vi.fn(async (main: Sha) => clean(main, FORK_HEAD));
    await reportTrial(request, runner);
    expect(runner.mock.calls).toEqual([
      [BEFORE, undefined],
      [MAIN, FORK_HEAD],
    ]);
  });

  it("does not run main when the baseline is not clean", async () => {
    const failing: TrialOutcome[] = [
      { outcome: "commit_not_in_fork" },
      { outcome: "main_unreachable", main: BEFORE },
      { outcome: "nothing_to_test", base: BEFORE, commit: COMMIT },
      { outcome: "conflict", base: BEFORE, commit: COMMIT, files: ["a"] },
    ];
    for (const before of failing) {
      const runner = vi.fn(async () => before);
      expect(await reportTrial(request, runner)).toEqual({ before, after: null });
      expect(runner).toHaveBeenCalledTimes(1);
    }
  });

  it("makes one run, which is both sides, when the baseline is main itself", async () => {
    const runner = vi.fn(async (main: Sha) => clean(main));
    const report = await reportTrial({ ...request, before: MAIN }, runner);
    expect(runner).toHaveBeenCalledTimes(1);
    expect(report.after).toBe(report.before);
  });

  it("reports what main's run says, whatever it is", async () => {
    const after: TrialOutcome = { outcome: "commit_not_in_fork" };
    const runner = vi.fn(async (main: Sha) => (main === BEFORE ? clean(main) : after));
    expect(await reportTrial(request, runner)).toEqual({ before: clean(BEFORE), after });
  });
});
