import { STEWARD_COMMITTER_EMAIL, STEWARD_COMMITTER_NAME } from "./merge-commands";
import { parseSha, type Sha } from "./merge-types";

/**
 * The git objects of one record commit, computed by the Worker. The Worker knows every byte the
 * commit holds (the files it writes, the parent's trees read through the Artifacts binding, the
 * fixed identity and the time), so it knows the commit's id before the sandbox builds it. The push
 * gateway then lets exactly that id through: nothing the sandbox does can put another commit on
 * an evidence repo. The sandbox builds the same objects from `stream` with `git fast-import`.
 */

/** One entry of a tree as the Artifacts binding reads it (`ArtifactsTreeEntry`). */
export interface TreeEntry {
  name: string;
  mode: string;
  hash: string;
}

/** What building a commit reads from the repo: trees and commits by id. */
export interface ObjectReader {
  readTree(hash: Sha): Promise<TreeEntry[] | null>;
  readCommit(hash: Sha): Promise<{ treeHash: string } | null>;
}

/** One file of a record commit. `path` is `<dir>/<name>`, each a safe name (`SEGMENT_PATTERN`). */
export interface FileWrite {
  path: string;
  bytes: Uint8Array;
}

/** A commit the Worker has computed: its id, and the `git fast-import` input that builds it. */
export interface PlannedCommit {
  commit: Sha;
  stream: Uint8Array;
}

/** The ref `stream` writes in the sandbox's scratch repo. */
export const RECORD_REF = "refs/tessel/record";

const TREE_MODE = "40000";
const FILE_MODE = "100644";
const SEGMENT_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const encoder = new TextEncoder();

function concat(parts: Uint8Array[]): Uint8Array {
  const total = parts.reduce((sum, part) => sum + part.length, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
}

function hexToBytes(hex: string): Uint8Array {
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = Number.parseInt(hex.slice(2 * i, 2 * i + 2), 16);
  }
  return bytes;
}

/** Hex digest of `bytes` with `algorithm` ("SHA-1" for git ids, "SHA-256" for content hashes). */
export async function hexDigest(
  algorithm: "SHA-1" | "SHA-256",
  bytes: Uint8Array,
): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest(algorithm, bytes));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** The id git gives an object of `type` with this body. */
export async function gitObjectId(
  type: "blob" | "tree" | "commit",
  body: Uint8Array,
): Promise<Sha> {
  const header = encoder.encode(`${type} ${body.length}\0`);
  const id = parseSha(await hexDigest("SHA-1", concat([header, body])));
  if (id === undefined) {
    throw new Error("SHA-1 did not give 40 hex characters");
  }
  return id;
}

/** Git's tree order: bytewise on the name, a directory compared as if it ended in "/". */
function sortKey(entry: TreeEntry): Uint8Array {
  return encoder.encode(entry.mode === TREE_MODE ? `${entry.name}/` : entry.name);
}

function compareBytes(left: Uint8Array, right: Uint8Array): number {
  const length = Math.min(left.length, right.length);
  for (let i = 0; i < length; i++) {
    const difference = (left[i] ?? 0) - (right[i] ?? 0);
    if (difference !== 0) {
      return difference;
    }
  }
  return left.length - right.length;
}

/** The body of a tree object holding `entries`, in git's order. */
export function treeBody(entries: readonly TreeEntry[]): Uint8Array {
  const sorted = entries.toSorted((a, b) => compareBytes(sortKey(a), sortKey(b)));
  return concat(
    sorted.flatMap((entry) => [
      encoder.encode(`${entry.mode} ${entry.name}\0`),
      hexToBytes(entry.hash),
    ]),
  );
}

/** `Tessel Steward <steward@tessel.invalid> <seconds> +0000`: author and committer of a record. */
function identity(seconds: number): string {
  return `${STEWARD_COMMITTER_NAME} <${STEWARD_COMMITTER_EMAIL}> ${seconds} +0000`;
}

function commitBody(tree: Sha, parent: Sha | null, seconds: number, message: string): Uint8Array {
  const lines = [`tree ${tree}`];
  if (parent !== null) {
    lines.push(`parent ${parent}`);
  }
  lines.push(`author ${identity(seconds)}`, `committer ${identity(seconds)}`, "", message);
  return encoder.encode(lines.join("\n"));
}

