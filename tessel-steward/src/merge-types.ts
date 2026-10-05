import { isValidName } from "./identity";
import { parseScopes, type ClaimedScope } from "./merge-coverage";
import type { StepOutcome } from "./run-steps";

/** A full, lowercase, 40-hex git object id. Only `parseSha` makes one. */
export type Sha = string & { readonly __brand: "Sha" };

const SHA_PATTERN = /^[0-9a-f]{40}$/;
const ARTIFACTS_FORK_SOURCE_PREFIX = "artifacts:";

/** Returns the value as a `Sha` if it is 40 lowercase hex characters. */
export function parseSha(value: unknown): Sha | undefined {
  return typeof value === "string" && SHA_PATTERN.test(value) ? (value as Sha) : undefined;
}

/**
 * A request to merge `commit`, which must be reachable from the default branch of `fork`, for a
 * claim that holds `scopes`. The merged change must be covered by them (invariant 11).
 */
export interface MergeRequest {
  fork: string;
  commit: Sha;
  scopes: ClaimedScope[];
}

export type ParsedMergeRequest = { ok: true; request: MergeRequest } | { ok: false; error: string };

/**
 * Validates the body of `POST /repos/<repo>/merges`:
 * `{ "fork": <repo name>, "commit": <sha>, "scopes": [{ "scope", "mode" }] }`. A request without
 * `scopes` is invalid: the coverage check is never skipped.
 */
export function parseMergeRequest(body: unknown): ParsedMergeRequest {
  if (typeof body !== "object" || body === null) {
    return { ok: false, error: 'expected a JSON object {"fork", "commit", "scopes"}' };
  }
  const { fork, commit, scopes } = body as {
    fork?: unknown;
    commit?: unknown;
    scopes?: unknown;
  };
  if (typeof fork !== "string" || !isValidName(fork)) {
    return { ok: false, error: "fork must be a repo name" };
  }
  const sha = parseSha(commit);
  if (sha === undefined) {
    return { ok: false, error: "commit must be 40 lowercase hex characters" };
  }
  const parsedScopes = parseScopes(scopes);
  if (!parsedScopes.ok) {
    return parsedScopes;
  }
  return { ok: true, request: { fork, commit: sha, scopes: parsedScopes.scopes } };
}

/**
 * Whether `info` describes a fork of the Artifacts repo `repo`. An imported repo, a non-fork and
 * a fork of any other repo are not. The namespace in `source` is not compared: both repos come
 * from this Worker's one ARTIFACTS binding.
 */
export function isForkOf(repo: string, info: { source: string | null }): boolean {
  const { source } = info;
  return (
    source !== null &&
    source.startsWith(ARTIFACTS_FORK_SOURCE_PREFIX) &&
    source.endsWith(`/${repo}`)
  );
}

const BRANCH_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._/-]{0,127}$/;

/** Whether a default branch name read from a fork is safe to put into a refspec. */
export function isSafeBranchName(name: string): boolean {
  return BRANCH_PATTERN.test(name) && !name.includes("..") && !name.endsWith("/");
}

/** Captured output of a git step: untrusted data, each stream capped by the runner. */
export type GitResult = Omit<StepOutcome, "step" | "passed">;

/**
 * The result of one merge attempt. Exactly one variant; the fields of a variant are exactly what
 * is known when it is returned.
 *
 * Evidence (CLAUDE.md rule 7). Only these are verified facts about the code:
 * - `merged`: main was updated to `head`. The Worker read main through the Artifacts binding after
 *   the push and got `head`; the sandbox's exit code is not trusted.
 * - `conflict`: replaying the commit onto the real main at `base` stopped with these files
 *   unmerged. This is the only outcome that shows a conflict was real.
 * - `tests_failed`: the repo's own `npm test` ran on the rebased `head` and did not exit 0.
 *   Exit code 124 or 137 means it timed out or was killed, which is not a failing assertion.
 * - `uncovered`: the commit rebased onto main changes files that the claim's scopes do not cover
 *   in a permitting mode (invariant 11), read from `git diff --name-status` of the rebased range
 *   before any repo code ran. `files` holds at most `MAX_REPORTED_FILES` paths (untrusted data);
 *   `total` counts all of them. Nothing was tested or pushed.
 *
 * Not conflict or test evidence, but true statements about this attempt:
 * - `already_merged`: the rebase left nothing to add to main.
 * - `main_moved`: another write reached main after `expected` was read; retry the merge.
 * - `commit_not_in_fork`: the commit is not reachable from the fork's default branch.
 *
 * Infrastructure (never counts for or against anything; the attempt did not finish):
 * - `clone`: cloning main or fetching the fork failed.
 * - `git_failed`: a local git step failed in a way that is not a conflict.
 * - `install`: the repo declares dependencies this runner cannot install, or the check did not
 *   complete, so its tests were not run.
 * - `push_failed`: the push did not move main and main did not move either.
 *
 * `stdout`/`stderr` in any `result` come from the repo or from git and are untrusted data.
 */
export type MergeOutcome =
  | { outcome: "merged"; base: Sha; head: Sha }
  | { outcome: "already_merged"; base: Sha }
  | { outcome: "conflict"; base: Sha; files: string[] }
  | { outcome: "tests_failed"; base: Sha; head: Sha; result: StepOutcome }
  | { outcome: "uncovered"; base: Sha; head: Sha; files: string[]; total: number }
  | { outcome: "main_moved"; expected: Sha; actual: Sha }
  | { outcome: "commit_not_in_fork" }
  | { outcome: "clone"; result: StepOutcome }
  | { outcome: "git_failed"; result: GitResult }
  | { outcome: "install"; base: Sha; head: Sha; result: StepOutcome }
  | { outcome: "push_failed"; base: Sha; head: Sha; result: GitResult };
