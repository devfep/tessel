import {
  baseCommand,
  changedFilesCommand,
  cloneCommand,
  commitExistsCommand,
  conflictsCommand,
  fetchForkCommand,
  headCommand,
  mergeBaseCommand,
  onMainCommand,
  parseNulSeparated,
  pushCommand,
  rebaseCommand,
  reachableCommand,
  type GitCommand,
  type MergeSources,
} from "./merge-commands";
import {
  MAX_REPORTED_FILES,
  parseNameStatus,
  uncoveredPaths,
  type ClaimedScope,
} from "./merge-coverage";
import { TESSEL_TOML_PATH } from "./tessel-config";
import {
  parseSha,
  type GitResult,
  type MergeOutcome,
  type Sha,
  type TrialOutcome,
} from "./merge-types";
import { runInstallThenTest, runStepThenRevoke, type StepOutcome } from "./run-steps";

/** The one update the write token may be used for: main from `base` to `head`. */
export interface PushUpdate {
  base: Sha;
  head: Sha;
}

/**
 * What a trial needs from the outside world: the sandbox and the read tokens. It has no way to
 * push and no way to mint a write token, so `runTrial` cannot do either.
 */
export interface TrialDeps {
  sources: MergeSources;
  /** Runs one git command in the sandbox. */
  run(command: GitCommand): Promise<GitResult>;
  /** Revokes the read tokens of main and of the fork; must not throw; false on failure. */
  revokeReadTokens(): Promise<boolean>;
  /** Runs the install step or the test step of the run's gate plan; no write token is live. */
  runPackageStep(step: "install" | "test"): Promise<StepOutcome>;
}

/** Everything `runMerge` needs from the outside world: a trial's boundaries, plus main. */
export interface MergeDeps extends TrialDeps {
  /**
   * The commit of main whose gate (`tessel.toml`) the sandbox was started for. The merge must be
   * based on exactly this commit; if the clone finds main elsewhere the merge is `main_moved`.
   */
  pinnedMain: Sha;
  /**
   * Mints the write token for main, lets `use` push exactly `update`, and revokes the token when
   * `use` ends, whether it returned or threw. Called only after the tests passed.
   */
  withPushAccess<T>(update: PushUpdate, use: () => Promise<T>): Promise<T>;
  /** The sha main points at now, or null if it could not be read. */
  currentMain(): Promise<Sha | null>;
}

interface Rebased {
  outcome: "rebased";
  base: Sha;
  head: Sha;
}

type VerifyFailure = Extract<MergeOutcome, { outcome: "commit_not_in_fork" | "git_failed" }>;
type RebaseFailure = Extract<MergeOutcome, { outcome: "conflict" | "git_failed" }>;

function shaOf(result: GitResult): Sha | undefined {
  return result.exitCode === 0 ? parseSha(result.stdout.trim()) : undefined;
}

function asCloneStep(result: GitResult): StepOutcome {
  return { step: "clone", ...result, passed: false };
}

async function fetchSources(deps: TrialDeps): Promise<StepOutcome> {
  const clone = await deps.run(cloneCommand(deps.sources));
  if (clone.exitCode !== 0) {
    return asCloneStep(clone);
  }
  return asCloneStep(await deps.run(fetchForkCommand(deps.sources)));
}

/** Checks that the commit exists in the fetched fork and is reachable from its default branch. */
async function verifyCommit(deps: TrialDeps, commit: Sha): Promise<VerifyFailure | undefined> {
  const { sources } = deps;
  const exists = await deps.run(commitExistsCommand(sources.workspace, commit));
  if (exists.exitCode === 1) {
    return { outcome: "commit_not_in_fork" };
  }
  if (exists.exitCode !== 0) {
    return { outcome: "git_failed", result: exists };
  }
  const reachable = await deps.run(reachableCommand(sources, commit));
  if (reachable.exitCode === 1) {
    return { outcome: "commit_not_in_fork" };
  }
  return reachable.exitCode === 0 ? undefined : { outcome: "git_failed", result: reachable };
}

