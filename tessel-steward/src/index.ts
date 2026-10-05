import { IDENTITY_TTL_MS, isValidName, signIdentityToken } from "./identity";
import { parsePushEvent } from "./push-event";
import { isForkOf, parseMergeRequest } from "./merge-types";
import { matchRoute, type Route } from "./routes";
import type { TestRunResult } from "./test-runner";
import { mintForkWriteToken } from "./token-policy";

export { MergePushGateway, MergeReadGateway } from "./merge-gateway";
export { ArtifactsGitGateway, TestRunner } from "./test-runner";

const AGENT_TOKEN_TTL_SECONDS = 3600;
const USAGE =
  "POST /repos/<repo>, POST /repos/<repo>/forks/<fork>, POST /repos/<repo>/tokens, " +
  "POST /repos/<repo>/test-runs, POST /repos/<repo>/merges or POST /repos/<repo>/agents/<agent>/identity";
const NOT_A_FORK_MESSAGE =
  "write tokens are issued only for agent forks; only the steward writes the main repo";
const TEST_REF = "main";
const INVALID_NAME_MESSAGE =
  "repo and agent must each be 1 to 128 characters of A-Z a-z 0-9 . _ - starting with a letter or digit";

const STATUS_BY_ARTIFACTS_CODE: Record<ArtifactsErrorCode, number> = {
  ALREADY_EXISTS: 409,
  NOT_FOUND: 404,
  CREATE_IN_PROGRESS: 409,
  IMPORT_IN_PROGRESS: 409,
  FORK_IN_PROGRESS: 409,
  INVALID_INPUT: 400,
  INVALID_REPO_NAME: 400,
  INVALID_TTL: 400,
  INVALID_URL: 400,
  REMOTE_AUTH_REQUIRED: 502,
  UPSTREAM_UNAVAILABLE: 502,
  MEMORY_LIMIT: 502,
  INTERNAL_ERROR: 502,
};

function json(body: unknown, status: number): Response {
  return Response.json(body, { status });
}

async function isAuthorized(request: Request, env: Env): Promise<boolean> {
  const presented = request.headers.get("Authorization");
  if (!env.STEWARD_ADMIN_TOKEN || presented === null) {
    return false;
  }
  const encoder = new TextEncoder();
  const [presentedHash, expectedHash] = await Promise.all([
    crypto.subtle.digest("SHA-256", encoder.encode(presented)),
    crypto.subtle.digest("SHA-256", encoder.encode(`Bearer ${env.STEWARD_ADMIN_TOKEN}`)),
  ]);
  return crypto.subtle.timingSafeEqual(presentedHash, expectedHash);
}

async function createRepo(env: Env, repo: string): Promise<Response> {
  const created = await env.ARTIFACTS.create(repo, { setDefaultBranch: "main" });
  const { name, remote, defaultBranch, token } = created;
  return json({ name, remote, defaultBranch, token }, 201);
}

async function forkRepo(env: Env, repo: string, fork: string): Promise<Response> {
  using source = await env.ARTIFACTS.get(repo);
  const forked = await source.fork(fork, {
    description: `Tessel fork of ${repo}`,
    defaultBranchOnly: true,
  });
  const { name, remote, defaultBranch } = forked;
  return json({ name, remote, defaultBranch }, 201);
}

async function mintWriteToken(env: Env, repo: string): Promise<Response> {
  using handle = await env.ARTIFACTS.get(repo);
  const minted = await mintForkWriteToken(handle, AGENT_TOKEN_TTL_SECONDS);
  if (!minted.minted) {
    return json({ error: NOT_A_FORK_MESSAGE }, 403);
  }
  const { remote } = minted.info;
  const { plaintext, scope, expiresAt } = minted.token;
  return json({ repo, remote, token: plaintext, scope, expiresAt }, 201);
}

async function runTests(env: Env, repo: string): Promise<Response> {
  const runner = env.TEST_RUNNER.getByName(crypto.randomUUID());
  const result: TestRunResult = await runner.runTests(repo, TEST_REF);
  return json(result, 200);
}

async function mergeFork(env: Env, request: Request, repo: string): Promise<Response> {
  if (!isValidName(repo)) {
    return json({ error: INVALID_NAME_MESSAGE }, 400);
  }
  const parsed = parseMergeRequest(await request.json().catch(() => null));
  if (!parsed.ok) {
    return json({ error: parsed.error }, 400);
  }
  const { fork, commit } = parsed.request;
  using handle = await env.ARTIFACTS.get(fork);
  if (!isForkOf(repo, await handle.info())) {
    return json({ error: `${fork} is not a fork of ${repo}` }, 400);
  }
  const runner = env.TEST_RUNNER.getByName(crypto.randomUUID());
  return json(await runner.merge(repo, fork, commit), 200);
}

async function issueIdentity(env: Env, repo: string, agent: string): Promise<Response> {
  if (!isValidName(repo) || !isValidName(agent)) {
    return json({ error: INVALID_NAME_MESSAGE }, 400);
  }
  const expiresAtMs = Date.now() + IDENTITY_TTL_MS;
  const token = await signIdentityToken(env.IDENTITY_SIGNING_KEY, {
    repo,
    agent,
    expMs: expiresAtMs,
  });
  return json({ token, agent, repo, expires_at_ms: expiresAtMs }, 201);
}

function runRoute(env: Env, request: Request, route: Route): Promise<Response> {
  switch (route.kind) {
    case "create":
      return createRepo(env, route.repo);
    case "fork":
      return forkRepo(env, route.repo, route.fork);
    case "token":
      return mintWriteToken(env, route.repo);
    case "test-run":
      return runTests(env, route.repo);
    case "merge":
      return mergeFork(env, request, route.repo);
    case "identity":
      return issueIdentity(env, route.repo, route.agent);
  }
}

function failure(route: Route, error: unknown): Response {
  if (!(error instanceof Error)) {
    throw error;
  }
  const code = (error as Partial<ArtifactsError>).code;
  const status = code === undefined ? 500 : (STATUS_BY_ARTIFACTS_CODE[code] ?? 500);
  console.error(JSON.stringify({ event: "route_failed", route, code, message: error.message }));
  return json({ error: `${route.kind} failed for ${route.repo}: ${error.message}`, code }, status);
}

export default {
  async fetch(request, env): Promise<Response> {
    if (!(await isAuthorized(request, env))) {
      return json({ error: "send Authorization: Bearer <STEWARD_ADMIN_TOKEN>" }, 401);
    }
    const route = matchRoute(new URL(request.url).pathname);
    if (request.method !== "POST" || route === undefined) {
      return json({ error: `expected ${USAGE}` }, 404);
    }
    try {
      return await runRoute(env, request, route);
    } catch (error) {
      return failure(route, error);
    }
  },

  async queue(batch, _env): Promise<void> {
    for (const message of batch.messages) {
      const parsed = parsePushEvent(message.body);
      if (parsed.ok) {
        console.log(JSON.stringify({ event: "push", ...parsed.push }));
      } else {
        console.error(
          JSON.stringify({ event: "unusable_message", id: message.id, reason: parsed.reason }),
        );
      }
      message.ack();
    }
  },
} satisfies ExportedHandler<Env>;
