import { DEPENDENCY_CHECK_SCRIPT } from "./dependency-check";
import { makeOutcome, type StepOutcome } from "./run-steps";
import { captureTail } from "./tail-capture";

export const WORKSPACE = "/workspace";
export const CONTAINER_CA_CERTIFICATE = "/etc/cloudflare/certs/cloudflare-containers-ca.crt";

const TEST_TIMEOUT_SECONDS = "600";
const DEPENDENCY_CHECK_TIMEOUT_SECONDS = "30";
const OUTPUT_LIMIT_BYTES = 256 * 1024;

/** Exit code and capped output of one command; the output is untrusted data. */
export interface Captured {
  exitCode: number;
  stdout: string;
  stderr: string;
  stdoutTruncated: boolean;
  stderrTruncated: boolean;
}

/**
 * Runs `argv` in the container under `timeout`, keeping the last 256 KiB of each stream.
 *
 * @param label Names the command in the error thrown when the container gives no output streams.
 * @param timeoutSeconds Seconds before the command is terminated (exit 124) and then killed (137).
 */
export async function execCaptured(
  container: Container,
  label: string,
  timeoutSeconds: string,
  argv: string[],
  options: ContainerExecOptions,
): Promise<Captured> {
  const process = await container.exec(
    ["timeout", "--kill-after=5", timeoutSeconds, ...argv],
    options,
  );
  const { stdout, stderr } = process;
  if (stdout === null || stderr === null) {
    throw new Error(`The ${label} step has no output streams`);
  }
  const [out, err, exitCode] = await Promise.all([
    captureTail(stdout, OUTPUT_LIMIT_BYTES),
    captureTail(stderr, OUTPUT_LIMIT_BYTES),
    process.exitCode,
  ]);
  return {
    exitCode,
    stdout: out.text,
    stderr: err.text,
    stdoutTruncated: out.truncated,
    stderrTruncated: err.truncated,
  };
}

export async function runStep(
  container: Container,
  step: StepOutcome["step"],
  timeoutSeconds: string,
  argv: string[],
  options: ContainerExecOptions,
): Promise<StepOutcome> {
  const captured = await execCaptured(container, step, timeoutSeconds, argv, options);
  return makeOutcome(
    step,
    captured.exitCode,
    { text: captured.stdout, truncated: captured.stdoutTruncated },
    { text: captured.stderr, truncated: captured.stderrTruncated },
  );
}

/** Runs the dependency check or the repo's tests in the cloned workspace. */
export function runPackageStep(
  container: Container,
  step: "install" | "test",
): Promise<StepOutcome> {
  switch (step) {
    case "install":
      return runStep(
        container,
        "install",
        DEPENDENCY_CHECK_TIMEOUT_SECONDS,
        ["node", "-e", DEPENDENCY_CHECK_SCRIPT],
        { cwd: WORKSPACE },
      );
    case "test":
      return runStep(container, "test", TEST_TIMEOUT_SECONDS, ["npm", "test"], {
        cwd: WORKSPACE,
      });
  }
}
