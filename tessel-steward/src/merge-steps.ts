import {
  baseCommand,
  changedFilesCommand,
  cloneCommand,
  commitExistsCommand,
  conflictsCommand,
  fetchForkCommand,
  headCommand,
  mergeBaseCommand,
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
import { parseSha, type GitResult, type MergeOutcome, type Sha } from "./merge-types";
import { runInstallThenTest, runStepThenRevoke, type StepOutcome } from "./run-steps";

/** The one update the write token may be used for: main from `base` to `head`. */
export interface PushUpdate {
  base: Sha;
  head: Sha;
}

/** Everything `runMerge` needs from the outside world: the sandbox, the tokens and main. */
export interface MergeDeps {
  sources: MergeSources;
  /** Runs one git command in the sandbox. */
  run(command: GitCommand): Promise<GitResult>;
  /** Revokes the read tokens of main and of the fork; must not throw; false on failure. */
  revokeReadTokens(): Promise<boolean>;
  /** Runs the dependency check ("install") or `npm test` ("test"); no write token is live. */
  runPackageStep(step: "install" | "test"): Promise<StepOutcome>;
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

function shaOf(result: GitResult): Sha | undefined {
  return result.exitCode === 0 ? parseSha(result.stdout.trim()) : undefined;
}

function asCloneStep(result: GitResult): StepOutcome {
  return { step: "clone", ...result, passed: false };
}

async function fetchSources(deps: MergeDeps): Promise<StepOutcome> {
  const clone = await deps.run(cloneCommand(deps.sources));
  if (clone.exitCode !== 0) {
    return asCloneStep(clone);
  }
  return asCloneStep(await deps.run(fetchForkCommand(deps.sources)));
}

/** Checks that the commit exists in the fetched fork and is reachable from its default branch. */
async function verifyCommit(deps: MergeDeps, commit: Sha): Promise<MergeOutcome | undefined> {
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
  deps: MergeDeps,
  base: Sha,
  commit: Sha,
): Promise<Rebased | MergeOutcome> {
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
 * must be covered by the claim. Runs before any repo code. A diff that fails, is cut off, or has
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
 * 3. Rebase `merge-base..commit` onto the main that was cloned, with a fixed committer.
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
  if (tested.step === "install") {
    return { outcome: "install", base, head, result: tested };
  }
  if (!tested.passed) {
    return { outcome: "tests_failed", base, head, result: tested };
  }
  return pushToMain(deps, base, head);
}
