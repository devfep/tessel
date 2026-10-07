import { readAccessConfig, verifyAccessToken, type AccessDeps } from "./access";
import { INVALID_NAME_MESSAGE, isValidName } from "./identity";
import { redactTokens } from "./redact";
import { parseSha } from "./merge-types";
import { sendReview, type DecisionOutcome } from "./review-decide";
import { mintCsrfToken, parseReviewerEmails, verifyCsrfToken } from "./review-csrf";
import { readEvents } from "./review-log";
import { reviewPage } from "./review-page";
import { foldHeld } from "./review-state";
import { matchReviewRoute, type ReviewRoute } from "./routes";

const VIEWER_AGENT = "dashboard";
/** The coordinator refuses a longer note (`MAX_REVIEW_NOTE_BYTES` in `src/coordinator.rs`). */
const MAX_NOTE_BYTES = 1024;
const MAX_BODY_BYTES = 8 * 1024;
const TOO_LARGE = "request body too large";

type ReviewEnv = Pick<
  Env,
  | "ACCESS_TEAM_DOMAIN"
  | "ACCESS_AUD"
  | "DASHBOARD_VIEWERS"
  | "REVIEWER_EMAILS"
  | "IDENTITY_SIGNING_KEY"
  | "COORDINATOR"
  | "TEST_RUNNER"
>;

export interface ReviewDeps extends AccessDeps {
  /** Overrides how long a decision waits for the log. For tests. */
  decisionWaitMs?: number;
  /** Overrides how long reading the log may take. For tests. */
  logWaitMs?: number;
}

interface Session {
  email: string;
  /** The reviewer agent this person decides as; `undefined` for a viewer who may not decide. */
  reviewer: string | undefined;
}

function reply(status: number, body: string, type = "text/plain; charset=utf-8"): Response {
  return new Response(body, {
    status,
    headers: { "Content-Type": type, "Cache-Control": "no-store" },
  });
}

function json(status: number, body: unknown): Response {
  return reply(status, JSON.stringify(body), "application/json");
}

/** An agent's Artifacts fork of `repo`: the coordinator's `fork_name` in `src/merge.rs`. */
function forkName(repo: string, agent: string): string {
  return `${repo}--${agent}`;
}

/**
 * Why a state-changing request is refused as possibly forged, or `undefined` when it came from
 * this site. A browser always sends `Sec-Fetch-Site`; one that does not must send a matching
 * `Origin`. A request with neither is refused.
 */
function forgeryReason(request: Request): string | undefined {
  const site = request.headers.get("Sec-Fetch-Site");
  if (site !== null) {
    return site === "same-origin" ? undefined : `cross-site request refused (${site})`;
  }
  const origin = request.headers.get("Origin");
  return origin === new URL(request.url).origin
    ? undefined
    : "request refused: no same-origin proof (Sec-Fetch-Site or Origin)";
}

async function loadHeld(env: ReviewEnv, repo: string, deps: ReviewDeps) {
  return foldHeld(await readEvents(env, repo, VIEWER_AGENT, deps.logWaitMs));
}

async function page(env: ReviewEnv, repo: string, session: Session, deps: ReviewDeps) {
  const held = await loadHeld(env, repo, deps);
  const csrfByClaim = new Map<number, string>();
  if (session.reviewer !== undefined) {
    for (const item of held) {
      const subject = {
        email: session.email,
        repo,
        claim: item.claim,
        commit: item.forkCommit,
      };
      csrfByClaim.set(
        item.claim,
        await mintCsrfToken(env.IDENTITY_SIGNING_KEY, subject, Date.now()),
      );
    }
  }
  return reviewPage({ repo, nonce: crypto.randomUUID(), held, csrfByClaim });
}

async function diff(
  env: ReviewEnv,
  route: { repo: string; claim: number },
  session: Session,
  deps: ReviewDeps,
) {
  const { repo, claim } = route;
  if (session.reviewer === undefined) {
    return json(403, {
      error: "your account is not a listed reviewer: each diff starts a sandbox",
    });
  }
  const item = (await loadHeld(env, repo, deps)).find((held) => held.claim === claim);
  const commit = parseSha(item?.forkCommit);
  if (item === undefined || commit === undefined) {
    return json(404, { error: `claim ${claim} is not held for review` });
  }
  const runner = env.TEST_RUNNER.getByName(crypto.randomUUID());
  const outcome = await runner.diff(repo, forkName(repo, item.agent), commit);
  return json(outcome.outcome === "ok" ? 200 : 502, outcome);
}

interface Decision {
  approve: boolean;
  note: string;
  /** The commit the page showed. */
  commit: unknown;
  csrf: unknown;
}

/** Reads the body, refusing more than `MAX_BODY_BYTES` bytes without buffering past that. */
async function readCapped(request: Request): Promise<string | undefined> {
  if (Number(request.headers.get("Content-Length") ?? "0") > MAX_BODY_BYTES) {
    return undefined;
  }
  if (request.body === null) {
    return "";
  }
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) {
      return new TextDecoder().decode(await new Blob(chunks).arrayBuffer());
    }
    total += value.length;
    if (total > MAX_BODY_BYTES) {
      await reader.cancel();
      return undefined;
    }
    chunks.push(value);
  }
}

