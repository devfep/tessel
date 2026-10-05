import { DEPENDENCY_CHECK_SCRIPT } from "./dependency-check";
import type { GatePlan } from "./gate-plan";
import {
  installCommands,
  parsePeakMemory,
  runWithinBudget,
  testCommands,
  type PlannedCommand,
} from "./gate-steps";
import { makeOutcome, refuseDependencies, type StepOutcome } from "./run-steps";
import { STEP_SECONDS, isTimedOut } from "./step-budget";
import { captureTail } from "./tail-capture";
import type { GateConfig } from "./tessel-config";

export const WORKSPACE = "/workspace";
export const CONTAINER_CA_CERTIFICATE = "/etc/cloudflare/certs/cloudflare-containers-ca.crt";

const MEMORY_PEAK_FILE = "/sys/fs/cgroup/memory.peak";
const MEMORY_PEAK_TIMEOUT_SECONDS = "5";
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

/**
 * The environment of the toolchain image (`toolchain.Dockerfile`), repeated for every command of
 * a configured gate so that it does not depend on how `exec` treats the image's own. A test pins
 * these values to the Dockerfile.
 */
export const TOOLCHAIN_ENV: Record<string, string> = {
  PATH: "/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
  HOME: "/home/node",
  CARGO_HOME: "/usr/local/cargo",
  RUSTUP_HOME: "/usr/local/rustup",
  CARGO_TARGET_DIR: "/opt/cargo-target",
  CARGO_INCREMENTAL: "0",
  CARGO_PROFILE_DEV_DEBUG: "0",
  npm_config_store_dir: "/opt/pnpm-store",
};

/**
 * The user every command of a configured gate runs as, git included. It is not root: some tests
 * rely on file permissions, which root ignores, and repo code should not hold more rights than
 * it needs. The image owns `/workspace`, the cargo home, the target directory and the pnpm store
 * for this user.
 */
export const TOOLCHAIN_USER = "node";

/** The `exec` options that select the user of `plan`: none for a legacy plan, which is root. */
export function userOptions(plan: GatePlan): { user?: string } {
  return plan.kind === "configured" ? { user: TOOLCHAIN_USER } : {};
}

function directory(dir: string): string {
  return dir === "." ? WORKSPACE : `${WORKSPACE}/${dir}`;
}

function stepFromCaptured(step: StepOutcome["step"], captured: Captured): StepOutcome {
  return makeOutcome(
    step,
    captured.exitCode,
    { text: captured.stdout, truncated: captured.stdoutTruncated },
    { text: captured.stderr, truncated: captured.stderrTruncated },
  );
}

/**
 * Peak memory of the whole container since it started, from the cgroup. Best effort: null when
 * the file is missing or unreadable, because a measurement must never fail a run.
 */
async function readPeakMemory(container: Container): Promise<number | null> {
  try {
    const read = await execCaptured(
      container,
      "memory",
      MEMORY_PEAK_TIMEOUT_SECONDS,
      ["cat", MEMORY_PEAK_FILE],
      {},
    );
    return read.exitCode === 0 ? parsePeakMemory(read.stdout) : null;
  } catch (error) {
    console.warn(
      JSON.stringify({
        event: "memory_peak_unreadable",
        reason: error instanceof Error ? error.message : String(error),
      }),
    );
    return null;
  }
}

/** Adds the wall time of `run` and the container's peak memory to the outcome it returns. */
async function measured(
  container: Container,
  run: () => Promise<StepOutcome>,
): Promise<StepOutcome> {
  const started = Date.now();
  const outcome = await run();
  const wallMs = Date.now() - started;
  return { ...outcome, measurement: { wallMs, peakMemoryBytes: await readPeakMemory(container) } };
}

function runConfigured(
  container: Container,
  step: "install" | "test",
  commands: PlannedCommand[],
  budgetSeconds: number,
): Promise<Captured> {
  return runWithinBudget(
    commands,
    budgetSeconds,
    (command, timeoutSeconds) =>
      execCaptured(container, step, String(timeoutSeconds), command.argv, {
        cwd: directory(command.dir),
        env: TOOLCHAIN_ENV,
        user: TOOLCHAIN_USER,
      }),
    Date.now,
  );
}

async function runConfiguredStep(
  container: Container,
  config: GateConfig,
  step: "install" | "test",
): Promise<StepOutcome> {
  if (step === "test") {
    return measured(container, async () =>
      stepFromCaptured(
        "test",
        await runConfigured(container, "test", testCommands(config), STEP_SECONDS.test),
      ),
    );
  }
  const captured = await runConfigured(
    container,
    "install",
    installCommands(config),
    STEP_SECONDS.install,
  );
  const outcome = stepFromCaptured("install", captured);
  if (captured.exitCode === 0) {
    return outcome;
  }
  return { ...outcome, reason: isTimedOut(captured.exitCode) ? "timeout" : "install_failed" };
}

async function runLegacyStep(
  container: Container,
  issue: "missing" | "invalid",
  step: "install" | "test",
): Promise<StepOutcome> {
  if (step === "test") {
    return measured(container, () =>
      runStep(container, "test", String(STEP_SECONDS.test), ["npm", "test"], {
        cwd: WORKSPACE,
      }),
    );
  }
  const check = await runStep(
    container,
    "install",
    String(STEP_SECONDS.dependencyCheck),
    ["node", "-e", DEPENDENCY_CHECK_SCRIPT],
    { cwd: WORKSPACE },
  );
  return check.exitCode === 0 ? check : refuseDependencies(check, issue);
}

/**
 * Runs the install step or the test step of `plan` in the cloned workspace. A configured plan
 * runs the commands of the trunk's `tessel.toml`; a legacy plan runs the dependency check and
 * `npm test`. The test step carries its wall time and the container's peak memory.
 */
export function runPackageStep(
  container: Container,
  plan: GatePlan,
  step: "install" | "test",
): Promise<StepOutcome> {
  switch (plan.kind) {
    case "configured":
      return runConfiguredStep(container, plan.config, step);
    case "legacy":
      return runLegacyStep(container, plan.issue, step);
  }
}