async function rebaseOntoMain(
  deps: TrialDeps,
  base: Sha,
  commit: Sha,
): Promise<Rebased | RebaseFailure> {
  const { workspace } = deps.sources;
  const mergeBaseResult = await deps.run(mergeBaseCommand(workspace, base, commit));
  const mergeBase = shaOf(mergeBaseResult);
  if (mergeBase === undefined) {
    return { outcome: "git_failed", result: mergeBaseResult };
  }
  const rebase = await deps.run(rebaseCommand(workspace, base, mergeBase, commit));
  if (rebase.exitCode !== 0) {
    const conflicts = await deps.run(conflictsCommand(workspace));
    const files = conflicts.exitCode === 0 ? parseNulSeparated(conflicts.stdout) : [];
    return files.length > 0
      ? { outcome: "conflict", base, files }
      : { outcome: "git_failed", result: rebase };
  }
  const headResult = await deps.run(headCommand(workspace));
  const head = shaOf(headResult);
  return head === undefined
    ? { outcome: "git_failed", result: headResult }
    : { outcome: "rebased", base, head };
}

/**
 * Invariant 11 on the change the steward sees: every file the rebased range `base..head` changes
 * must be covered by the claim, and none may be the gate (`tessel.toml`: a fork must not weaken the
 * gate it is judged by; a human changes it by hand). Runs before any repo code. A diff that fails, is cut off, or has
 * a record this code does not know is `git_failed`: it is never read as covered.
 */
async function checkCoverage(
  deps: MergeDeps,
  base: Sha,
  head: Sha,
  scopes: readonly ClaimedScope[],
): Promise<MergeOutcome | undefined> {
  const diff = await deps.run(changedFilesCommand(deps.sources.workspace, base, head));
  const complete = diff.exitCode === 0 && !diff.stdoutTruncated;
  const required = complete ? parseNameStatus(diff.stdout) : undefined;
  if (required === undefined) {
    return { outcome: "git_failed", result: diff };
  }
  if (required.some(({ path }) => path === TESSEL_TOML_PATH)) {
    return { outcome: "gate_changed", base, head };
  }
  const uncovered = uncoveredPaths(required, scopes);
  if (uncovered.length === 0) {
    return undefined;
  }
  return {
    outcome: "uncovered",
    base,
    head,
    files: uncovered.slice(0, MAX_REPORTED_FILES),
    total: uncovered.length,
  };
}

/**
 * Pushes, then decides the outcome from a read of main made by the Worker through the Artifacts
 * binding, never from the push command's exit code: repo code ran in the sandbox and can fake
 * it. Main at `head` is `merged`; at `base` it is `push_failed`; at anything else it is
 * `main_moved`; unreadable is `push_failed`.
 */
async function pushToMain(deps: MergeDeps, base: Sha, head: Sha): Promise<MergeOutcome> {
  const result = await deps.withPushAccess({ base, head }, () =>
    deps.run(pushCommand(deps.sources, base, head)),
  );
  const actual = await deps.currentMain();
  if (actual === head) {
    return { outcome: "merged", base, head };
  }
  if (actual !== null && actual !== base) {
    return { outcome: "main_moved", expected: base, actual };
  }
  return { outcome: "push_failed", base, head, result };
}

/**
 * Merges `commit` of the fork into main: rebase onto main, check coverage, test, push with a
 * lease.
 *
 * Order, which the tests pin:
 * 1. Clone main and fetch the fork with read tokens, then revoke both before anything else runs
 *    (if a revocation fails, nothing runs and this throws).
 * 2. Verify the commit is reachable from the fork's default branch.
 * 3. Check the cloned main is the commit whose gate the sandbox started for (else `main_moved`),
 *    and rebase `merge-base..commit` onto it, with a fixed committer.
 * 4. Check that `scopes` cover every file the rebased range changes (invariant 11), before any
 *    repo code runs. The check is file level; see `merge-coverage.ts`.
 * 5. Run the dependency check and the tests. No token of any kind is live.
 * 6. Only if the tests passed: mint the write token, push with a lease on the cloned main,
 *    revoke the write token.
 *
 * @param deps The sandbox, token and main-reading boundaries.
 * @param commit The submitted commit, already validated as a sha.
 * @param scopes The scopes the claim holds. Required: the coverage check cannot be skipped.
 * @returns The outcome. Which outcomes are evidence is documented on `MergeOutcome`.
 * @throws If a read token cannot be revoked, or a boundary throws.
 */
