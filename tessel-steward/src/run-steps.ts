export interface StepOutcome {
  step: "clone" | "test";
  exitCode: number;
  stdout: string;
  stderr: string;
  passed: boolean;
}

/**
 * Runs the clone step, then the test step if the clone exited 0.
 *
 * The token is revoked as soon as the clone step ends, whether it returned or threw, so it is
 * never valid while the repo's own code runs. A failed clone is returned as is and the test
 * step does not run; a thrown clone error propagates.
 *
 * @param runStep Runs one step in the sandbox.
 * @param revokeToken Revokes the repo token; must not throw.
 * @returns The clone outcome if it failed, otherwise the test outcome.
 */
export async function runCloneThenTest(
  runStep: (step: StepOutcome["step"]) => Promise<StepOutcome>,
  revokeToken: () => Promise<void>,
): Promise<StepOutcome> {
  let clone: StepOutcome;
  try {
    clone = await runStep("clone");
  } finally {
    await revokeToken();
  }
  if (clone.exitCode !== 0) {
    return clone;
  }
  return runStep("test");
}
