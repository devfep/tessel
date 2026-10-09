import {
  baseCommand,
  changedFilesCommand,
  cloneCommand,
  commitExistsCommand,
  diffCommand,
  fetchForkCommand,
  mergeBaseCommand,
} from "./merge-commands";
import { parseSha, type GitResult, type Sha } from "./merge-types";
import type { TrialDeps } from "./merge-steps";

/** The most diff text a review page is given. */
export const DIFF_CAP_BYTES = 200 * 1024;

export interface ChangedFile {
  /** `A`dded, `M`odified, `D`eleted, `R`enamed, `C`opied, `T`ype changed. */
  status: string;
  /** The path after the change. Untrusted data. */
  path: string;
  /** The old path of a rename or copy. Untrusted data. */
  from?: string;
}

/**
 * The diff of one commit against its merge-base with main, or why there is none.
 * `diff` is cut at `DIFF_CAP_BYTES` when `truncated`; when the sandbox's own capture overflowed
 * (`captureOverflow`) the start of the output is gone, so `diff` is empty and only `files` shows.
 */
export type DiffOutcome =
  | {
      outcome: "ok";
      base: Sha;
      commit: Sha;
      files: ChangedFile[];
      diff: string;
      truncated: boolean;
      captureOverflow: boolean;
    }
  | { outcome: "error"; reason: string };

function failed(step: string, result: GitResult): DiffOutcome {
  return { outcome: "error", reason: `${step} failed (git exit ${result.exitCode})` };
}

/** Parses `git diff --name-status -z -M`; `undefined` when the output is malformed. */
export function parseChangedFiles(stdout: string): ChangedFile[] | undefined {
  const tokens = stdout.split("\0");
  if (tokens.at(-1) === "") {
    tokens.pop();
  }
  const files: ChangedFile[] = [];
  let at = 0;
  while (at < tokens.length) {
    const status = tokens[at++]?.[0];
    const first = tokens[at++];
    const renamed = status === "R" || status === "C";
    const second = renamed ? tokens[at++] : undefined;
    if (status === undefined || first === undefined || first === "") {
      return undefined;
    }
    if (renamed) {
      if (second === undefined || second === "") {
        return undefined;
      }
      files.push({ status, path: second, from: first });
    } else {
      files.push({ status, path: first });
    }
  }
  return files;
}

function capBytes(text: string): { text: string; truncated: boolean } {
  const bytes = new TextEncoder().encode(text);
  if (bytes.length <= DIFF_CAP_BYTES) {
    return { text, truncated: false };
  }
  const cut = new TextDecoder().decode(bytes.slice(0, DIFF_CAP_BYTES));
  return { text: cut.replace(/�$/, ""), truncated: true };
}

async function fetchSources(deps: TrialDeps): Promise<DiffOutcome | undefined> {
  const clone = await deps.run(cloneCommand(deps.sources));
  if (clone.exitCode !== 0) {
    return failed("cloning main", clone);
  }
  const fetched = await deps.run(fetchForkCommand(deps.sources));
  return fetched.exitCode === 0 ? undefined : failed("fetching the fork", fetched);
}

/** Resolves `commit`'s merge-base with main in the fetched workspace. */
async function findBase(deps: TrialDeps, commit: Sha): Promise<Sha | DiffOutcome> {
  const { workspace } = deps.sources;
  const exists = await deps.run(commitExistsCommand(workspace, commit));
  if (exists.exitCode !== 0) {
    return { outcome: "error", reason: "the commit is not in the fork" };
  }
  const mainResult = await deps.run(baseCommand(workspace));
  const main = mainResult.exitCode === 0 ? parseSha(mainResult.stdout.trim()) : undefined;
  if (main === undefined) {
    return failed("reading main", mainResult);
  }
  const result = await deps.run(mergeBaseCommand(workspace, main, commit));
  const base = result.exitCode === 0 ? parseSha(result.stdout.trim()) : undefined;
  return base ?? failed("finding the merge-base", result);
}

async function readChanges(deps: TrialDeps, base: Sha, commit: Sha): Promise<DiffOutcome> {
  const { workspace } = deps.sources;
  const names = await deps.run(changedFilesCommand(workspace, base, commit));
  const files =
    names.exitCode === 0 && !names.stdoutTruncated ? parseChangedFiles(names.stdout) : undefined;
  if (files === undefined) {
    return failed("listing the changed files", names);
  }
  const patch = await deps.run(diffCommand(workspace, base, commit));
  if (patch.exitCode !== 0) {
    return failed("reading the diff", patch);
  }
  if (patch.stdoutTruncated) {
    return { outcome: "ok", base, commit, files, diff: "", truncated: true, captureOverflow: true };
  }
  const { text, truncated } = capBytes(patch.stdout);
  return { outcome: "ok", base, commit, files, diff: text, truncated, captureOverflow: false };
}

/**
 * Reads what `commit` of the fork changes relative to main, in a sandbox that already has read
 * access to both: clone main, fetch the fork, revoke the read tokens, then local git only. The
 * base is the merge-base of main and `commit`, so main moving since the fork does not show up as
 * the commit's change. Nothing is rebased, tested, or pushed.
 *
 * @param deps The sandbox and read-token boundaries; `deps.run` never sees a write token.
 * @param commit The fork commit under review.
 * @throws If a read token cannot be revoked or a boundary throws.
 */
export async function runDiff(deps: TrialDeps, commit: Sha): Promise<DiffOutcome> {
  const fetchFailure = await fetchSources(deps);
  if (!(await deps.revokeReadTokens())) {
    throw new Error("a read token could not be revoked");
  }
  if (fetchFailure !== undefined) {
    return fetchFailure;
  }
  const base = await findBase(deps, commit);
  return typeof base === "string" ? readChanges(deps, base, commit) : base;
}