export async function runMerge(
  deps: MergeDeps,
  commit: Sha,
  scopes: readonly ClaimedScope[],
): Promise<MergeOutcome> {
  const fetched = await runStepThenRevoke(() => fetchSources(deps), deps.revokeReadTokens);
  if (fetched.exitCode !== 0) {
    return { outcome: "clone", result: fetched };
  }
  const baseResult = await deps.run(baseCommand(deps.sources.workspace));
  const base = shaOf(baseResult);
  if (base === undefined) {
    return { outcome: "git_failed", result: baseResult };
  }
  if (base !== deps.pinnedMain) {
    return { outcome: "main_moved", expected: deps.pinnedMain, actual: base };
  }
  const unverified = await verifyCommit(deps, commit);
  if (unverified !== undefined) {
    return unverified;
  }
  const rebased = await rebaseOntoMain(deps, base, commit);
  if (rebased.outcome !== "rebased") {
    return rebased;
  }
  const { head } = rebased;
  if (head === base) {
    return { outcome: "already_merged", base };
  }
  const uncovered = await checkCoverage(deps, base, head, scopes);
  if (uncovered !== undefined) {
    return uncovered;
  }
  const tested = await runInstallThenTest((step) => deps.runPackageStep(step));
  if (tested.reason === "timeout") {
    return { outcome: "timeout", base, head, result: tested };
  }
  if (tested.step === "install") {
    return { outcome: "install", base, head, result: tested };
  }
  if (!tested.passed) {
    return { outcome: "tests_failed", base, head, result: tested };
  }
  return pushToMain(deps, base, head);
}

/**
 * Checks that `main` is a commit on main's history, so a trial is against a real state of main.
 * Any answer but yes is `main_unreachable`, which is final.
 */
async function pinMain(deps: TrialDeps, main: Sha): Promise<TrialOutcome | undefined> {
  const onMain = await deps.run(onMainCommand(deps.sources.workspace, main));
  return onMain.exitCode === 0 ? undefined : { outcome: "main_unreachable", main };
}

/** Replays `commit` onto `main` and tests it. */
async function tryOnMain(deps: TrialDeps, main: Sha, commit: Sha): Promise<TrialOutcome> {
  const rebased = await rebaseOntoMain(deps, main, commit);
  switch (rebased.outcome) {
    case "conflict":
      return { ...rebased, commit };
    case "git_failed":
      return rebased;
    case "rebased":
      return testRebased(deps, rebased, commit);
  }
}

/**
 * Tries a fork's commit on main as it was at `main`: the steps of `runMerge` up to and including
 * the tests, and nothing after them. It does not push, does not touch the write token (`deps`
 * cannot) and does not check coverage: it verifies, it does not merge. It is one run in one
 * sandbox; `reportTrial` makes two, for a baseline and for the new main.
 *
 * Order, which the tests pin:
 * 1. Clone main and fetch the fork with read tokens, then revoke both before anything else runs.
 * 2. Check that `main` is on main's history.
 * 3. Verify that `commit` is reachable from the fork's default branch.
 * 4. Rebase `merge-base..commit` onto `main` with the fixed committer, as a merge does.
 * 5. Run the dependency check and the tests.
 *
 * @param deps The sandbox and read-token boundaries.
 * @param main The sha of main to try the commit on.
 * @param commit The commit to try.
 * @returns The outcome. Which outcomes are evidence is documented on `TrialOutcome`.
 * @throws If a read token cannot be revoked, or a boundary throws.
 */
export async function runTrial(deps: TrialDeps, main: Sha, commit: Sha): Promise<TrialOutcome> {
  const fetched = await runStepThenRevoke(() => fetchSources(deps), deps.revokeReadTokens);
  if (fetched.exitCode !== 0) {
    return { outcome: "clone", result: fetched };
  }
  const unpinned = await pinMain(deps, main);
  if (unpinned !== undefined) {
    return unpinned;
  }
  const unverified = await verifyCommit(deps, commit);
  if (unverified !== undefined) {
    return unverified;
  }
  return tryOnMain(deps, main, commit);
}

async function testRebased(deps: TrialDeps, rebased: Rebased, commit: Sha): Promise<TrialOutcome> {
  const { base, head } = rebased;
  if (head === base) {
    return { outcome: "nothing_to_test", base, commit };
  }
  const tested = await runInstallThenTest((step) => deps.runPackageStep(step));
  if (tested.reason === "timeout") {
    return { outcome: "timeout", base, head, commit, result: tested };
  }
  if (tested.step === "install") {
    return { outcome: "install", base, head, commit, result: tested };
  }
  if (!tested.passed) {
    return { outcome: "tests_failed", base, head, commit, result: tested };
  }
  return { outcome: "clean", base, head, commit };
}
