import { describe, expect, it } from "vitest";

import { parseSha, type Sha } from "./merge-types";
import {
  isExpectedPush,
  isReceivePackDiscovery,
  isReceivePackPost,
  parsePushCommands,
  readBodyCapped,
  type ExpectedPush,
} from "./receive-pack-policy";

function sha(character: string): Sha {
  const parsed = parseSha(character.repeat(40));
  if (parsed === undefined) {
    throw new Error("bad test sha");
  }
  return parsed;
}

const encoder = new TextEncoder();
const OLD = sha("a");
const NEW = sha("b");
const expected: ExpectedPush = {
  remote: "https://git.example/git/tessel/demo.git",
  ref: "refs/heads/main",
  old: OLD,
  new: NEW,
};

function pkt(text: string): string {
  return `${(text.length + 4).toString(16).padStart(4, "0")}${text}`;
}

function body(...lines: string[]): Uint8Array {
  return encoder.encode(`${lines.join("")}0000PACK...`);
}

const MAIN_UPDATE = pkt(`${OLD} ${NEW} refs/heads/main\0 report-status agent=git/2\n`);

describe("parsePushCommands", () => {
  it("reads the command line of a push, ignoring capabilities and the pack", () => {
    expect(parsePushCommands(body(MAIN_UPDATE))).toEqual([
      { old: OLD, new: NEW, ref: "refs/heads/main" },
    ]);
  });

  it("reads several commands", () => {
    const second = pkt(`${OLD} ${NEW} refs/heads/other\n`);
    expect(parsePushCommands(body(MAIN_UPDATE, second))).toHaveLength(2);
  });

  it.each([
    ["no flush packet", encoder.encode(MAIN_UPDATE)],
    ["only a flush packet", encoder.encode("0000")],
    ["a non-hex length", encoder.encode("zzzz")],
    ["a length past the end", encoder.encode("00ffabc")],
    ["a length under 5", encoder.encode("0003x0000")],
    ["a shallow line", body(pkt(`shallow ${OLD}\n`), MAIN_UPDATE)],
    ["a push certificate", body(pkt("push-cert\0 report-status\n"))],
    [
      "the push-options capability with option lines after the flush",
      encoder.encode(
        `${pkt(`${OLD} ${NEW} refs/heads/main\0 report-status push-options\n`)}0000${pkt("ci.skip\n")}0000PACK`,
      ),
    ],
    ["a short sha", body(pkt(`${OLD.slice(1)} ${NEW} refs/heads/main\0 x\n`))],
    ["invalid UTF-8", Uint8Array.from([0x30, 0x30, 0x30, 0x38, 0xff, 0xff, 0xff, 0xff])],
  ])("rejects %s", (_label, bytes) => {
    expect(parsePushCommands(bytes)).toBeUndefined();
  });
});

describe("isExpectedPush", () => {
  const command = { old: OLD, new: NEW, ref: "refs/heads/main" };

  it("accepts exactly the expected update", () => {
    expect(isExpectedPush([command], expected)).toBe(true);
  });

  it.each([
    ["another new sha", { ...command, new: sha("c") }],
    ["another old sha", { ...command, old: sha("c") }],
    ["creating the ref", { ...command, old: "0".repeat(40) }],
    ["deleting the ref", { ...command, new: "0".repeat(40) }],
    ["another ref", { ...command, ref: "refs/heads/other" }],
  ])("refuses %s", (_label, other) => {
    expect(isExpectedPush([other], expected)).toBe(false);
  });

  it("refuses the expected update when it is not the only one", () => {
    expect(isExpectedPush([command, { ...command, ref: "refs/heads/x" }], expected)).toBe(false);
    expect(isExpectedPush([], expected)).toBe(false);
  });
});

describe("request lines", () => {
  const base = `${expected.remote}`;

  it("accepts the discovery request and the push of the one repo", () => {
    expect(
      isReceivePackDiscovery(
        { method: "GET", url: `${base}/info/refs?service=git-receive-pack` },
        expected,
      ),
    ).toBe(true);
    expect(isReceivePackPost({ method: "POST", url: `${base}/git-receive-pack` }, expected)).toBe(
      true,
    );
  });

  it.each([
    ["a read", { method: "GET", url: `${base}/info/refs?service=git-upload-pack` }],
    [
      "another repo",
      {
        method: "GET",
        url: "https://git.example/git/tessel/x.git/info/refs?service=git-receive-pack",
      },
    ],
    [
      "another host",
      {
        method: "GET",
        url: "https://evil.example/git/tessel/demo.git/info/refs?service=git-receive-pack",
      },
    ],
    [
      "plain http",
      {
        method: "GET",
        url: "http://git.example/git/tessel/demo.git/info/refs?service=git-receive-pack",
      },
    ],
    ["a POST to discovery", { method: "POST", url: `${base}/info/refs?service=git-receive-pack` }],
  ])("refuses %s as a discovery request", (_label, request) => {
    expect(isReceivePackDiscovery(request, expected)).toBe(false);
  });

  it.each([
    ["a GET", { method: "GET", url: `${base}/git-receive-pack` }],
    ["upload-pack", { method: "POST", url: `${base}/git-upload-pack` }],
    ["a query", { method: "POST", url: `${base}/git-receive-pack?x=1` }],
    [
      "another repo",
      { method: "POST", url: "https://git.example/git/tessel/x.git/git-receive-pack" },
    ],
  ])("refuses %s as the push", (_label, request) => {
    expect(isReceivePackPost(request, expected)).toBe(false);
  });
});

function stream(...chunks: Uint8Array[]): ReadableStream<Uint8Array> {
  return new ReadableStream({
    start(controller) {
      for (const chunk of chunks) {
        controller.enqueue(chunk);
      }
      controller.close();
    },
  });
}

describe("readBodyCapped", () => {
  it("joins the chunks", async () => {
    const bytes = await readBodyCapped(stream(Uint8Array.of(1, 2), Uint8Array.of(3)), 10);
    expect(bytes).toEqual(Uint8Array.of(1, 2, 3));
  });

  it("accepts a body of exactly the limit", async () => {
    expect(await readBodyCapped(stream(Uint8Array.of(1, 2, 3)), 3)).toHaveLength(3);
  });

  it("gives up on a body over the limit", async () => {
    expect(
      await readBodyCapped(stream(Uint8Array.of(1, 2), Uint8Array.of(3, 4)), 3),
    ).toBeUndefined();
  });

  it("treats a missing body as empty", async () => {
    expect(await readBodyCapped(null, 3)).toEqual(new Uint8Array());
  });
});