function splitPath(path: string): [string, string] {
  const [dir, name, ...rest] = path.split("/");
  if (dir === undefined || name === undefined || rest.length > 0) {
    throw new Error(`a record path must be <dir>/<name>, not ${JSON.stringify(path)}`);
  }
  if (!SEGMENT_PATTERN.test(dir) || !SEGMENT_PATTERN.test(name)) {
    throw new Error(`unsafe record path ${JSON.stringify(path)}`);
  }
  return [dir, name];
}

/**
 * The entries of `tree`, with a directory's mode as git writes it in a tree object (`40000`,
 * which `ls-tree` prints as `040000`): the computed ids depend on those exact bytes.
 */
async function readEntries(reader: ObjectReader, tree: string): Promise<TreeEntry[]> {
  const hash = parseSha(tree);
  const entries = hash === undefined ? null : await reader.readTree(hash);
  if (entries === null) {
    throw new Error(`tree ${tree} could not be read`);
  }
  return entries.map(({ name, mode, hash: id }) => ({
    name,
    mode: mode === "040000" ? TREE_MODE : mode,
    hash: id,
  }));
}

async function rootTree(reader: ObjectReader, parent: Sha | null): Promise<TreeEntry[]> {
  if (parent === null) {
    return [];
  }
  const commit = await reader.readCommit(parent);
  if (commit === null) {
    throw new Error(`commit ${parent} could not be read`);
  }
  return readEntries(reader, commit.treeHash);
}

/** Replaces or adds the entry `name`, refusing to turn a directory into a file or back. */
function withEntry(entries: readonly TreeEntry[], entry: TreeEntry): TreeEntry[] {
  const existing = entries.find((candidate) => candidate.name === entry.name);
  if (existing !== undefined && (existing.mode === TREE_MODE) !== (entry.mode === TREE_MODE)) {
    throw new Error(`${entry.name} exists as another kind of entry`);
  }
  return [...entries.filter((candidate) => candidate.name !== entry.name), entry];
}

/** The id of the root tree after writing `files` on top of `root`. */
async function writeTrees(
  reader: ObjectReader,
  root: TreeEntry[],
  files: readonly FileWrite[],
): Promise<Sha> {
  const byDir = new Map<string, TreeEntry[]>();
  for (const file of files) {
    const [dir, name] = splitPath(file.path);
    const existing = root.find((entry) => entry.name === dir && entry.mode === TREE_MODE);
    const entries =
      byDir.get(dir) ?? (existing === undefined ? [] : await readEntries(reader, existing.hash));
    const blob = await gitObjectId("blob", file.bytes);
    byDir.set(dir, withEntry(entries, { name, mode: FILE_MODE, hash: blob }));
  }
  let entries = root;
  for (const [dir, children] of byDir) {
    const hash = await gitObjectId("tree", treeBody(children));
    entries = withEntry(entries, { name: dir, mode: TREE_MODE, hash });
  }
  return gitObjectId("tree", treeBody(entries));
}

function dataCommand(bytes: Uint8Array): Uint8Array[] {
  return [encoder.encode(`data ${bytes.length}\n`), bytes, encoder.encode("\n")];
}

function fastImportStream(
  parent: Sha | null,
  files: readonly FileWrite[],
  seconds: number,
  message: string,
): Uint8Array {
  const parts: Uint8Array[] = [
    encoder.encode(`commit ${RECORD_REF}\n`),
    encoder.encode(`author ${identity(seconds)}\ncommitter ${identity(seconds)}\n`),
    ...dataCommand(encoder.encode(message)),
  ];
  if (parent !== null) {
    parts.push(encoder.encode(`from ${parent}\n`));
  }
  for (const file of files) {
    parts.push(encoder.encode(`M ${FILE_MODE} inline ${file.path}\n`), ...dataCommand(file.bytes));
  }
  parts.push(encoder.encode("\n"));
  return concat(parts);
}

/**
 * Computes the commit that writes `files` on top of `parent` (null for a repo with no commits)
 * with the steward as author and committer at `seconds`, and the `git fast-import` input that
 * builds the same commit on `RECORD_REF`.
 *
 * @throws If a path is unsafe, a tree or commit of the parent cannot be read, or a path would turn
 *   a directory into a file or back.
 */
export async function planCommit(
  reader: ObjectReader,
  parent: Sha | null,
  files: readonly FileWrite[],
  commit: { seconds: number; message: string },
): Promise<PlannedCommit> {
  const tree = await writeTrees(reader, await rootTree(reader, parent), files);
  const body = commitBody(tree, parent, commit.seconds, commit.message);
  return {
    commit: await gitObjectId("commit", body),
    stream: fastImportStream(parent, files, commit.seconds, commit.message),
  };
}