async function readDecision(request: Request): Promise<Decision | string> {
  if (!(request.headers.get("Content-Type") ?? "").toLowerCase().startsWith("application/json")) {
    return "send Content-Type: application/json";
  }
  const text = await readCapped(request);
  if (text === undefined) {
    return TOO_LARGE;
  }
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch {
    return "request body is not JSON";
  }
  const { approve, note, commit, csrf } = (
    typeof body === "object" && body !== null ? body : {}
  ) as { approve?: unknown; note?: unknown; commit?: unknown; csrf?: unknown };
  if (typeof approve !== "boolean" || (note !== undefined && typeof note !== "string")) {
    return "expected {approve: boolean, note?: string, commit: string, csrf: string}";
  }
  return { approve, note: note ?? "", commit, csrf };
}

/** The note always starts with the path the decision took, so the log shows it. */
function composeNote(agent: string, note: string): string {
  const text = note.trim();
  return text === "" ? `${agent} via review UI:` : `${agent} via review UI: ${text}`;
}

function decisionResponse(outcome: DecisionOutcome): Response {
  switch (outcome.outcome) {
    case "decided":
      return json(200, outcome);
    case "refused":
      return json(409, { ...outcome, error: outcome.message });
    case "unknown":
      return json(202, outcome);
  }
}

/**
 * Checks that the person was shown what is held now: the token must be for this person, claim
 * and commit, and the claim must still be held at that commit. A page left open while the claim
 * was rejected and submitted again must not approve the new commit unseen.
 */
async function checkShown(
  env: ReviewEnv,
  route: { repo: string; claim: number },
  email: string,
  decision: Decision,
  deps: ReviewDeps,
): Promise<Response | undefined> {
  const { commit, csrf } = decision;
  const subject = { email, ...route, commit: typeof commit === "string" ? commit : "" };
  if (
    typeof commit !== "string" ||
    !(await verifyCsrfToken(env.IDENTITY_SIGNING_KEY, csrf, subject, Date.now()))
  ) {
    return json(403, { error: "missing or expired decision token: reload the page" });
  }
  const current = (await loadHeld(env, route.repo, deps)).find((h) => h.claim === route.claim);
  return current?.forkCommit === commit
    ? undefined
    : json(409, { error: "the submission changed: reload" });
}

async function decide(
  env: ReviewEnv,
  request: Request,
  route: { repo: string; claim: number },
  session: Session,
  deps: ReviewDeps,
): Promise<Response> {
  const forged = forgeryReason(request);
  if (forged !== undefined) {
    return json(403, { error: forged });
  }
  if (session.reviewer === undefined) {
    return json(403, { error: "your account is not a listed reviewer" });
  }
  const decision = await readDecision(request);
  if (typeof decision === "string") {
    return json(decision === TOO_LARGE ? 413 : 400, { error: decision });
  }
  const note = composeNote(session.reviewer, decision.note);
  if (new TextEncoder().encode(note).length > MAX_NOTE_BYTES) {
    return json(400, { error: `the note is longer than ${MAX_NOTE_BYTES} bytes` });
  }
  const refusal = await checkShown(env, route, session.email, decision, deps);
  if (refusal !== undefined) {
    return refusal;
  }
  const outcome = await sendReview(
    env,
    route.repo,
    session.reviewer,
    { claim: route.claim, approve: decision.approve, note },
    deps.decisionWaitMs,
  );
  return decisionResponse(outcome);
}

function dispatch(
  env: ReviewEnv,
  request: Request,
  route: ReviewRoute,
  session: Session,
  deps: ReviewDeps,
): Promise<Response> | Response {
  const wanted = route.kind === "decision" ? "POST" : "GET";
  if (request.method !== wanted) {
    return reply(405, `use ${wanted} for this path`);
  }
  switch (route.kind) {
    case "page":
      return page(env, route.repo, session, deps);
    case "diff":
      return diff(env, route, session, deps);
    case "decision":
      return decide(env, request, route, session, deps);
  }
}

/**
 * Serves `/review/<repo>` (the held submissions), `/review/<repo>/<claim>/diff` (the diff, started
 * on demand) and `POST /review/<repo>/<claim>/decision` (approve or reject). Every path first
 * passes the Cloudflare Access check: 503 when sign-in is not configured, 401 without a valid
 * token, 403 for an email that is not a listed viewer. The diff, which starts a sandbox, and
 * deciding also need a `REVIEWER_EMAILS` entry for the email; deciding also needs a same-origin
 * request and the page's decision token for the commit shown.
 *
 * Known limit: the page replays the whole log from seq 0 on every load under a 10 s timeout, so a
 * very large log will time out (502); a later change should fold from a summary instead.
 */
export async function handleReview(
  request: Request,
  env: ReviewEnv,
  deps: ReviewDeps = {},
): Promise<Response> {
  const config = readAccessConfig(env);
  if (config === undefined) {
    return reply(503, "sign-in not configured");
  }
  const verdict = await verifyAccessToken(
    request.headers.get("Cf-Access-Jwt-Assertion"),
    config,
    deps,
  );
  if (!verdict.ok) {
    return reply(verdict.status, verdict.reason);
  }
  const route = matchReviewRoute(new URL(request.url).pathname);
  if (route === undefined) {
    return reply(404, "expected /review/<repo>, /review/<repo>/<claim>/diff or .../decision");
  }
  if (!isValidName(route.repo)) {
    return reply(400, INVALID_NAME_MESSAGE);
  }
  const reviewer = parseReviewerEmails(env.REVIEWER_EMAILS).get(verdict.email.toLowerCase());
  try {
    return await dispatch(env, request, route, { email: verdict.email, reviewer }, deps);
  } catch (error) {
    const message = redactTokens(error instanceof Error ? error.message : String(error));
    console.error(JSON.stringify({ event: "review_failed", route: route.kind, message }));
    return json(502, { error: `the review screen could not finish: ${message}` });
  }
}
