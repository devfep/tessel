import type { Sha, TrialOutcome, TrialReport, TrialRequest } from "./merge-types";

/** Runs one trial in a sandbox of its own: a new Durable Object instance for every call. */
export type TrialRunner = (main: Sha, commit: Sha | undefined) => Promise<TrialOutcome>;

/**
 * Tries the request's commit on main at `before`, and, only if that was clean, on main at `main`.
 * Each run is its own call to `runner`, so each has its own container, its own read tokens and
 * its own work tree: what the first run leaves behind (files in `/tmp` or `$HOME`, a process still
 * listening) cannot make the second fail. The second run tries the commit the first one tried, so
 * a fork head that moves in between cannot make the two runs differ.
 *
 * When `before` and `main` are the same sha there is one run, and it is both sides.
 *
 * @throws If `runner` throws.
 */
export async function reportTrial(
  request: TrialRequest,
  runner: TrialRunner,
): Promise<TrialReport> {
  const before = await runner(request.before, request.commit);
  if (before.outcome !== "clean") {
    return { before, after: null };
  }
  if (request.before === request.main) {
    return { before, after: before };
  }
  return { before, after: await runner(request.main, before.commit) };
}
