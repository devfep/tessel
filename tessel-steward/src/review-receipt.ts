import { foldExposure, type ScopeClaimView } from "./review-state";

/** An event the log holds, named by its `seq`. */
export interface Logged {
  seq: number;
  event: string;
}

export interface Receipt {
  /** The human's `review_decided` for the claim's current hold, once logged. */
  decided: (Logged & { approve: boolean }) | null;
  /** The `merged` or `submit_rejected` that followed the decision, once logged. */
  closed: Logged | null;
  /** Agents that were queued behind the claim's scopes when it was decided. */
  waiting: string[];
  /** Each waiting agent's `claim_granted` after `closed`, once logged. */
  granted: Array<{ agent: string; seq: number }>;
  /** Every line a decision can fill is filled: nothing more will appear. */
  complete: boolean;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

interface Hold {
  touched: ScopeClaimView[];
  decided: (Logged & { approve: boolean; index: number }) | null;
  closed: (Logged & { index: number }) | null;
}

function findHold(events: readonly unknown[], claim: number): Hold | undefined {
  let touched: ScopeClaimView[] = [];
  let hold: Hold | undefined;
  events.forEach((event, index) => {
    if (!isRecord(event) || event["claim"] !== claim || typeof event["seq"] !== "number") {
      return;
    }
    const seq = event["seq"];
    switch (event["event"]) {
      case "submitted":
        touched = Array.isArray(event["touched"]) ? (event["touched"] as ScopeClaimView[]) : [];
        break;
      case "review_requested":
        hold = { touched, decided: null, closed: null };
        break;
      case "review_decided":
        if (hold?.decided === null) {
          hold.decided = {
            seq,
            event: "review_decided",
            approve: event["approve"] === true,
            index,
          };
        }
        break;
      case "merged":
      case "submit_rejected":
        if (hold?.decided != null && hold.closed === null) {
          hold.closed = { seq, event: event["event"], index };
        }
        break;
      default:
        break;
    }
  });
  return hold;
}

function grantsAfter(
  events: readonly unknown[],
  from: number,
  waiting: ReadonlySet<string>,
): Array<{ agent: string; seq: number }> {
  const granted = new Map<string, number>();
  for (const event of events.slice(from + 1)) {
    if (!isRecord(event) || event["event"] !== "claim_granted") {
      continue;
    }
    const { agent, seq } = event;
    if (typeof agent === "string" && typeof seq === "number" && waiting.has(agent)) {
      if (!granted.has(agent)) {
        granted.set(agent, seq);
      }
    }
  }
  return [...granted].map(([agent, seq]) => ({ agent, seq }));
}

/**
 * What the log shows so far of what followed the human's decision on `claim`: its current hold's
 * `review_decided`, then `merged` or `submit_rejected`, then `claim_granted` for the agents that
 * were queued behind its scopes at the decision. A rejection grants nobody: the claim is active
 * again with its locks, so the receipt is complete at `submit_rejected`. Only logged events
 * appear, each with its `seq`; a line stays `null` until its event is in `events`. A claim held
 * again after a rejection starts over from its newest `review_requested`.
 */
export function foldReceipt(events: readonly unknown[], claim: number): Receipt {
  const hold = findHold(events, claim);
  if (hold?.decided == null) {
    return { decided: null, closed: null, waiting: [], granted: [], complete: false };
  }
  const { index, ...decided } = hold.decided;
  const waiting = foldExposure(events.slice(0, index + 1), claim, hold.touched).waiters.map(
    (waiter) => waiter.agent,
  );
  if (hold.closed === null) {
    return { decided, closed: null, waiting, granted: [], complete: false };
  }
  const { index: closedIndex, ...closed } = hold.closed;
  if (closed.event === "submit_rejected") {
    return { decided, closed, waiting, granted: [], complete: true };
  }
  const granted = grantsAfter(events, closedIndex, new Set(waiting));
  return { decided, closed, waiting, granted, complete: granted.length === waiting.length };
}
