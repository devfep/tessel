import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// "cloudflare:workers" only exists in the Workers runtime.
vi.mock("cloudflare:workers", () => ({
  WorkerEntrypoint: class {
    readonly ctx: unknown;
    constructor(ctx: unknown) {
      this.ctx = ctx;
    }
  },
}));

import { MergePushGateway, RecordPushGateway } from "./merge-gateway";

const REMOTE = "https://git.example/git/tessel/demo--evidence.git";
const ZERO = "0".repeat(40);
const NEW = "e".repeat(40);
const PROPS = { remote: REMOTE, ref: "refs/heads/main", old: ZERO, new: NEW, token: "art_v1_t" };

function pkt(text: string): string {
  return `${(text.length + 4).toString(16).padStart(4, "0")}${text}`;
}

function push(update: string, url = `${REMOTE}/git-receive-pack`): Request {
  const body = `${pkt(`${update}\0 report-status\n`)}0000PACK...`;
  return new Request(url, { method: "POST", body });
}

type Gateway = { fetch(request: Request): Promise<Response> };

function gateway(kind: typeof RecordPushGateway | typeof MergePushGateway): Gateway {
  const Construct = kind as unknown as new (ctx: unknown) => Gateway;
  return new Construct({ props: PROPS });
}

const forwarded: Request[] = [];

beforeEach(() => {
  vi.stubGlobal("fetch", async (request: Request) => {
    forwarded.push(request);
    return new Response("ok");
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  forwarded.splice(0);
});

describe("RecordPushGateway", () => {
  it("forwards the fetch of its record repo with the token", async () => {
    const record = gateway(RecordPushGateway);
    const discovery = new Request(`${REMOTE}/info/refs?service=git-upload-pack`);
    expect((await record.fetch(discovery)).status).toBe(200);
    expect(
      (await record.fetch(new Request(`${REMOTE}/git-upload-pack`, { method: "POST" }))).status,
    ).toBe(200);
    expect(forwarded.map((request) => request.headers.get("Authorization"))).toEqual([
      "Bearer art_v1_t",
      "Bearer art_v1_t",
    ]);
  });

  it("forwards exactly the computed update of main, creating it on an empty repo", async () => {
    const record = gateway(RecordPushGateway);
    expect((await record.fetch(push(`${ZERO} ${NEW} refs/heads/main`))).status).toBe(200);
  });

  it.each([
    ["another commit", `${ZERO} ${"f".repeat(40)} refs/heads/main`],
    ["another old value", `${"a".repeat(40)} ${NEW} refs/heads/main`],
    ["another ref", `${ZERO} ${NEW} refs/heads/other`],
  ])("refuses a push of %s", async (_label, update) => {
    expect((await gateway(RecordPushGateway).fetch(push(update))).status).toBe(403);
    expect(forwarded).toEqual([]);
  });

  it("refuses the fetch or push of any other repo", async () => {
    const other = "https://git.example/git/tessel/demo.git";
    const record = gateway(RecordPushGateway);
    expect(
      (await record.fetch(new Request(`${other}/info/refs?service=git-upload-pack`))).status,
    ).toBe(403);
    const update = `${ZERO} ${NEW} refs/heads/main`;
    expect((await record.fetch(push(update, `${other}/git-receive-pack`))).status).toBe(403);
    expect(forwarded).toEqual([]);
  });
});

describe("MergePushGateway", () => {
  it("still refuses a fetch: only the record gateway forwards one", async () => {
    const discovery = new Request(`${REMOTE}/info/refs?service=git-upload-pack`);
    expect((await gateway(MergePushGateway).fetch(discovery)).status).toBe(403);
  });
});
