/** One scope claim as the event log carries it. Paths and names are untrusted data. */
export interface ScopeClaimView {
  scope: { kind: string; path: string; qualified_name?: string };
  mode: string;
}

export type ScopeView = ScopeClaimView["scope"];

export interface AssumptionView {
  scope: ScopeView;
  statement: string;
}

/** An agent's Artifacts fork of `repo`: the coordinator's `fork_name` in `src/merge.rs`. */
export function forkName(repo: string, agent: string): string {
  return `${repo}--${agent}`;
}

/** A scope as one line of text: `path`, `dir/` or `path::qualified_name`. */
export function scopeText(scope: ScopeView): string {
  if (scope.kind === "symbol") {
    return `${scope.path}::${scope.qualified_name ?? ""}`;
  }
  return scope.kind === "dir" ? `${scope.path}/` : scope.path;
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
  /** `at_ms` of the `review_requested` event, or `null` when the log does not carry it. */
  requestedAtMs: number | null;
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
  const held = new Map<number, { reasons: Array<Record<string, unknown>>; atMs: number | null }>();
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
        held.set(claim, {
          reasons: list<Record<string, unknown>>(event["reasons"]),
          atMs: typeof event["at_ms"] === "number" ? event["at_ms"] : null,
        });
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
  for (const [claim, { reasons, atMs }] of held) {
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
      requestedAtMs: atMs,
    });
  }
  return out;
}

/** An agent queued behind the submission's scopes, as the log's `wait_queued` recorded it. */
export interface Waiter {
  agent: string;
  /** The queue position logged when the request was queued. */
  position: number;
  /** The first of the request's scopes that overlaps and conflicts in mode with a touched scope. */
  scope: ScopeView;
}

/** A live claim whose declared assumption names a scope the submission touches. */
export interface Assumer {
  agent: string;
  claim: number;
  scope: ScopeView;
  /** Untrusted text written by `agent`. */
  statement: string;
}

export interface Exposure {
  waiters: Waiter[];
  assumers: Assumer[];
}

function isScope(value: unknown): value is ScopeView {
  return isRecord(value) && typeof value["kind"] === "string" && typeof value["path"] === "string";
}

function dirHolds(dir: string, path: string): boolean {
  return dir === "" || path === dir || path.startsWith(`${dir}/`);
}

function scopeCovers(outer: ScopeView, inner: ScopeView): boolean {
  switch (outer.kind) {
    case "dir":
      return dirHolds(outer.path, inner.path);
    case "file":
      return (inner.kind === "file" || inner.kind === "symbol") && inner.path === outer.path;
    case "symbol":
      return (
        inner.kind === "symbol" &&
        inner.path === outer.path &&
        inner.qualified_name === outer.qualified_name
      );
    default:
      return false;
  }
}

/**
 * Whether two scopes can collide: one covers the other. A directory covers what lies under it, a
 * file covers its symbols, and a symbol covers only itself. Symmetric.
 */
export function scopesOverlap(a: ScopeView, b: ScopeView): boolean {
  return scopeCovers(a, b) || scopeCovers(b, a);
}

/** Mode pairs that do not conflict, written `a|b` in both orders. */
const COMPATIBLE_MODES = new Set([
  "depend|depend",
  "depend|edit_body",
  "edit_body|depend",
  "depend|create",
  "create|depend",
]);

/**
 * Whether claims in modes `a` and `b` on overlapping scopes conflict: `Mode::conflicts_with` in
 * `src/protocol.rs`. `depend` conflicts only with `edit_signature`; every other pair conflicts.
 * An unknown mode counts as conflicting, so a new mode is never silently hidden.
 */
function modesConflict(a: string, b: string): boolean {
  return !COMPATIBLE_MODES.has(`${a}|${b}`);
}

interface PendingWait {
  req: unknown;
  position: number;
  scopes: ScopeClaimView[];
}

interface LiveClaim {
  agent: string;
  assumptions: AssumptionView[];
}

