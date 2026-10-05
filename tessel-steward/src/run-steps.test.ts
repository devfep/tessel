import { describe, expect, it } from "vitest";

import { DEPENDENCIES_DECLARED_EXIT_CODE } from "./dependency-check";
import {
  CONFIG_INVALID_MESSAGE,
  DEPENDENCIES_UNKNOWN_MESSAGE,
  DEPENDENCIES_UNSUPPORTED_MESSAGE,
  REVOKE_FAILED_MESSAGE,
  CLONE_MOVED_MESSAGE,
  cloneAtCommit,
  invalidConfigOutcome,
  makeOutcome,
  refuseDependencies,
  runCloneThenTest,
  runInstallThenTest,
  runStepThenRevoke,
  type StepOutcome,
} from "./run-steps";

function outcome(step: StepOutcome["step"], exitCode: number): StepOutcome {
  return {
    step,
    exitCode,
    stdout: `${step} out`,
    stderr: "",
    stdoutTruncated: false,
    stderrTruncated: false,
    passed: step === "test" && exitCode === 0,
  };
}

interface Plan {
  clone: number | Error;
  install: number;
  test: number;
  revoked: boolean;
}

function harness(overrides: Partial<Plan> = {}) {
  const plan: Plan = { clone: 0, install: 0, test: 0, revoked: true, ...overrides };
  const calls: string[] = [];
  const runStep = async (step: StepOutcome["step"]): Promise<StepOutcome> => {
    calls.push(`start ${step}`);
    const code = plan[step];
    if (code instanceof Error) {
      throw code;
    }
    return outcome(step, code);
  };
  const revokeToken = async (): Promise<boolean> => {
    calls.push("revoke");
    return plan.revoked;
  };
  return { calls, runStep, revokeToken };
}

describe("runCloneThenTest", () => {
  it("revokes after a good clone, checks dependencies, then runs the tests", async () => {
    const { calls, runStep, revokeToken } = harness();
    const result = await runCloneThenTest(runStep, revokeToken);
    expect(calls).toEqual(["start clone", "revoke", "start install", "start test"]);
    expect(result.step).toBe("test");
  });

  it("returns a failed install step as it is and never runs the tests", async () => {
    const { calls, runStep, revokeToken } = harness({ install: 1 });
    const result = await runCloneThenTest(runStep, revokeToken);
    expect(calls).toEqual(["start clone", "revoke", "start install"]);
    expect(result).toEqual(outcome("install", 1));
  });

  it("marks a test step that timed out or was killed as a timeout and keeps it the test step", async () => {
    for (const test of [124, 137]) {
      const { runStep, revokeToken } = harness({ test });
      const result = await runCloneThenTest(runStep, revokeToken);
      expect(result).toMatchObject({
        step: "test",
        exitCode: test,
        stdout: "test out",
        passed: false,
        reason: "timeout",
      });
    }
  });

  it("marks an install step that timed out or was killed as a timeout and runs no tests", async () => {
    for (const install of [124, 137]) {
      const { calls, runStep, revokeToken } = harness({ install });
      const result = await runCloneThenTest(runStep, revokeToken);
      expect(calls).toEqual(["start clone", "revoke", "start install"]);
      expect(result).toMatchObject({ step: "install", exitCode: install, reason: "timeout" });
    }
  });

  it("keeps an ordinary non-zero test exit a test failure", async () => {
    const { runStep, revokeToken } = harness({ test: 1 });
    const result = await runCloneThenTest(runStep, revokeToken);
    expect(result).toMatchObject({ step: "test", exitCode: 1, passed: false });
    expect(result.reason).toBeUndefined();
  });

  it("throws when the revoke fails and runs neither the check nor the tests", async () => {
    const { calls, runStep, revokeToken } = harness({ revoked: false });
    await expect(runCloneThenTest(runStep, revokeToken)).rejects.toThrow(REVOKE_FAILED_MESSAGE);
    expect(calls).toEqual(["start clone", "revoke"]);
  });

  it("throws the revoke error, not the clone failure, when both fail", async () => {
    const { calls, runStep, revokeToken } = harness({ clone: 128, revoked: false });
    await expect(runCloneThenTest(runStep, revokeToken)).rejects.toThrow(REVOKE_FAILED_MESSAGE);
    expect(calls).toEqual(["start clone", "revoke"]);
  });

  it("returns the clone failure, revokes, and runs nothing else", async () => {
    const { calls, runStep, revokeToken } = harness({ clone: 128 });
    const result = await runCloneThenTest(runStep, revokeToken);
    expect(calls).toEqual(["start clone", "revoke"]);
    expect(result).toEqual(outcome("clone", 128));
    expect(result.passed).toBe(false);
  });

  it("revokes and propagates the error when the clone step throws", async () => {
    const failure = new Error("exec failed");
    const { calls, runStep, revokeToken } = harness({ clone: failure });
    await expect(runCloneThenTest(runStep, revokeToken)).rejects.toBe(failure);
    expect(calls).toEqual(["start clone", "revoke"]);
  });

  it("propagates the clone error, not the revoke error, when both fail", async () => {
    const failure = new Error("exec failed");
    const { runStep, revokeToken } = harness({ clone: failure, revoked: false });
    await expect(runCloneThenTest(runStep, revokeToken)).rejects.toBe(failure);
  });

  it("reports passed false for a failing test step", async () => {
    const { runStep, revokeToken } = harness({ test: 1 });
    expect(await runCloneThenTest(runStep, revokeToken)).toEqual(outcome("test", 1));
  });

  it("reports passed true for a test step that exits 0", async () => {
    const { runStep, revokeToken } = harness();
    expect(await runCloneThenTest(runStep, revokeToken)).toEqual(outcome("test", 0));
  });
});

