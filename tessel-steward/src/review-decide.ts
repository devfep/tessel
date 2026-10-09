import { fetchSummary, openCoordinatorSocket, type LinkEnv } from "./coordinator-link";
import type { UpstreamSocket } from "./sse-relay";

/** How long a decision waits for the coordinator to log it before it reports "unknown". */
export const DECISION_WAIT_MS = 10_000;
/** A reviewer with no commit sends zeros, which the coordinator never takes as the head. */
const NO_COMMIT = "0".repeat(40);
const PROTOCOL_VERSION = 1;
const REVIEW_REQ = 1;

/**
 * What became of one `Review`. `decided` is reported only after the coordinator's event log
 * holds the matching `review_decided` (so `approve` is the logged value). `refused` carries the
 * coordinator's own error. `unknown` means the answer did not arrive in time or the socket
 * closed first: the decision may or may not have been recorded.
 */
export type DecisionOutcome =
  | { outcome: "decided"; approve: boolean; seq: number }
  | { outcome: "refused"; code: string; message: string }
  | { outcome: "unknown"; reason: string };

export interface ReviewRequest {
  claim: number;
  approve: boolean;
  /** Already carries the prefix that names the review UI. */
  note: string;
}

function parse(data: unknown): Record<string, unknown> | undefined {
  if (typeof data !== "string") {
    return undefined;
  }
  try {
    const value: unknown = JSON.parse(data);
    return typeof value === "object" && value !== null && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : undefined;
  } catch {
    return undefined;
  }
}

/** The first log position a decision made now can have: one past the log's last event. */
async function nextSeq(env: LinkEnv, repo: string, agent: string): Promise<number> {
  const response = await fetchSummary(env, repo, agent);
  if (!response.ok) {
    throw new Error(`the coordinator answered ${response.status} for the summary`);
  }
  const body = (await response.json()) as { head_seq?: unknown };
  return typeof body.head_seq === "number" ? body.head_seq + 1 : 0;
}

/** Reads one coordinator frame and returns the outcome it settles, if any. */
function settle(
  message: Record<string, unknown>,
  decision: { agent: string; claim: number; fromSeq: number; welcomed: boolean },
): DecisionOutcome | undefined {
  if (message["type"] === "error") {
    // Only an error that answers the Review, or one before the Review was sent, is a refusal.
    // Any other error says nothing about whether the decision was logged, so it is not reported.
    if (decision.welcomed && message["req"] !== REVIEW_REQ) {
      return undefined;
    }
    return {
      outcome: "refused",
      code: String(message["code"]),
      message: String(message["message"]),
    };
  }
  const event = message["event"] as Record<string, unknown> | null | undefined;
  const seq = event?.["seq"];
  if (
    message["type"] === "event" &&
    event?.["event"] === "review_decided" &&
    event["claim"] === decision.claim &&
    event["reviewer"] === decision.agent &&
    typeof seq === "number" &&
    seq >= decision.fromSeq
  ) {
    return { outcome: "decided", approve: event["approve"] === true, seq };
  }
  return undefined;
}

function converse(
  socket: UpstreamSocket,
  agent: string,
  request: ReviewRequest,
  fromSeq: number,
  waitMs: number,
): Promise<DecisionOutcome> {
  return new Promise((resolve) => {
    let done = false;
    let welcomed = false;
    const finish = (outcome: DecisionOutcome): void => {
      if (!done) {
        done = true;
        clearTimeout(timer);
        resolve(outcome);
      }
    };
    const timer = setTimeout(
      () => finish({ outcome: "unknown", reason: "the coordinator did not answer in time" }),
      waitMs,
    );
    const gone = (): void =>
      finish({ outcome: "unknown", reason: "the coordinator closed the connection" });
    socket.addEventListener("message", ({ data }) => {
      const message = parse(data);
      if (message === undefined) {
        return;
      }
      if (message["type"] === "welcome") {
        welcomed = true;
        socket.send(JSON.stringify({ type: "watch", from_seq: fromSeq }));
        socket.send(JSON.stringify({ type: "review", req: REVIEW_REQ, ...request }));
        return;
      }
      const outcome = settle(message, { agent, claim: request.claim, fromSeq, welcomed });
      if (outcome !== undefined) {
        finish(outcome);
      }
    });
    socket.addEventListener("close", gone);
    socket.addEventListener("error", gone);
    socket.send(
      JSON.stringify({ type: "hello", agent, base: NO_COMMIT, protocol: PROTOCOL_VERSION }),
    );
  });
}

/**
 * Sends one `Review` for `request.claim` as `agent` over a short-lived coordinator socket, then
 * waits up to `waitMs` for the coordinator's answer: its error, or the `review_decided` event in
 * its log. The identity token is signed here and never leaves the Worker.
 *
 * Each decision opens a socket that says `Hello`, so the coordinator logs one `agent_connected`
 * for the reviewer agent per decision. Closing the socket withdraws that agent's queued `Wait`
 * requests only when the agent has no other socket open.
 *
 * @param agent The reviewer agent the signed-in person maps to; it must be one of the repo's
 *   `REVIEWERS`, or the coordinator refuses.
 * @throws If the coordinator cannot be reached (before anything was sent).
 */
export async function sendReview(
  env: LinkEnv,
  repo: string,
  agent: string,
  request: ReviewRequest,
  waitMs: number = DECISION_WAIT_MS,
): Promise<DecisionOutcome> {
  const fromSeq = await nextSeq(env, repo, agent);
  const socket = await openCoordinatorSocket(env, repo, agent);
  try {
    return await converse(socket, agent, request, fromSeq, waitMs);
  } finally {
    socket.close();
  }
}
