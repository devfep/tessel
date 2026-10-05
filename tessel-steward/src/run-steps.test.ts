import { describe, expect, it } from "vitest";

import {
  DEPENDENCIES_DECLARED_EXIT_CODE,
  DEPENDENCIES_UNKNOWN_MESSAGE,
  DEPENDENCIES_UNSUPPORTED_MESSAGE,
  REVOKE_FAILED_MESSAGE,
  runCloneThenTest,
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

  it("refuses a repo with dependencies: install outcome, tests never run", async () => {
    const { calls, runStep, revokeToken } = harness({
      install: DEPENDENCIES_DECLARED_EXIT_CODE,
    });
    const result = await runCloneThenTest(runStep, revokeToken);
    expect(calls).toEqual(["start clone", "revoke", "start install"]);
    expect(result).toEqual({
      step: "install",
      exitCode: DEPENDENCIES_DECLARED_EXIT_CODE,
      stdout: "",
      stderr: DEPENDENCIES_UNSUPPORTED_MESSAGE,
      stdoutTruncated: false,
      stderrTruncated: false,
      passed: false,
    });
  });

  it("refuses with a different message when the dependency check cannot read package.json", async () => {
    const { calls, runStep, revokeToken } = harness({ install: 4 });
    const result = await runCloneThenTest(runStep, revokeToken);
    expect(calls).toEqual(["start clone", "revoke", "start install"]);
    expect(result.step).toBe("install");
    expect(result.exitCode).toBe(4);
    expect(result.stderr).toBe(DEPENDENCIES_UNKNOWN_MESSAGE);
    expect(result.passed).toBe(false);
  });

  it("throws when the revoke fails and runs neither the check nor the tests", async () => {
    const { calls, runStep, revokeToken } = harness({ revoked: false });
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
