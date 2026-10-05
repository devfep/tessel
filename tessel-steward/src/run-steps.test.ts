import { describe, expect, it } from "vitest";

import { runCloneThenTest, type StepOutcome } from "./run-steps";

function outcome(step: StepOutcome["step"], exitCode: number): StepOutcome {
  return {
    step,
    exitCode,
    stdout: `${step} out`,
    stderr: "",
    passed: step === "test" && exitCode === 0,
  };
}

function harness(exitCodes: { clone: number | Error; test: number }) {
  const calls: string[] = [];
  const runStep = async (step: StepOutcome["step"]): Promise<StepOutcome> => {
    calls.push(`start ${step}`);
    const code = exitCodes[step];
    if (code instanceof Error) {
      throw code;
    }
    return outcome(step, code);
  };
  const revokeToken = async (): Promise<void> => {
    calls.push("revoke");
  };
  return { calls, runStep, revokeToken };
}

describe("runCloneThenTest", () => {
  it("revokes after a good clone and before the test step starts", async () => {
    const { calls, runStep, revokeToken } = harness({ clone: 0, test: 0 });
    const result = await runCloneThenTest(runStep, revokeToken);
    expect(calls).toEqual(["start clone", "revoke", "start test"]);
    expect(result.step).toBe("test");
  });

  it("returns the clone failure, revokes, and never runs the tests", async () => {
    const { calls, runStep, revokeToken } = harness({ clone: 128, test: 0 });
    const result = await runCloneThenTest(runStep, revokeToken);
    expect(calls).toEqual(["start clone", "revoke"]);
    expect(result).toEqual(outcome("clone", 128));
    expect(result.passed).toBe(false);
  });

  it("revokes and propagates the error when the clone step throws", async () => {
    const failure = new Error("exec failed");
    const { calls, runStep, revokeToken } = harness({ clone: failure, test: 0 });
    await expect(runCloneThenTest(runStep, revokeToken)).rejects.toBe(failure);
    expect(calls).toEqual(["start clone", "revoke"]);
  });

  it("reports passed false for a failing test step", async () => {
    const { runStep, revokeToken } = harness({ clone: 0, test: 1 });
    expect(await runCloneThenTest(runStep, revokeToken)).toEqual(outcome("test", 1));
  });

  it("reports passed true for a test step that exits 0", async () => {
    const { runStep, revokeToken } = harness({ clone: 0, test: 0 });
    expect(await runCloneThenTest(runStep, revokeToken)).toEqual(outcome("test", 0));
  });
});
