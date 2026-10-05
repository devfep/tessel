import type { Sha, TrialOutcome, TrialReport } from "./merge-types";

/** Runs one trial in a sandbox of its own: a new Durable Object instance for every call. */
export type TrialRunner = (main: Sha, commit: Sha) => Promise<TrialOutcome>;

/** What `reportTrial` tries: `commit` on main at `before`, the baseline, and at `main`. */
export interface TrialPlan {
  before: Sha;
  main: Sha;
  commit: Sha;
}

/**
 * Tries the commit on main at `before` and on main at `main`, both at once. Each run is its own
 * call to `runner`, so each has its own container, its own read tokens and its own work tree: what
 * one run leaves behind (files in `/tmp` or `$HOME`, a process still listening) cannot make the
 * other fail. Both runs try the same commit, which the caller resolved once, so a fork head that
 * moves meanwhile cannot make them differ. They run at once because one side's worst case already
 * fills the steward call timeout (see `STEWARD_CALL_TIMEOUT_MS`).
 *
 * The result of `main` is kept only if the baseline was `clean`: otherwise the work was already
 * failing and the main run says nothing about the merge that moved main, so it is discarded.
 * When `before` and `main` are the same sha there is one run, and it is both sides.
 *
 * @throws The first error of a run that threw, after both runs have finished.
 */
export async function reportTrial(plan: TrialPlan, runner: TrialRunner): Promise<TrialReport> {
  if (plan.before === plan.main) {
    const only = await runner(plan.main, plan.commit);
    return { before: only, after: only.outcome === "clean" ? only : null };
  }
  const [before, main] = await Promise.allSettled([
    runner(plan.before, plan.commit),
    runner(plan.main, plan.commit),
  ]);
  if (before.status === "rejected") {
    throw before.reason;
  }
  if (before.value.outcome !== "clean") {
    return { before: before.value, after: null };
  }
  if (main.status === "rejected") {
    throw main.reason;
  }
  return { before: before.value, after: main.value };
}
