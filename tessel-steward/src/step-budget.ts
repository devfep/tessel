/**
 * The coordinator waits this long for one merge or one trial side (`STEWARD_CALL_TIMEOUT_MS` in
 * `src/merge.rs`; a test pins the two together). It sits under the 15-minute wall limit of the
 * Durable Object alarm that makes the call.
 */
export const STEWARD_CALL_TIMEOUT_SECONDS = 13 * 60;

/** Upper bound on the quick local git commands of one run (base, verify, merge-base, diff...). */
export const LOCAL_GIT_COMMANDS_MAX = 8;

/** Seconds `timeout --kill-after` waits after the TERM before it sends KILL, on every command. */
export const KILL_AFTER_SECONDS = 5;

/**
 * Time that no step's timeout covers, reserved by name so the sum is honest:
 * - `containerStart`: starting the container and the first exec answering (a 2.5 GB image).
 * - `tokens`: minting and intercepting the read tokens, the write token and its revocation.
 * - `reads`: repo info, the head of main and `tessel.toml` through the Artifacts binding.
 * - `teardown`: destroying the container and revoking what is left.
 * - `measurement`: the peak-memory read after the test step.
 */
export const RESERVED_SECONDS = {
  containerStart: 60,
  tokens: 15,
  reads: 10,
  teardown: 15,
  measurement: 5,
} as const;

/** Spare seconds on top of everything above, so a small overrun is not a timeout. */
export const MARGIN_SECONDS = 30;

const FIXED_STEP_SECONDS = {
  clone: 60,
  fetch: 30,
  local: 6,
  rebase: 30,
  dependencyCheck: 10,
  install: 60,
  push: 30,
} as const;

function reservedSeconds(): number {
  return Object.values(RESERVED_SECONDS).reduce((sum, seconds) => sum + seconds, 0);
}

/** Worst case of every step but the test: each single command also waits out its kill grace. */
function worstCaseWithoutTest(): number {
  const { clone, fetch, rebase, push, local, install } = FIXED_STEP_SECONDS;
  const singles = clone + fetch + rebase + push + 4 * KILL_AFTER_SECONDS;
  return singles + LOCAL_GIT_COMMANDS_MAX * (local + KILL_AFTER_SECONDS) + install;
}

/**
 * Seconds each step may take, one budget and not independent limits. A single command's timeout
 * is its share and the kill grace is added on top (counted in `worstCaseSeconds`); the commands of
 * `install` and `test` share their step's seconds, grace included (`runWithinBudget`).
 * `test` is whatever remains of the call timeout after the other shares, the reserved time and
 * the margin: 327 s today. A step that runs out of its share ends as an infrastructure outcome,
 * never as a failing test (`isTimedOut`). `dependencyCheck` runs inside `install`.
 */
export const STEP_SECONDS = {
  ...FIXED_STEP_SECONDS,
  test: STEWARD_CALL_TIMEOUT_SECONDS - MARGIN_SECONDS - reservedSeconds() - worstCaseWithoutTest(),
} as const;

/** Every step using all of its share, once per run of a merge (the longest path). */
export function worstCaseSeconds(): number {
  return worstCaseWithoutTest() + STEP_SECONDS.test;
}

/** Seconds of the call timeout taken by the reserved items. */
export function totalReservedSeconds(): number {
  return reservedSeconds();
}

/** Exit codes of `timeout` for a command it terminated (124) or killed (137). */
const TIMED_OUT_EXIT_CODES: readonly number[] = [124, 137];

/** Whether an exit code means the step ran out of time or was killed, not that it failed. */
export function isTimedOut(exitCode: number): boolean {
  return TIMED_OUT_EXIT_CODES.includes(exitCode);
}
