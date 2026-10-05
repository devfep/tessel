import { describe, expect, it } from "vitest";

import { DEPENDENCIES_DECLARED_EXIT_CODE } from "./dependency-check";
import {
  DEPENDENCIES_UNKNOWN_MESSAGE,
  DEPENDENCIES_UNSUPPORTED_MESSAGE,
  REVOKE_FAILED_MESSAGE,
  makeOutcome,
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

  it("refuses with a different message when the dependency check did not complete", async () => {
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
