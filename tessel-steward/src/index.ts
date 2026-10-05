import { parsePushEvent } from "./push-event";

const AGENT_TOKEN_TTL_SECONDS = 3600;
const USAGE = "POST /repos/<repo>, POST /repos/<repo>/forks/<fork> or POST /repos/<repo>/tokens";

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

type Route =
  | { kind: "create"; repo: string }
  | { kind: "fork"; repo: string; fork: string }
  | { kind: "token"; repo: string };

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

function matchRoute(pathname: string): Route | undefined {
  const [root, repo, action, fork, ...rest] = pathname.split("/").filter(Boolean);
  if (root !== "repos" || repo === undefined || rest.length > 0) {
    return undefined;
  }
  if (action === undefined) {
    return { kind: "create", repo };
  }
  if (action === "tokens" && fork === undefined) {
    return { kind: "token", repo };
  }
  if (action === "forks" && fork !== undefined) {
    return { kind: "fork", repo, fork };
  }
  return undefined;
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
  const token = await handle.createToken("write", AGENT_TOKEN_TTL_SECONDS);
  const { remote } = await handle.info();
  const { plaintext, scope, expiresAt } = token;
  return json({ repo, remote, token: plaintext, scope, expiresAt }, 201);
}

function runRoute(env: Env, route: Route): Promise<Response> {
  switch (route.kind) {
    case "create":
      return createRepo(env, route.repo);
    case "fork":
      return forkRepo(env, route.repo, route.fork);
    case "token":
      return mintWriteToken(env, route.repo);
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
      return await runRoute(env, route);
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
