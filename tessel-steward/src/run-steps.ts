import type { ConfigIssue } from "./gate-plan";
import { isTimedOut } from "./step-budget";
import type { TailCapture } from "./tail-capture";
import { DEPENDENCIES_DECLARED_EXIT_CODE } from "./dependency-check";

/**
 * Why a step stopped the run before it reached a verdict, when the reason is known to the
 * steward and not to the repo. A fixed value, never repo text.
 * - `dependencies`: the repo declares dependencies and has no `tessel.toml` on main.
 * - `config`: the repo declares dependencies and its `tessel.toml` on main is not acceptable.
 * - `unknown`: the dependency check did not complete.
 * - `install_failed`: an install command of a `tessel.toml` exited non-zero (a changed lockfile,
 *   or a dependency missing from the image).
 * - `timeout`: a step used up its share of the time budget or was killed.
 */
export type StepReason = "dependencies" | "config" | "unknown" | "install_failed" | "timeout";

/** What the steward measured around the test step; `peakMemoryBytes` is null when unreadable. */
export interface Measurement {
  wallMs: number;
  peakMemoryBytes: number | null;
}

export interface StepOutcome {
  step: "clone" | "install" | "test";
  exitCode: number;
  stdout: string;
  stderr: string;
  stdoutTruncated: boolean;
  stderrTruncated: boolean;
  passed: boolean;
  reason?: StepReason;
  measurement?: Measurement;
}

export const REVOKE_FAILED_MESSAGE =
  "The repo token could not be revoked, so no repo code was run in the sandbox";
export const DEPENDENCIES_UNSUPPORTED_MESSAGE =
  "This repo declares dependencies, and dependency installation is not supported by this " +
  "runner yet; the tests were not run";
export const CONFIG_INVALID_MESSAGE =
  "This repo declares dependencies, and its tessel.toml on main is invalid, so they cannot be " +
  "installed; the tests were not run";
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

/**
 * The outcome of the dependency check of a repo without a usable `tessel.toml`, when it did not
 * exit 0: the tests were not run. The message is fixed text, not the check's output.
 */
export function refuseDependencies(check: StepOutcome, issue: ConfigIssue): StepOutcome {
  const declared = check.exitCode === DEPENDENCIES_DECLARED_EXIT_CODE;
  let reason: StepReason = "unknown";
  if (declared) {
    reason = issue === "invalid" ? "config" : "dependencies";
  }
  const messages = {
    dependencies: DEPENDENCIES_UNSUPPORTED_MESSAGE,
    config: CONFIG_INVALID_MESSAGE,
    unknown: DEPENDENCIES_UNKNOWN_MESSAGE,
  };
  return {
    step: "install",
    exitCode: check.exitCode,
    stdout: "",
    stderr: messages[reason],
    stdoutTruncated: false,
    stderrTruncated: false,
    passed: false,
    reason,
  };
}

/**
 * Runs one step, then revokes the token, whether the step returned or threw.
 *
 * The token is therefore never valid once the step ends. If the revocation fails, an error is
 * thrown (a thrown step error takes precedence) so the caller runs nothing further.
 *
 * @param run Runs the step in the sandbox.
 * @param revokeToken Revokes the token(s); must not throw; resolves to false on failure.
 * @returns The step's outcome.
 * @throws If the step throws or the token could not be revoked.
 */
export async function runStepThenRevoke(
  run: () => Promise<StepOutcome>,
  revokeToken: () => Promise<boolean>,
): Promise<StepOutcome> {
  let outcome: StepOutcome;
  let revoked = false;
  try {
    outcome = await run();
  } finally {
    revoked = await revokeToken();
  }
  if (!revoked) {
    throw new Error(REVOKE_FAILED_MESSAGE);
  }
  return outcome;
}

/**
 * Runs the install step, then the test step.
 *
 * The `"install"` step must exit 0, or the tests are not run and its outcome is returned as is:
 * tests that cannot have their dependencies are not test evidence. A test step that used up its
 * share of the time budget or was killed (exit 124 or 137) is also not a test failure: it is
 * returned as an `"install"` outcome with reason `timeout`, which every caller treats as
 * infrastructure.
 *
 * @param runStep Runs one step in the sandbox.
 * @returns The install outcome if it stopped the run, otherwise the test outcome.
 */
export async function runInstallThenTest(
  runStep: (step: "install" | "test") => Promise<StepOutcome>,
): Promise<StepOutcome> {
  const install = await runStep("install");
  if (install.exitCode !== 0) {
    return install;
  }
  const test = await runStep("test");
  if (isTimedOut(test.exitCode)) {
    return { ...test, step: "install", passed: false, reason: "timeout" };
  }
  return test;
}

/**
 * Runs the clone step, then the dependency check, then the test step.
 *
 * The token is revoked as soon as the clone step ends, whether it returned or threw, so it is
 * never valid while the repo's own code runs. If the revocation fails, nothing further runs
 * and an error is thrown (a thrown clone error takes precedence). A failed clone is returned
 * as is. See `runInstallThenTest` for the rest.
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
  const clone = await runStepThenRevoke(() => runStep("clone"), revokeToken);
  if (clone.exitCode !== 0) {
    return clone;
  }
  return runInstallThenTest(runStep);
}
