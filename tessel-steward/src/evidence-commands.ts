import { RECORD_REF, type PlannedCommit } from "./evidence-objects";
import { GIT_ENV, MAIN_BRANCH } from "./merge-commands";
import type { Sha } from "./merge-types";
import { STEP_SECONDS } from "./step-budget";

/** One command of the record step: argv and environment, plus the bytes it reads on stdin. */
export interface RecordCommand {
  argv: string[];
  env: Record<string, string>;
  timeoutSeconds: number;
  stdin: Uint8Array;
}

/** The `old` argument of `RECORD_SCRIPT` for a repo whose main has no commit yet. */
const NO_PARENT = "none";

/**
 * Builds the planned commit in a scratch repo and pushes it to main of the record repo, in one
 * command so the step has one timeout. Fixed text: every value is a positional argument
 * (`$1` remote, `$2` the parent or `none`, `$3` the commit the Worker computed), never shell text.
 *
 * It fetches main (a full fetch: a push from a shallow repo sends `shallow` lines, which the push
 * gateway refuses), builds the commit from stdin with `git fast-import`, checks the result is the
 * computed commit, and pushes it with a lease on the parent (on "no main" for a new repo). It never
 * forces. The gateway lets only that one update through, so the check here only fails early.
 */
export const RECORD_SCRIPT = [
  "set -eu",
  'remote="$1" old="$2" new="$3"',
  "dir=$(mktemp -d)",
  "trap 'rm -rf \"$dir\"' EXIT",
  'git init --quiet --bare "$dir" </dev/null',
  `lease="refs/heads/${MAIN_BRANCH}:"`,
  `if [ "$old" != ${NO_PARENT} ]; then`,
  `  git -C "$dir" fetch --quiet --no-tags -- "$remote" \\`,
  `    "+refs/heads/${MAIN_BRANCH}:refs/tessel/old" </dev/null`,
  `  lease="refs/heads/${MAIN_BRANCH}:$old"`,
  "fi",
  'git -C "$dir" fast-import --quiet',
  `built=$(git -C "$dir" rev-parse --verify "${RECORD_REF}^{commit}")`,
  'if [ "$built" != "$new" ]; then',
  '  echo "fast-import built $built, not $new" >&2',
  "  exit 3",
  "fi",
  `git -C "$dir" push --porcelain "--force-with-lease=$lease" -- "$remote" \\`,
  `  "$new:refs/heads/${MAIN_BRANCH}" </dev/null`,
].join("\n");

/** The command that builds `planned` on top of `parent` and pushes it to `remote`'s main. */
export function recordCommand(
  remote: string,
  parent: Sha | null,
  planned: PlannedCommit,
): RecordCommand {
  return {
    argv: ["sh", "-c", RECORD_SCRIPT, "tessel-record", remote, parent ?? NO_PARENT, planned.commit],
    env: GIT_ENV,
    timeoutSeconds: STEP_SECONDS.record,
    stdin: planned.stream,
  };
}
