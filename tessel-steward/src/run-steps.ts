import type { TailCapture } from "./tail-capture";
import { DEPENDENCIES_DECLARED_EXIT_CODE } from "./dependency-check";

export interface StepOutcome {
  step: "clone" | "install" | "test";
  exitCode: number;
  stdout: string;
  stderr: string;
  stdoutTruncated: boolean;
  stderrTruncated: boolean;
  passed: boolean;
}

export const REVOKE_FAILED_MESSAGE =
  "The repo token could not be revoked, so no repo code was run in the sandbox";
export const DEPENDENCIES_UNSUPPORTED_MESSAGE =
  "This repo declares dependencies, and dependency installation is not supported by this " +
  "runner yet; the tests were not run";
export const DEPENDENCIES_UNKNOWN_MESSAGE =
  "The dependency check did not complete, so it is unknown whether this repo declares " +
  "dependencies; the tests were not run";

/**
 * Builds the outcome of a step that ran. `passed` is true only for step "test" with exit code 0:
 * "clone" and "install" are infrastructure outcomes, never test evidence.
 */
export function makeOutcome(
  step: StepOutcome["step"],
  exitCode: number,
  stdout: TailCapture,
  stderr: TailCapture,
): StepOutcome {
  return {
    step,
    exitCode,
    stdout: stdout.text,
    stderr: stderr.text,
    stdoutTruncated: stdout.truncated,
    stderrTruncated: stderr.truncated,
    passed: step === "test" && exitCode === 0,
  };
}

function refusal(check: StepOutcome): StepOutcome {
  const declared = check.exitCode === DEPENDENCIES_DECLARED_EXIT_CODE;
  return {
    step: "install",
    exitCode: check.exitCode,
    stdout: "",
    stderr: declared ? DEPENDENCIES_UNSUPPORTED_MESSAGE : DEPENDENCIES_UNKNOWN_MESSAGE,
    stdoutTruncated: false,
    stderrTruncated: false,
    passed: false,
  };
}

/**
 * Runs the clone step, then the dependency check, then the test step.
 *
 * The token is revoked as soon as the clone step ends, whether it returned or threw, so it is
 * never valid while the repo's own code runs. If the revocation fails, nothing further runs
 * and an error is thrown (a thrown clone error takes precedence). A failed clone is returned
 * as is. The `"install"` step is the dependency check: if it does not exit 0, the tests are
 * not run and an `"install"` outcome with a fixed message is returned, because tests that
 * cannot have their dependencies are not test evidence.
 *
 * @param runStep Runs one step in the sandbox.
 * @param revokeToken Revokes the repo token; must not throw; resolves to false on failure.
 * @returns The clone or install outcome if one stopped the run, otherwise the test outcome.
 * @throws If a step throws or the token could not be revoked.
 */
export async function runCloneThenTest(
  runStep: (step: StepOutcome["step"]) => Promise<StepOutcome>,
  revokeToken: () => Promise<boolean>,
): Promise<StepOutcome> {
  let clone: StepOutcome;
  let revoked = false;
  try {
    clone = await runStep("clone");
  } finally {
    revoked = await revokeToken();
  }
  if (!revoked) {
    throw new Error(REVOKE_FAILED_MESSAGE);
  }
  if (clone.exitCode !== 0) {
    return clone;
  }
  const check = await runStep("install");
  if (check.exitCode !== 0) {
    return refusal(check);
  }
  return runStep("test");
}