describe("makeOutcome", () => {
  const empty = { text: "", truncated: false };
  const steps: StepOutcome["step"][] = ["clone", "install", "test"];

  it.each(steps)("sets passed only for step test with exit code 0 (%s)", (step) => {
    expect(makeOutcome(step, 0, empty, empty).passed).toBe(step === "test");
    expect(makeOutcome(step, 1, empty, empty).passed).toBe(false);
  });

  it("copies the captured text and truncation flags of each stream", () => {
    const stdout = { text: "out", truncated: true };
    const stderr = { text: "err", truncated: false };
    expect(makeOutcome("test", 2, stdout, stderr)).toEqual({
      step: "test",
      exitCode: 2,
      stdout: "out",
      stderr: "err",
      stdoutTruncated: true,
      stderrTruncated: false,
      passed: false,
    });
  });
});

describe("runStepThenRevoke", () => {
  it("revokes after the step and returns its outcome", async () => {
    const { calls, revokeToken } = harness();
    const result = await runStepThenRevoke(async () => {
      calls.push("start step");
      return outcome("clone", 0);
    }, revokeToken);
    expect(calls).toEqual(["start step", "revoke"]);
    expect(result.step).toBe("clone");
  });

  it("revokes when the step throws, and the step's error wins", async () => {
    const { calls, revokeToken } = harness({ revoked: false });
    await expect(
      runStepThenRevoke(async () => {
        throw new Error("exec failed");
      }, revokeToken),
    ).rejects.toThrow("exec failed");
    expect(calls).toEqual(["revoke"]);
  });

  it("throws when the revocation fails, even though the step succeeded", async () => {
    const { revokeToken } = harness({ revoked: false });
    await expect(runStepThenRevoke(async () => outcome("clone", 0), revokeToken)).rejects.toThrow(
      REVOKE_FAILED_MESSAGE,
    );
  });
});

describe("runInstallThenTest", () => {
  it("runs the tests only after a clean dependency check", async () => {
    const { calls, runStep } = harness();
    const result = await runInstallThenTest(runStep);
    expect(calls).toEqual(["start install", "start test"]);
    expect(result.passed).toBe(true);
  });

  it("stops at the dependency check with an install outcome", async () => {
    const { calls, runStep } = harness({ install: DEPENDENCIES_DECLARED_EXIT_CODE });
    const result = await runInstallThenTest(runStep);
    expect(calls).toEqual(["start install"]);
    expect(result).toMatchObject({ step: "install", passed: false });
  });
});

const check = (exitCode: number) => outcome("install", exitCode);

describe("refuseDependencies", () => {
  it("refuses a repo with dependencies and no tessel.toml with a fixed message", () => {
    expect(refuseDependencies(check(DEPENDENCIES_DECLARED_EXIT_CODE))).toEqual({
      step: "install",
      exitCode: DEPENDENCIES_DECLARED_EXIT_CODE,
      stdout: "",
      stderr: DEPENDENCIES_UNSUPPORTED_MESSAGE,
      stdoutTruncated: false,
      stderrTruncated: false,
      passed: false,
      reason: "dependencies",
    });
  });

  it("fails an invalid tessel.toml at install with reason config, running nothing", () => {
    expect(invalidConfigOutcome()).toEqual({
      step: "install",
      exitCode: 1,
      stdout: "",
      stderr: CONFIG_INVALID_MESSAGE,
      stdoutTruncated: false,
      stderrTruncated: false,
      passed: false,
      reason: "config",
    });
  });

  it("refuses with a different message when the dependency check did not complete", () => {
    expect(refuseDependencies(check(4))).toMatchObject({
      exitCode: 4,
      stderr: DEPENDENCIES_UNKNOWN_MESSAGE,
      passed: false,
      reason: "unknown",
    });
  });
});

const head = (exitCode: number, stdout: string) => async (): Promise<StepOutcome> => ({
  ...outcome("clone", exitCode),
  stdout,
});

describe("cloneAtCommit", () => {
  const WANTED = "a".repeat(40);

  it("keeps the clone when HEAD is the commit whose gate was read", async () => {
    const cloned = outcome("clone", 0);
    expect(await cloneAtCommit(async () => cloned, head(0, `${WANTED}\n`), WANTED)).toBe(cloned);
  });

  it("fails the clone step, with a fixed message, when the branch moved", async () => {
    const result = await cloneAtCommit(
      async () => outcome("clone", 0),
      head(0, `${"b".repeat(40)}\n`),
      WANTED,
    );
    expect(result).toMatchObject({ step: "clone", passed: false, stderr: CLONE_MOVED_MESSAGE });
    expect(result.exitCode).not.toBe(0);
  });

  it("fails the clone step when HEAD cannot be read", async () => {
    const result = await cloneAtCommit(async () => outcome("clone", 0), head(128, WANTED), WANTED);
    expect(result).toMatchObject({ step: "clone", stderr: CLONE_MOVED_MESSAGE });
  });

  it("returns a failed clone as it is, without reading HEAD", async () => {
    let read = false;
    const failed = outcome("clone", 128);
    const result = await cloneAtCommit(
      async () => failed,
      async () => {
        read = true;
        return outcome("clone", 0);
      },
      WANTED,
    );
    expect(result).toBe(failed);
    expect(read).toBe(false);
  });
});
