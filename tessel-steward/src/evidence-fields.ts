import { parseScopes, type ClaimedNode } from "./merge-coverage";

/**
 * What the coordinator tells the steward about a merge so its evidence record needs no log scan:
 * the claim, its intent, the submission's decision record and its review. Field names and shapes
 * are `MergeRecord` in `src/merge.rs` (a Rust test and `evidence-fields.test.ts` pin the same
 * body). Every string in them was written by an agent or a reviewer: untrusted data, quoted in
 * the record and never followed (CLAUDE.md rule 4).
 */

/** An assumption the claim declared (`Assumption` in `src/protocol.rs`). */
export interface RecordAssumption {
  scope: ClaimedNode;
  statement: string;
}

/** Why the agent did the work (`Intent` in `src/protocol.rs`). */
export interface RecordIntent {
  summary: string;
  taskRef: string | null;
  assumptions: RecordAssumption[];
}

/** Where the agent's redacted transcript is (`TranscriptRef` in `src/protocol.rs`). */
export interface RecordTranscript {
  repo: string;
  path: string;
  sha256: string;
}

/** What the submission said about its work (`DecisionRecord` in `src/protocol.rs`). */
export interface RecordDecisions {
  rejected: Array<{ approach: string; reason: string }>;
  evidence: string[];
  transcript: RecordTranscript | null;
}

/** A reviewer's decision on a held submission. */
export interface RecordReviewDecision {
  reviewer: string;
  approve: boolean;
  note: string | null;
}

/**
 * Why the submission was held for review, and what the reviewer decided. `reasons` are
 * `ReviewReason` objects as the coordinator serialises them.
 */
export interface RecordReview {
  reasons: Array<Record<string, unknown>>;
  decision: RecordReviewDecision | null;
}

/** The record fields a merge request may carry. */
export const RECORD_FIELDS = ["claim", "intent", "decisions", "review"] as const;
export type RecordField = (typeof RECORD_FIELDS)[number];

/**
 * The record fields of one merge request. A field the coordinator did not send is undefined (an
 * older coordinator sends none). `review: null` means the submission was not held. A field that
 * was sent but is not the expected shape is undefined and named in `malformed`: the merge is never
 * refused over its record.
 */
export interface MergeRecordFields {
  claim?: number;
  intent?: RecordIntent;
  decisions?: RecordDecisions;
  review?: RecordReview | null;
  malformed: RecordField[];
}

type Parsed<T> = { ok: true; value: T } | { ok: false };

const MALFORMED = { ok: false } as const;

function ok<T>(value: T): Parsed<T> {
  return { ok: true, value };
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isString(value: unknown): value is string {
  return typeof value === "string";
}

function stringArray(value: unknown): Parsed<string[]> {
  if (value === undefined) {
    return ok([]);
  }
  return Array.isArray(value) && value.every(isString) ? ok(value) : MALFORMED;
}

function parseClaim(value: unknown): Parsed<number> {
  return Number.isSafeInteger(value) && (value as number) >= 0 ? ok(value as number) : MALFORMED;
}

function parseAssumption(value: unknown): RecordAssumption | undefined {
  if (!isObject(value) || !isString(value["statement"])) {
    return undefined;
  }
  const parsed = parseScopes([{ scope: value["scope"], mode: "depend" }]);
  const node = parsed.ok ? parsed.scopes[0]?.scope : undefined;
  return node === undefined ? undefined : { scope: node, statement: value["statement"] };
}

function parseIntent(value: unknown): Parsed<RecordIntent> {
  if (!isObject(value) || !isString(value["summary"])) {
    return MALFORMED;
  }
  const taskRef = value["task_ref"] ?? null;
  const raw = value["assumptions"] ?? [];
  if ((taskRef !== null && !isString(taskRef)) || !Array.isArray(raw)) {
    return MALFORMED;
  }
  const assumptions = raw.map(parseAssumption);
  if (assumptions.some((assumption) => assumption === undefined)) {
    return MALFORMED;
  }
  return ok({ summary: value["summary"], taskRef, assumptions: assumptions as RecordAssumption[] });
}

function parseTranscript(value: unknown): Parsed<RecordTranscript | null> {
  if (value === undefined || value === null) {
    return ok(null);
  }
  if (!isObject(value)) {
    return MALFORMED;
  }
  const { repo, path, sha256 } = value;
  return isString(repo) && isString(path) && isString(sha256)
    ? ok({ repo, path, sha256 })
    : MALFORMED;
}

function parseRejected(value: unknown): Parsed<RecordDecisions["rejected"]> {
  const raw = value ?? [];
  if (!Array.isArray(raw)) {
    return MALFORMED;
  }
  const rejected: RecordDecisions["rejected"] = [];
  for (const item of raw) {
    if (!isObject(item) || !isString(item["approach"]) || !isString(item["reason"])) {
      return MALFORMED;
    }
    rejected.push({ approach: item["approach"], reason: item["reason"] });
  }
  return ok(rejected);
}

function parseDecisions(value: unknown): Parsed<RecordDecisions> {
  if (!isObject(value)) {
    return MALFORMED;
  }
  const rejected = parseRejected(value["rejected"]);
  const evidence = stringArray(value["evidence"]);
  const transcript = parseTranscript(value["transcript"]);
  if (!rejected.ok || !evidence.ok || !transcript.ok) {
    return MALFORMED;
  }
  return ok({ rejected: rejected.value, evidence: evidence.value, transcript: transcript.value });
}

function parseDecision(value: unknown): Parsed<RecordReviewDecision | null> {
  if (value === undefined || value === null) {
    return ok(null);
  }
  if (!isObject(value)) {
    return MALFORMED;
  }
  const { reviewer, approve } = value;
  const note = value["note"] ?? null;
  if (!isString(reviewer) || typeof approve !== "boolean" || (note !== null && !isString(note))) {
    return MALFORMED;
  }
  return ok({ reviewer, approve, note });
}

function parseReview(value: unknown): Parsed<RecordReview | null> {
  if (value === null) {
    return ok(null);
  }
  if (!isObject(value) || !Array.isArray(value["reasons"]) || !value["reasons"].every(isObject)) {
    return MALFORMED;
  }
  const decision = parseDecision(value["decision"]);
  return decision.ok ? ok({ reasons: value["reasons"], decision: decision.value }) : MALFORMED;
}

const PARSERS: { [K in RecordField]: (value: unknown) => Parsed<unknown> } = {
  claim: parseClaim,
  intent: parseIntent,
  decisions: parseDecisions,
  review: parseReview,
};

/**
 * Reads the optional record fields of a merge request body. Never fails: what is missing stays
 * undefined, what is malformed is undefined and listed in `malformed`.
 */
export function parseRecordFields(body: Record<string, unknown>): MergeRecordFields {
  const fields: Record<string, unknown> = {};
  const malformed: RecordField[] = [];
  for (const name of RECORD_FIELDS) {
    if (body[name] === undefined) {
      continue;
    }
    const parsed = PARSERS[name](body[name]);
    if (parsed.ok) {
      fields[name] = parsed.value;
    } else {
      malformed.push(name);
    }
  }
  return { ...(fields as Omit<MergeRecordFields, "malformed">), malformed };
}
