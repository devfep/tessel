import { readAccessConfig, verifyAccessToken, type AccessDeps } from "./access";
import { dashboardPage } from "./dashboard-page";
import { fetchSummary, openCoordinatorSocket } from "./coordinator-link";
import { INVALID_NAME_MESSAGE, isValidName } from "./identity";
import { matchDashboardRoute } from "./routes";
import { relayWatch } from "./sse-relay";

const DASHBOARD_AGENT = "dashboard";

type DashboardEnv = Pick<
  Env,
  "ACCESS_TEAM_DOMAIN" | "ACCESS_AUD" | "DASHBOARD_VIEWERS" | "IDENTITY_SIGNING_KEY" | "COORDINATOR"
>;

function plain(status: number, message: string): Response {
  return new Response(message, { status, headers: { "Cache-Control": "no-store" } });
}

async function proxySummary(env: DashboardEnv, repo: string): Promise<Response> {
  const upstream = await fetchSummary(env, repo, DASHBOARD_AGENT);
  if (!upstream.ok) {
    return plain(502, `the coordinator answered ${upstream.status} for the summary`);
  }
  return new Response(upstream.body, {
    headers: { "Content-Type": "application/json", "Cache-Control": "no-store" },
  });
}

function resumeSeq(request: Request): number {
  const header = request.headers.get("Last-Event-ID") ?? "";
  if (!/^\d+$/.test(header)) {
    return 0;
  }
  const last = Number(header);
  return Number.isSafeInteger(last) ? last + 1 : 0;
}

function events(env: DashboardEnv, request: Request, repo: string): Response {
  return new Response(
    relayWatch(() => openCoordinatorSocket(env, repo, DASHBOARD_AGENT), resumeSeq(request)),
    {
      headers: {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-store",
        "X-Accel-Buffering": "no",
      },
    },
  );
}

/**
 * Serves `/dashboard/<repo>` (page), `/events` (SSE relay of the coordinator `Watch`) and
 * `/summary` (proxy of the coordinator's summary). Every path first passes the Cloudflare Access
 * check: 503 when sign-in is not configured, 401 without a valid token, 403 for an email that is
 * not a listed viewer.
 */
export async function handleDashboard(
  request: Request,
  env: DashboardEnv,
  deps: AccessDeps = {},
): Promise<Response> {
  const config = readAccessConfig(env);
  if (config === undefined) {
    return plain(503, "sign-in not configured");
  }
  const verdict = await verifyAccessToken(
    request.headers.get("Cf-Access-Jwt-Assertion"),
    config,
    deps,
  );
  if (!verdict.ok) {
    return plain(verdict.status, verdict.reason);
  }
  const route = matchDashboardRoute(new URL(request.url).pathname);
  if (route === undefined) {
    return plain(
      404,
      "expected /dashboard/<repo>, /dashboard/<repo>/events or /dashboard/<repo>/summary",
    );
  }
  if (request.method !== "GET") {
    return plain(405, "the dashboard is read-only: use GET");
  }
  if (!isValidName(route.repo)) {
    return plain(400, INVALID_NAME_MESSAGE);
  }
  switch (route.kind) {
    case "page":
      return dashboardPage(crypto.randomUUID());
    case "events":
      return events(env, request, route.repo);
    case "summary":
      return proxySummary(env, route.repo);
  }
}
