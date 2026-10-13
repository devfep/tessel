import { recordCommand, type RecordCommand } from "./evidence-commands";
import type { MergeRecordFields } from "./evidence-fields";
import { hexDigest, planCommit, type FileWrite, type ObjectReader } from "./evidence-objects";
import {
  INDEX_PATH,
  indexLine,
  recordPath,
  renderRecord,
  type GateStep,
  type RecordInput,
  type TranscriptStatus,
} from "./evidence-record";
import type { ClaimedScope } from "./merge-coverage";
import type { EvidenceResult, GitResult, Sha } from "./merge-types";
import { redactTokens } from "./redact";

/**
 * The record step: after a merge landed, the steward writes what it knows about it into two
 * Artifacts repos of its own, `<repo>--evidence` and `<repo>--transcripts`. They are not forks, so
 * no agent can get a write token for them (`token-policy.ts`); only this step writes them.
 *
 * For each repo the Worker reads main through the Artifacts binding, computes the one commit it
 * will allow (`planCommit`), mints a write token, lets the sandbox push exactly that update with a
 * lease on the head it read, and revokes the token. Then it reads main again through the binding
 * and compares the SHA-256 of every file it wrote: the push's exit code is never trusted.
 */

/** Largest transcript the steward copies. */
export const TRANSCRIPT_MAX_BYTES = 4 * 1024 * 1024;

/** A record repo, read through the Artifacts binding. */
export interface RecordRepo extends ObjectReader {
  name: string;
  remote: string;
  /** The commit main points at, or null when main has no commit yet. */
  head(): Promise<Sha | null>;
  /** The bytes of `path` at `ref`, or null when there is no such file. */
  readFile(ref: string, path: string): Promise<Uint8Array | null>;
}

/** A file of the fork, found but not yet read. */
export interface ForkFile {
  size: number;
  bytes(): Promise<Uint8Array>;
}

/** The one update of main a record push may make. `old` is null for a repo with no commit. */
export interface RecordUpdate {
  old: Sha | null;
  new: Sha;
}

/** What the record step needs from the outside world. */
export interface EvidenceDeps {
  now(): Date;
  /** Opens the record repo `name`, creating it if it does not exist. Throws for a fork. */
  openRepo(name: string): Promise<RecordRepo>;
  /**
   * Finds `path` at `ref` of the merge's fork through the binding; null when missing. Its bytes
   * are read only on `bytes()`, so a file over the limit is refused by its size alone.
   */
  readForkFile(ref: string, path: string): Promise<ForkFile | null>;
  /**
   * Mints a write token for `repo`, lets the sandbox fetch `repo` and push exactly `update`, runs
   * `use`, and revokes the token when `use` ends, whether it returned or threw.
   */
  withRecordAccess<T>(repo: RecordRepo, update: RecordUpdate, use: () => Promise<T>): Promise<T>;
  /** Runs one command in the sandbox. */
  run(command: RecordCommand): Promise<GitResult>;
}

/** What the record of one landed merge is made of. */
export interface EvidenceInput {
  repo: string;
  fork: string;
  commit: Sha;
  scopes: readonly ClaimedScope[];
  fields: MergeRecordFields;
  base: Sha;
  head: Sha;
  gate: readonly GateStep[] | null;
}

type Committed = { ok: true; head: Sha } | { ok: false; reason: string };

const encoder = new TextEncoder();

/** The agent of a fork named `<repo>--<agent>`. */
export function agentOf(repo: string, fork: string): string {
  const prefix = `${repo}--`;
  return fork.startsWith(prefix) ? fork.slice(prefix.length) : fork;
}

function reasonOf(error: unknown): string {
  return redactTokens(error instanceof Error ? error.message : String(error));
}

async function sameBytes(repo: RecordRepo, file: FileWrite): Promise<boolean> {
  const read = await repo.readFile("main", file.path);
  if (read === null) {
    return false;
  }
  const [actual, expected] = await Promise.all([
    hexDigest("SHA-256", read),
    hexDigest("SHA-256", file.bytes),
  ]);
  return actual === expected;
}

/**
 * Writes `files` to main of `repo` as one commit on top of `old`, the head the caller read, then
 * checks through the binding that main is that commit and holds those bytes.
 */
async function commitFiles(
  deps: EvidenceDeps,
  repo: RecordRepo,
  old: Sha | null,
  files: readonly FileWrite[],
  commit: { seconds: number; message: string },
): Promise<Committed> {
  const planned = await planCommit(repo, old, files, commit);
  const pushed = await deps.withRecordAccess(repo, { old, new: planned.commit }, () =>
    deps.run(recordCommand(repo.remote, old, planned)),
  );
  const actual = await repo.head();
  if (actual !== planned.commit) {
    const now = actual ?? "unreadable";
    const exit = `push exit ${pushed.exitCode}`;
    return { ok: false, reason: `main of ${repo.name} is ${now}, not ${planned.commit} (${exit})` };
  }
  for (const file of files) {
    if (!(await sameBytes(repo, file))) {
      return { ok: false, reason: `${file.path} on main of ${repo.name} has other bytes` };
    }
  }
  return { ok: true, head: planned.commit };
}

