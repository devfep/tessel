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
const plan = { before: BEFORE, main: MAIN, commit: COMMIT };

function clean(base: Sha): TrialOutcome {
  return { outcome: "clean", base, head: sha("9"), commit: COMMIT };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("reportTrial", () => {
  it("runs both sides with the one commit it was given", async () => {
    const runner = vi.fn(async (main: Sha) => clean(main));
    const report = await reportTrial(plan, runner);
    expect(runner.mock.calls).toEqual([
      [BEFORE, COMMIT],
      [MAIN, COMMIT],
    ]);
    expect(report).toEqual({ before: clean(BEFORE), after: clean(MAIN) });
  });

  it("starts both sides before either has finished", async () => {
    const gates = new Map<Sha, ReturnType<typeof deferred<TrialOutcome>>>([
      [BEFORE, deferred()],
      [MAIN, deferred()],
    ]);
    const started: Sha[] = [];
    const pending = reportTrial(plan, (main) => {
      started.push(main);
      return gates.get(main)!.promise;
    });
    await Promise.resolve();
    expect(started).toEqual([BEFORE, MAIN]);
    gates.get(MAIN)?.resolve(clean(MAIN));
    gates.get(BEFORE)?.resolve(clean(BEFORE));
    expect(await pending).toEqual({ before: clean(BEFORE), after: clean(MAIN) });
  });

  it("discards main's result when the baseline is not clean, even if main passed", async () => {
    const failing: TrialOutcome[] = [
      { outcome: "commit_not_in_fork" },
      { outcome: "main_unreachable", main: BEFORE },
      { outcome: "nothing_to_test", base: BEFORE, commit: COMMIT },
      { outcome: "conflict", base: BEFORE, commit: COMMIT, files: ["a"] },
    ];
    for (const before of failing) {
      const runner = vi.fn(async (main: Sha) => (main === BEFORE ? before : clean(main)));
      expect(await reportTrial(plan, runner)).toEqual({ before, after: null });
      expect(runner).toHaveBeenCalledTimes(2);
    }
  });

  it("is Inconclusive without rethrowing when the baseline is not clean and main's run throws", async () => {
    const before: TrialOutcome = {
      outcome: "tests_failed",
      base: BEFORE,
      head: sha("9"),
      commit: COMMIT,
      result: {
        step: "test",
        exitCode: 1,
        stdout: "",
        stderr: "",
        stdoutTruncated: false,
        stderrTruncated: false,
        passed: false,
      },
    };
    const runner = async (main: Sha): Promise<TrialOutcome> => {
      if (main === BEFORE) {
        return before;
      }
      throw new Error("container died");
    };
    expect(await reportTrial(plan, runner)).toEqual({ before, after: null });
  });

  it("makes one run, which is both sides, when the baseline is main itself", async () => {
    const runner = vi.fn(async (main: Sha) => clean(main));
    const report = await reportTrial({ ...plan, before: MAIN }, runner);
    expect(runner).toHaveBeenCalledTimes(1);
    expect(report.after).toBe(report.before);
  });

  it("reports what main's run says, whatever it is", async () => {
    const after: TrialOutcome = { outcome: "commit_not_in_fork" };
    const runner = vi.fn(async (main: Sha) => (main === BEFORE ? clean(main) : after));
    expect(await reportTrial(plan, runner)).toEqual({ before: clean(BEFORE), after });
  });

  it("throws the first error only after both runs have finished", async () => {
    const slow = deferred<TrialOutcome>();
    const runner = (main: Sha) =>
      main === BEFORE ? Promise.reject(new Error("container died")) : slow.promise;
    let settled = false;
    const pending = reportTrial(plan, runner).finally(() => {
      settled = true;
    });
    pending.catch(() => undefined);
    await new Promise((resolve) => setTimeout(resolve, 5));
    expect(settled).toBe(false);
    slow.resolve(clean(MAIN));
    await expect(pending).rejects.toThrow("container died");
  });
});
