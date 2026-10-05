import { INVALID_NAME_MESSAGE, isValidName } from "./identity";
import { isForkOf, parseMergeRequest, parseSha, parseTrialRequest, type Sha } from "./merge-types";
import { reportTrial } from "./trial-report";

function json(body: unknown, status: number): Response {
  return Response.json(body, { status });
}

/**
 * Validates `{ "fork", "commit", "scopes" }` for `repo`, checks that the fork is a fork of `repo`, and runs
 * the merge in a fresh test runner. Answers 200 with the `MergeOutcome` or 400 with an error. A
 * merge that throws is for the caller to report.
 */
export async function handleMergeRequest(env: Env, repo: string, body: unknown): Promise<Response> {
  if (!isValidName(repo)) {
    return json({ error: INVALID_NAME_MESSAGE }, 400);
  }
  const parsed = parseMergeRequest(body);
  if (!parsed.ok) {
    return json({ error: parsed.error }, 400);
  }
  const { fork, commit, scopes } = parsed.request;
  using handle = await env.ARTIFACTS.get(fork);
  if (!isForkOf(repo, await handle.info())) {
    return json({ error: `${fork} is not a fork of ${repo}` }, 400);
  }
  const runner = env.TEST_RUNNER.getByName(crypto.randomUUID());
  return json(await runner.merge(repo, fork, commit, scopes), 200);
}

/**
 * Validates `{ "fork", "before", "main", "commit"? }` for `repo`, checks that the fork is a fork of
 * `repo`, resolves the commit to try once (the request's, or the head of the fork's default branch
 * read through the Artifacts binding) and runs the trial on main at `before` and at `main` at the
 * same time, each in a fresh test runner, never merged. Answers 200 with the `TrialReport` or 400
 * with an error.
 *
 * @throws If the fork's head cannot be read.
 */
export async function handleTrialRequest(env: Env, repo: string, body: unknown): Promise<Response> {
  if (!isValidName(repo)) {
    return json({ error: INVALID_NAME_MESSAGE }, 400);
  }
  const parsed = parseTrialRequest(body);
  if (!parsed.ok) {
    return json({ error: parsed.error }, 400);
  }
  const { fork, before, main } = parsed.request;
  using handle = await env.ARTIFACTS.get(fork);
  const info = await handle.info();
  if (!isForkOf(repo, info)) {
    return json({ error: `${fork} is not a fork of ${repo}` }, 400);
  }
  const commit = parsed.request.commit ?? (await forkHead(handle, info.defaultBranch));
  const report = await reportTrial({ before, main, commit }, (tried, tryCommit) =>
    env.TEST_RUNNER.getByName(crypto.randomUUID()).trial(repo, fork, tried, tryCommit),
  );
  return json(report, 200);
}

/** The head of the fork's default branch, read through the Artifacts binding. */
async function forkHead(handle: ArtifactsRepo, branch: string): Promise<Sha> {
  const [newest] = await handle.log({ ref: branch, limit: 1 });
  const head = parseSha(newest?.hash);
  if (head === undefined) {
    throw new Error("the fork has no readable head");
  }
  return head;
}
