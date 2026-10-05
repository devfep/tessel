/**
 * The coordinator waits this long for one merge or one trial side (`STEWARD_CALL_TIMEOUT_MS` in
 * `src/merge.rs`; a test pins the two together). It sits under the 15-minute wall limit of the
 * Durable Object alarm that makes the call.
 */
export const STEWARD_CALL_TIMEOUT_SECONDS = 13 * 60;

/** Upper bound on the quick local git commands of one run (base, verify, merge-base, diff...). */
export const LOCAL_GIT_COMMANDS_MAX = 8;

/**
 * Seconds each step may take, whatever it is doing. These are one budget, not independent limits:
 * the worst case is every step using all of its share, and `worstCaseSeconds()` plus
 * `MARGIN_SECONDS` must stay under `STEWARD_CALL_TIMEOUT_SECONDS`. A step that runs out of its
 * share ends as an infrastructure outcome, never as a failing test (`isTimedOut`).
 *
 * - `install`: the whole install step of a configured repo, shared by its commands.
 * - `test`: the whole test step, shared by the `[[test]]` commands of a configured repo.
 * - `dependencyCheck`: the check of a repo without `tessel.toml`; it runs inside `install`.
 */
export const STEP_SECONDS = {
  clone: 90,
  fetch: 45,
  local: 8,
  rebase: 45,
  dependencyCheck: 10,
  install: 90,
  test: 300,
  push: 45,
} as const;

/**
 * Room for what has no timeout of its own: container start, token minting and revocation, the
 * reads of main and of `tessel.toml`, container teardown.
 */
export const MARGIN_SECONDS = 90;

/** Every step using all of its share, once per run of a merge (the longest path). */
export function worstCaseSeconds(): number {
  const { clone, fetch, local, rebase, install, test, push } = STEP_SECONDS;
  return clone + fetch + LOCAL_GIT_COMMANDS_MAX * local + rebase + install + test + push;
}

/** Exit codes of `timeout` for a command it terminated (124) or killed (137). */
const TIMED_OUT_EXIT_CODES: readonly number[] = [124, 137];

/** Whether an exit code means the step ran out of time or was killed, not that it failed. */
export function isTimedOut(exitCode: number): boolean {
  return TIMED_OUT_EXIT_CODES.includes(exitCode);
}