function seconds(date: Date): number {
  return Math.floor(date.getTime() / 1000);
}

/** Reads the transcript from the fork and checks its hash; the bytes to copy, or why not. */
async function readTranscript(
  deps: EvidenceDeps,
  input: EvidenceInput,
): Promise<Uint8Array | TranscriptStatus> {
  const { claim, decisions } = input.fields;
  const transcript = decisions?.transcript ?? null;
  if (transcript === null) {
    return { kind: "none" };
  }
  if (claim === undefined) {
    return { kind: "refused", reason: "the coordinator sent no claim id" };
  }
  if (transcript.repo !== input.fork) {
    return { kind: "refused", reason: "the transcript is not on this merge's fork" };
  }
  if (transcript.path !== `transcripts/${claim}.md`) {
    return { kind: "refused", reason: `the transcript is not at transcripts/${claim}.md` };
  }
  const file = await deps.readForkFile(`tessel/transcript-${claim}`, transcript.path);
  if (file === null) {
    return { kind: "refused", reason: "the file could not be read from the fork" };
  }
  if (file.size > TRANSCRIPT_MAX_BYTES) {
    return { kind: "refused", reason: `larger than ${TRANSCRIPT_MAX_BYTES} bytes` };
  }
  const bytes = await file.bytes();
  if (bytes.length > TRANSCRIPT_MAX_BYTES) {
    return { kind: "refused", reason: `larger than ${TRANSCRIPT_MAX_BYTES} bytes` };
  }
  const actual = await hexDigest("SHA-256", bytes);
  return actual === transcript.sha256.toLowerCase()
    ? bytes
    : { kind: "refused", reason: "sha256 mismatch" };
}

/** Copies a verified transcript to `<agent>/<claim>.md` of `<repo>--transcripts`. */
async function copyTranscript(
  deps: EvidenceDeps,
  input: EvidenceInput,
  writtenAt: Date,
): Promise<TranscriptStatus> {
  try {
    const read = await readTranscript(deps, input);
    if (!(read instanceof Uint8Array)) {
      return read;
    }
    const agent = agentOf(input.repo, input.fork);
    const repo = await deps.openRepo(`${input.repo}--transcripts`);
    const path = `${agent}/${input.fields.claim}.md`;
    const message = `Copy the transcript of claim ${input.fields.claim} by ${agent}\n`;
    const files = [{ path, bytes: read }];
    const commit = { seconds: seconds(writtenAt), message };
    const copied = await commitFiles(deps, repo, await repo.head(), files, commit);
    if (!copied.ok) {
      return { kind: "refused", reason: `the copy was not written: ${copied.reason}` };
    }
    return { kind: "copied", repo: repo.name, path, sha256: await hexDigest("SHA-256", read) };
  } catch (error) {
    return { kind: "refused", reason: `the copy was not written: ${reasonOf(error)}` };
  }
}

async function writeRecord(deps: EvidenceDeps, input: EvidenceInput): Promise<EvidenceResult> {
  const writtenAt = deps.now();
  const transcript = await copyTranscript(deps, input, writtenAt);
  const record: RecordInput = {
    repo: input.repo,
    agent: agentOf(input.repo, input.fork),
    forkCommit: input.commit,
    base: input.base,
    head: input.head,
    writtenAt,
    scopes: input.scopes,
    gate: input.gate,
    fields: input.fields,
    transcript,
  };
  const repo = await deps.openRepo(`${input.repo}--evidence`);
  const parent = await repo.head();
  const index = parent === null ? null : await repo.readFile(parent, INDEX_PATH);
  const path = recordPath(input.head);
  const files: FileWrite[] = [
    { path: INDEX_PATH, bytes: concatText(index, indexLine(record)) },
    { path, bytes: encoder.encode(renderRecord(record)) },
  ];
  const message = `Record merge ${input.head} of ${input.repo}\n`;
  const commit = { seconds: seconds(writtenAt), message };
  const committed = await commitFiles(deps, repo, parent, files, commit);
  return committed.ok
    ? { written: true, repo: repo.name, path, head: committed.head }
    : { written: false, reason: committed.reason };
}

function concatText(before: Uint8Array | null, line: string): Uint8Array {
  const added = encoder.encode(line);
  if (before === null) {
    return added;
  }
  const out = new Uint8Array(before.length + added.length);
  out.set(before, 0);
  out.set(added, before.length);
  return out;
}

/**
 * Writes the record of a merge that landed: `merges/<head>.md` and a line of `merges/INDEX.md` in
 * `<repo>--evidence`, after copying the agent's transcript when the submission named one. Call it
 * only after the Worker read main at `head`. It never throws and never changes the merge: a
 * failure is logged and answered with `written: false` and the reason.
 */
export async function writeEvidence(
  deps: EvidenceDeps,
  input: EvidenceInput,
): Promise<EvidenceResult> {
  let result: EvidenceResult;
  try {
    result = await writeRecord(deps, input);
  } catch (error) {
    result = { written: false, reason: reasonOf(error) };
  }
  if (!result.written) {
    const reason = redactTokens(result.reason);
    console.error(JSON.stringify({ event: "evidence_not_written", repo: input.repo, reason }));
    return { written: false, reason };
  }
  return result;
}
