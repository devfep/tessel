import type { Captured } from "./container-step";
import { KILL_AFTER_SECONDS } from "./step-budget";
import type { GateConfig } from "./tessel-config";

/** A command of a configured gate: `argv` run in `dir`, relative to the clone. */
export interface PlannedCommand {
  argv: string[];
  dir: string;
}

/** The only flags under which the steward installs: no network, no drift, no scripts. */
export const PNPM_INSTALL_ARGV = [
  "pnpm",
  "install",
  "--offline",
  "--frozen-lockfile",
  "--ignore-scripts",
] as const;
export const CARGO_FETCH_ARGV = ["cargo", "fetch", "--locked", "--offline"] as const;

/**
 * The install step of a configured repo. The steward owns these argv arrays, so a `tessel.toml`
 * chooses where to install, not how: it cannot turn scripts or the network back on.
 */
export function installCommands(config: GateConfig): PlannedCommand[] {
  const commands: PlannedCommand[] = [];
  if (config.installCargo) {
    commands.push({ argv: [...CARGO_FETCH_ARGV], dir: "." });
  }
  for (const dir of config.installPnpm) {
    commands.push({ argv: [...PNPM_INSTALL_ARGV], dir });
  }
  return commands;
}

/** The test step of a configured repo: the `[[test]]` commands of the trunk's `tessel.toml`. */
export function testCommands(config: GateConfig): PlannedCommand[] {
  return config.tests.map(({ argv, dir }) => ({ argv: [...argv], dir }));
}

export const BUDGET_EXHAUSTED_MESSAGE =
  "The step used up its share of the time budget before this command could start";

/** Runs one command; `timeoutSeconds` is what `timeout` gets, before its kill grace. */
export type RunCommand = (command: PlannedCommand, timeoutSeconds: number) => Promise<Captured>;

/**
 * Runs `commands` one after the other inside one time budget. Each gets what is left of
 * `budgetSeconds` minus the kill grace `timeout --kill-after` adds, so the commands together
 * cannot outlast the budget. The first command that exits
 * non-zero ends the sequence and its output is returned; otherwise the last command's is. When
 * nothing is left before a command starts, exit 124 (a timeout) is returned without running it.
 *
 * @param now Milliseconds clock; injected so tests do not wait.
 */
export async function runWithinBudget(
  commands: readonly PlannedCommand[],
  budgetSeconds: number,
  run: RunCommand,
  now: () => number,
): Promise<Captured> {
  const started = now();
  let last: Captured = {
    exitCode: 0,
    stdout: "",
    stderr: "",
    stdoutTruncated: false,
    stderrTruncated: false,
  };
  for (const command of commands) {
    const remaining = Math.floor(budgetSeconds - (now() - started) / 1000) - KILL_AFTER_SECONDS;
    if (remaining < 1) {
      return { ...last, exitCode: 124, stderr: BUDGET_EXHAUSTED_MESSAGE, stderrTruncated: false };
    }
    last = await run(command, remaining);
    if (last.exitCode !== 0) {
      return last;
    }
  }
  return last;
}

const RSS_LINE_PATTERN = /^[0-9]{1,12}$/;

/**
 * Parses the file `/usr/bin/time -f %M -a -o` appends to: one line of kilobytes of maximum
 * resident set per command, which covers the command's child processes. Other lines (GNU time
 * also notes a non-zero exit) and anything repo code wrote are ignored. Returns the largest, in
 * bytes and never more than `capBytes` (the instance's memory), or null when no line is a plain
 * number.
 *
 * The file is written inside the sandbox, and the gate's own code runs as the same uid (and, under
 * the `durable_object` policy, with root's capabilities), so it can forge any line. The cap only
 * keeps a forged value physically possible; the result is a measurement, never a pass or fail
 * input.
 */
export function parsePeakRss(text: string, capBytes: number): number | null {
  let peakKilobytes: number | null = null;
  for (const line of text.split("\n")) {
    if (RSS_LINE_PATTERN.test(line)) {
      peakKilobytes = Math.max(peakKilobytes ?? 0, Number(line));
    }
  }
  return peakKilobytes === null ? null : Math.min(peakKilobytes * 1024, capBytes);
}
