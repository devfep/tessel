/** One scope claim as the event log carries it. Paths and names are untrusted data. */
export interface ScopeClaimView {
  scope: { kind: string; path: string; qualified_name?: string };
  mode: string;
}

export interface AssumptionView {
  scope: ScopeClaimView["scope"];
  statement: string;
}

/** One submission waiting for a human (invariant 12). Every string is untrusted data. */
export interface HeldSubmission {
  claim: number;
  agent: string;
  fence: number;
  forkCommit: string;
  /** The `ReviewRequested` reasons exactly as logged. */
  reasons: Array<Record<string, unknown>>;
  intent: { summary: string; taskRef: string | null; assumptions: AssumptionView[] };
  touched: ScopeClaimView[];
  evidence: string[];
}

type LogEvent = Record<string, unknown>;

interface Granted {
  agent: string;
  fence: number;
  summary: string;
  taskRef: string | null;
  assumptions: AssumptionView[];
}

interface Submitted {
  forkCommit: string;
  touched: ScopeClaimView[];
  evidence: string[];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function list<T>(value: unknown): T[] {
  return Array.isArray(value) ? (value as T[]) : [];
}

function granted(event: LogEvent): Granted {
  const intent = isRecord(event["intent"]) ? event["intent"] : {};
  const taskRef = intent["task_ref"];
  return {
    agent: text(event["agent"]),
    fence: Number(event["fence"]),
    summary: text(intent["summary"]),
    taskRef: typeof taskRef === "string" ? taskRef : null,
    assumptions: list<AssumptionView>(intent["assumptions"]),
  };
}

function submitted(event: LogEvent): Submitted {
  const decisions = isRecord(event["decisions"]) ? event["decisions"] : {};
  return {
    forkCommit: text(event["fork_commit"]),
    touched: list<ScopeClaimView>(event["touched"]),
    evidence: list<unknown>(decisions["evidence"]).filter(
      (line): line is string => typeof line === "string",
    ),
  };
}

/**
 * Folds the event log into the submissions that are held for review right now: those with a
 * `review_requested` and no later `review_decided`, `submit_rejected`, `merged` or
 * `claim_released`. A claim submitted again after a rejection is held again only by a new
 * `review_requested`. Oldest first.
 *
 * A held claim whose grant or submission is missing from `events` (a log read from the middle)
 * is left out: the page would have nothing honest to show for it.
 */
export function foldHeld(events: readonly unknown[]): HeldSubmission[] {
  const grants = new Map<number, Granted>();
  const submissions = new Map<number, Submitted>();
  const held = new Map<number, Array<Record<string, unknown>>>();
  for (const event of events) {
    if (!isRecord(event) || typeof event["claim"] !== "number") {
      continue;
    }
    const claim = event["claim"];
    switch (event["event"]) {
      case "claim_granted":
        grants.set(claim, granted(event));
        break;
      case "claim_amended": {
        const grant = grants.get(claim);
        if (grant !== undefined) {
          grants.set(claim, { ...grant, fence: Number(event["fence"]) });
        }
        break;
      }
      case "submitted":
        submissions.set(claim, submitted(event));
        break;
      case "review_requested":
        held.set(claim, list<Record<string, unknown>>(event["reasons"]));
        break;
      case "review_decided":
      case "submit_rejected":
      case "merged":
      case "claim_released":
        held.delete(claim);
        break;
      default:
        break;
    }
  }
  const out: HeldSubmission[] = [];
  for (const [claim, reasons] of held) {
    const grant = grants.get(claim);
    const submission = submissions.get(claim);
    if (grant === undefined || submission === undefined) {
      continue;
    }
    out.push({
      claim,
      agent: grant.agent,
      fence: grant.fence,
      forkCommit: submission.forkCommit,
      reasons,
      intent: { summary: grant.summary, taskRef: grant.taskRef, assumptions: grant.assumptions },
      touched: submission.touched,
      evidence: submission.evidence,
    });
  }
  return out;
}
