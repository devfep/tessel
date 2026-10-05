import type { Sha } from "./merge-types";

const RECEIVE_PACK_SERVICE = "?service=git-receive-pack";
const PKT_LENGTH_BYTES = 4;
const COMMAND_PATTERN = /^([0-9a-f]{40}) ([0-9a-f]{40}) (\S+)$/;
const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false });

/** One ref update a `git push` asks for: `old` -> `new` on `ref`. */
export interface PushCommand {
  old: string;
  new: string;
  ref: string;
}

/**
 * Reads the command list at the start of a `git-receive-pack` request body (pkt-lines up to the
 * flush packet; the pack follows and is not read).
 *
 * @returns The commands, or undefined for anything that is not a plain command list: a bad
 *   length, a line that is not `<old> <new> <ref>`, a `shallow` or `push-cert` line, text that is
 *   not UTF-8, or no flush packet.
 */
export function parsePushCommands(body: Uint8Array): PushCommand[] | undefined {
  const commands: PushCommand[] = [];
  let offset = 0;
  while (offset + PKT_LENGTH_BYTES <= body.length) {
    const header = String.fromCharCode(...body.subarray(offset, offset + PKT_LENGTH_BYTES));
    if (!/^[0-9a-f]{4}$/.test(header)) {
      return undefined;
    }
    const length = Number.parseInt(header, 16);
    if (length === 0) {
      return commands.length > 0 ? commands : undefined;
    }
    if (length <= PKT_LENGTH_BYTES || offset + length > body.length) {
      return undefined;
    }
    const command = parseCommandLine(body.subarray(offset + PKT_LENGTH_BYTES, offset + length));
    if (command === undefined) {
      return undefined;
    }
    commands.push(command);
    offset += length;
  }
  return undefined;
}

function parseCommandLine(line: Uint8Array): PushCommand | undefined {
  let text: string;
  try {
    text = decoder.decode(line);
  } catch {
    return undefined;
  }
  const [command = "", ...capabilities] = text.split("\0");
  const match = COMMAND_PATTERN.exec(command.replace(/\n$/, ""));
  if (match === null || capabilities.length > 1) {
    return undefined;
  }
  const [, old = "", updated = "", ref = ""] = match;
  return { old, new: updated, ref };
}

/** Exactly the one ref update the merge executor is allowed to make. */
export interface ExpectedPush {
  remote: string;
  ref: string;
  old: Sha;
  new: Sha;
}

/** Whether the request line is the discovery request of a push to the one allowed repo. */
export function isReceivePackDiscovery(
  request: { method: string; url: string },
  expected: ExpectedPush,
): boolean {
  const remote = new URL(expected.remote);
  const target = new URL(request.url);
  return (
    request.method === "GET" &&
    target.protocol === "https:" &&
    target.host === remote.host &&
    target.pathname === `${remote.pathname}/info/refs` &&
    target.search === RECEIVE_PACK_SERVICE
  );
}

/** Whether the request line is the push itself to the one allowed repo. Check the body next. */
export function isReceivePackPost(
  request: { method: string; url: string },
  expected: ExpectedPush,
): boolean {
  const remote = new URL(expected.remote);
  const target = new URL(request.url);
  return (
    request.method === "POST" &&
    target.protocol === "https:" &&
    target.host === remote.host &&
    target.pathname === `${remote.pathname}/git-receive-pack` &&
    target.search === ""
  );
}

/** Whether the parsed commands are exactly the one expected update: no more, no other. */
export function isExpectedPush(commands: PushCommand[], expected: ExpectedPush): boolean {
  const [only, ...rest] = commands;
  return (
    only !== undefined &&
    rest.length === 0 &&
    only.ref === expected.ref &&
    only.old === expected.old &&
    only.new === expected.new
  );
}

/**
 * Reads a request body into memory, giving up as soon as it passes `limitBytes`.
 *
 * @returns The bytes, or undefined if the body is larger than the limit (the stream is cancelled).
 */
export async function readBodyCapped(
  body: ReadableStream<Uint8Array> | null,
  limitBytes: number,
): Promise<Uint8Array | undefined> {
  if (body === null) {
    return new Uint8Array();
  }
  const chunks: Uint8Array[] = [];
  let total = 0;
  const reader = body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) {
      break;
    }
    total += value.length;
    if (total > limitBytes) {
      await reader.cancel();
      return undefined;
    }
    chunks.push(value);
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return bytes;
}
