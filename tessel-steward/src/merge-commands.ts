import { MAX_PUSH_BODY_BYTES } from "./receive-pack-policy";
import { isSafeBranchName, type Sha } from "./merge-types";
import { STEP_SECONDS } from "./step-budget";

/** One git invocation. Every value is an argv element or an environment value, never shell text. */
export interface GitCommand {
  argv: string[];
  env: Record<string, string>;
  timeoutSeconds: number;
}

/** Where the merge runs and what it reads. `forkBranch` is the fork's default branch. */
export interface MergeSources {
  workspace: string;
  mainRemote: string;
  forkRemote: string;
  forkBranch: string;
}

export const MAIN_BRANCH = "main";
export const STEWARD_COMMITTER_NAME = "Tessel Steward";
export const STEWARD_COMMITTER_EMAIL = "steward@tessel.invalid";
/** Every replayed commit gets this committer date, so a retried rebase yields the same shas. */
export const STEWARD_COMMITTER_DATE = "2026-01-01T00:00:00Z";

const GIT_ENV: Record<string, string> = {
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_CONFIG_NOSYSTEM: "1",
  GIT_TERMINAL_PROMPT: "0",
  GIT_EDITOR: "true",
};

function local(workspace: string, args: string[], timeoutSeconds: number = STEP_SECONDS.local) {
  return { argv: ["git", "-C", workspace, ...args], env: GIT_ENV, timeoutSeconds };
}

function forkRef(branch: string): string {
  if (!isSafeBranchName(branch)) {
    throw new Error(`Refusing to use the unsafe fork branch name ${JSON.stringify(branch)}`);
  }
  return `refs/remotes/fork/${branch}`;
}

/** Clones main with its full history (the rebase needs the merge-base). */
export function cloneCommand(sources: MergeSources): GitCommand {
  const { workspace, mainRemote } = sources;
  return {
    argv: ["git", "clone", "--no-tags", `--branch=${MAIN_BRANCH}`, "--", mainRemote, workspace],
    env: GIT_ENV,
    timeoutSeconds: STEP_SECONDS.clone,
  };
}

/** Fetches only the fork's default branch, into `refs/remotes/fork/<branch>`. */
export function fetchForkCommand(sources: MergeSources): GitCommand {
  const { workspace, forkRemote, forkBranch } = sources;
  const ref = forkRef(forkBranch);
  return local(
    workspace,
    ["fetch", "--no-tags", "--", forkRemote, `+refs/heads/${forkBranch}:${ref}`],
    STEP_SECONDS.fetch,
  );
}

/** Resolves main as cloned. Stdout is the sha. */
export function baseCommand(workspace: string): GitCommand {
  return local(workspace, ["rev-parse", "--verify", `refs/remotes/origin/${MAIN_BRANCH}^{commit}`]);
}

/** Exit 0 when `main` is a commit on main's history as cloned. */
export function onMainCommand(workspace: string, main: Sha): GitCommand {
  return local(workspace, [
    "merge-base",
    "--is-ancestor",
    main,
    `refs/remotes/origin/${MAIN_BRANCH}`,
  ]);
}

/** Exit 0 when the commit object was fetched, 1 when it was not. */
export function commitExistsCommand(workspace: string, commit: Sha): GitCommand {
  return local(workspace, ["rev-parse", "--verify", "--quiet", `${commit}^{commit}`]);
}

/** Exit 0 when the commit is reachable from the fork's default branch, 1 when it is not. */
export function reachableCommand(sources: MergeSources, commit: Sha): GitCommand {
  const { workspace, forkBranch } = sources;
  return local(workspace, ["merge-base", "--is-ancestor", commit, forkRef(forkBranch)]);
}

/** Stdout is the merge-base of main and the commit. */
export function mergeBaseCommand(workspace: string, base: Sha, commit: Sha): GitCommand {
  return local(workspace, ["merge-base", base, commit]);
}

/** Replays `mergeBase..commit` onto `base` with a fixed committer; leaves HEAD at the result. */
export function rebaseCommand(
  workspace: string,
  base: Sha,
  mergeBase: Sha,
  commit: Sha,
): GitCommand {
  const command = local(
    workspace,
    [
      "-c",
      "core.hooksPath=/dev/null",
      "-c",
      "commit.gpgsign=false",
      "rebase",
      "--onto",
      base,
      mergeBase,
      commit,
    ],
    STEP_SECONDS.rebase,
  );
  return {
    ...command,
    env: {
      ...GIT_ENV,
      GIT_COMMITTER_NAME: STEWARD_COMMITTER_NAME,
      GIT_COMMITTER_EMAIL: STEWARD_COMMITTER_EMAIL,
      GIT_COMMITTER_DATE: STEWARD_COMMITTER_DATE,
    },
  };
}

/** Stdout is the NUL-separated repo paths that are unmerged after a stopped rebase. */
export function conflictsCommand(workspace: string): GitCommand {
  return local(workspace, ["diff", "--name-only", "--diff-filter=U", "-z"]);
}

/**
 * Stdout is the NUL-separated `git diff --name-status` records of `base..head`, with renames
 * detected. `-z` keeps paths unquoted; they are untrusted data and are never put in a command.
 */
export function changedFilesCommand(workspace: string, base: Sha, head: Sha): GitCommand {
  return local(workspace, [
    "diff",
    "--name-status",
    "-z",
    "-M",
    "--no-ext-diff",
    `${base}..${head}`,
    "--",
  ]);
}

/** Stdout is the sha HEAD points at. */
export function headCommand(workspace: string): GitCommand {
  return local(workspace, ["rev-parse", "--verify", "HEAD^{commit}"]);
}

/**
 * Pushes `head` to main only if main is still `base`. Never forces: `head` descends from `base`,
 * and the lease names the sha that was read at the start.
 */
export function pushCommand(sources: MergeSources, base: Sha, head: Sha): GitCommand {
  const { workspace, mainRemote } = sources;
  return local(
    workspace,
    [
      "-c",
      `http.postBuffer=${MAX_PUSH_BODY_BYTES}`,
      "push",
      "--porcelain",
      `--force-with-lease=refs/heads/${MAIN_BRANCH}:${base}`,
      "--",
      mainRemote,
      `${head}:refs/heads/${MAIN_BRANCH}`,
    ],
    STEP_SECONDS.push,
  );
}

/** Splits `git diff -z` output into paths. Paths are untrusted data. */
export function parseNulSeparated(stdout: string): string[] {
  return stdout.split("\0").filter((path) => path !== "");
}
