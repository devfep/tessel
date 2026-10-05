import { INVALID_NAME_MESSAGE, isValidName } from "./identity";
import { isForkOf, parseMergeRequest } from "./merge-types";

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