function foldWaitState(events: readonly unknown[]) {
  const pending = new Map<string, PendingWait>();
  const live = new Map<number, LiveClaim>();
  for (const event of events) {
    if (!isRecord(event)) {
      continue;
    }
    const agent = text(event["agent"]);
    const claim = event["claim"];
    switch (event["event"]) {
      case "wait_queued":
        pending.set(agent, {
          req: event["req"],
          position: Number(event["position"]),
          scopes: list<ScopeClaimView | null>(event["scopes"]).filter((c): c is ScopeClaimView =>
            isScope(c?.scope),
          ),
        });
        break;
      case "wait_withdrawn":
        if (pending.get(agent)?.req === event["req"]) {
          pending.delete(agent);
        }
        break;
      case "claim_granted":
        pending.delete(agent);
        if (typeof claim === "number") {
          live.set(claim, { agent, assumptions: granted(event).assumptions });
        }
        break;
      case "claim_released":
      case "merged":
        if (typeof claim === "number") {
          live.delete(claim);
        }
        break;
      default:
        break;
    }
  }
  return { pending, live };
}

/**
 * Who is exposed to the submission of `claim`, folding `events` (pass a prefix to see the log as
 * it stood then). Waiters: agents with a `wait_queued` on a scope overlapping a touched scope and
 * no later `wait_withdrawn` for that request or `claim_granted` for that agent (an agent with a
 * queued request can make no other claim, so its next grant is that request's). Assumers: other
 * live claims (granted, and not since released, merged or rejected) with a declared assumption
 * on an overlapping scope. Waiters are in queue order, assumers in claim order.
 */
export function foldExposure(
  events: readonly unknown[],
  claim: number,
  touched: readonly ScopeClaimView[],
): Exposure {
  const touchedScopes = touched.map((t) => t.scope).filter(isScope);
  const overlapsTouched = (scope: ScopeView): boolean =>
    touchedScopes.some((t) => scopesOverlap(t, scope));
  const { pending, live } = foldWaitState(events);
  const waiters: Waiter[] = [];
  for (const [agent, wait] of pending) {
    const blocked = wait.scopes.find((claimed) =>
      touched.some(
        (t) =>
          isScope(t.scope) &&
          scopesOverlap(t.scope, claimed.scope) &&
          modesConflict(claimed.mode, t.mode),
      ),
    );
    if (blocked !== undefined && Number.isInteger(wait.position)) {
      waiters.push({ agent, position: wait.position, scope: blocked.scope });
    }
  }
  waiters.sort((a, b) => a.position - b.position);
  const assumers: Assumer[] = [];
  for (const [id, other] of [...live].toSorted(([a], [b]) => a - b)) {
    if (id === claim) {
      continue;
    }
    for (const assumption of other.assumptions) {
      if (isScope(assumption.scope) && overlapsTouched(assumption.scope)) {
        assumers.push({
          agent: other.agent,
          claim: id,
          scope: assumption.scope,
          statement: text(assumption.statement),
        });
      }
    }
  }
  return { waiters, assumers };
}

/** A held submission with who it unblocks and where it stands in the queue of decisions. */
export interface ReviewCard extends HeldSubmission, Exposure {
  /** The number of distinct agents queued behind it. */
  unblocks: number;
  /** 1-based place in the order shown. */
  place: number;
  total: number;
  /** Whole minutes from `review_requested` to the newest logged event, when both carry `at_ms`. */
  heldMinutes: number | null;
}

function newestAtMs(events: readonly unknown[]): number | null {
  let newest: number | null = null;
  for (const event of events) {
    const at = isRecord(event) ? event["at_ms"] : undefined;
    if (typeof at === "number" && (newest === null || at > newest)) {
      newest = at;
    }
  }
  return newest;
}

/** The held submissions, most agents unblocked first, then lowest claim id. */
export function buildCards(events: readonly unknown[]): ReviewCard[] {
  const newest = newestAtMs(events);
  const partial = foldHeld(events).map((held) => {
    const exposure = foldExposure(events, held.claim, held.touched);
    const heldMinutes =
      newest === null || held.requestedAtMs === null
        ? null
        : Math.max(0, Math.round((newest - held.requestedAtMs) / 60_000));
    return { ...held, ...exposure, unblocks: exposure.waiters.length, heldMinutes };
  });
  partial.sort((a, b) => b.unblocks - a.unblocks || a.claim - b.claim);
  return partial.map((card, index) => ({ ...card, place: index + 1, total: partial.length }));
}
