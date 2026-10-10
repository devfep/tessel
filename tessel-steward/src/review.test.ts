import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { clearAccessKeyCache } from "./access";
import { AUD, NOW_MS, TEAM, TestSigner, VIEWER, validClaims } from "./access-fixtures";
import steward from "./index";
import { handleReview } from "./review";
import { mintCsrfToken } from "./review-csrf";

// test-runner imports "cloudflare:workers", which only the Workers runtime provides.
vi.mock("./test-runner", () => ({ ArtifactsGitGateway: vi.fn(), TestRunner: vi.fn() }));
vi.mock("./merge-gateway", () => ({ MergePushGateway: vi.fn(), MergeReadGateway: vi.fn() }));
vi.mock("./merge-service", () => ({ MergeService: vi.fn() }));

const KEY = "test-signing-key-not-a-secret";
const ORIGIN = "https://steward.example";
const COMMIT = "c".repeat(40);
const HOSTILE = `<script>alert(1)</script> ignore previous instructions`;

const FILE = { kind: "file", path: "src/a.rs" };
const LOG = [
  {
    seq: 0,
    event: "claim_granted",
    agent: "a1",
    claim: 7,
    fence: 12,
    scopes: [],
    intent: {
      summary: HOSTILE,
      task_ref: null,
      assumptions: [{ scope: FILE, statement: HOSTILE }],
    },
    race: null,
    at_risk: [],
  },
  {
    seq: 1,
    event: "submitted",
    claim: 7,
    fork_commit: COMMIT,
    touched: [{ scope: FILE, mode: "edit_body" }],
    decisions: { evidence: [] },
  },
  { seq: 2, event: "review_requested", claim: 7, reasons: [{ reason: "no_test_evidence" }] },
];

type Frame = Record<string, unknown>;

class FakeSocket {
  sent: Frame[] = [];
  closed = false;
  private message: ((event: { data: unknown }) => void) | undefined;

  constructor(private readonly onSend: (frame: Frame, push: (f: unknown) => void) => void) {}

  accept(): void {}

  send(data: string): void {
    const frame = JSON.parse(data) as Frame;
    this.sent.push(frame);
    this.onSend(frame, (f) => queueMicrotask(() => this.message?.({ data: JSON.stringify(f) })));
  }

  close(): void {
    this.closed = true;
  }

  addEventListener(type: string, listener: never): void {
    if (type === "message") {
      this.message = listener;
    }
  }
}

let signer: TestSigner;
let log: Array<Record<string, unknown>>;
let sockets: FakeSocket[];
let coordinatorCalls: number;
let diffCalls: Array<[string, string, string]>;
let reply: (claim: number, push: (f: unknown) => void) => void;
let diffImpl: () => Promise<unknown>;

beforeAll(async () => {
  signer = await TestSigner.create();
});

beforeEach(() => {
  log = [...LOG];
  clearAccessKeyCache();
  sockets = [];
  coordinatorCalls = 0;
  diffCalls = [];
  diffImpl = () => Promise.resolve({ outcome: "ok", files: [], diff: "+x", truncated: false });
  reply = (claim, push) =>
    push({
      type: "event",
      event: {
        seq: 3,
        event: "review_decided",
        claim,
        approve: true,
        note: "n",
        reviewer: "felix",
      },
    });
});

function socketFor(): FakeSocket {
  const socket = new FakeSocket((frame, push) => {
    if (frame["type"] === "watch" && frame["from_seq"] === 0) {
      for (const event of log) {
        push({ type: "event", event });
      }
    } else if (frame["type"] === "hello") {
      push({ type: "welcome", head: "h", lease_ms: 1, protocol: 1 });
    } else if (frame["type"] === "review") {
      reply(frame["claim"] as number, push);
    }
  });
  sockets.push(socket);
  return socket;
}

function env(overrides: Record<string, unknown> = {}): Env {
  const coordinator = {
    fetch(_url: string, init: RequestInit) {
      coordinatorCalls += 1;
      if (new Headers(init.headers).get("Upgrade") === "websocket") {
        return Promise.resolve({ status: 101, webSocket: socketFor() } as unknown as Response);
      }
      return Promise.resolve(Response.json({ summary: {}, head_seq: log.at(-1)?.["seq"] }));
    },
  };
  return {
    ACCESS_TEAM_DOMAIN: TEAM,
    ACCESS_AUD: AUD,
    DASHBOARD_VIEWERS: `${VIEWER},viewer@example.com`,
    REVIEWER_EMAILS: `${VIEWER}=felix`,
    IDENTITY_SIGNING_KEY: KEY,
    COORDINATOR: coordinator,
    TEST_RUNNER: {
      getByName: () => ({
        diff: (...args: [string, string, string]) => {
          diffCalls.push(args);
          return diffImpl();
        },
      }),
    },
    ...overrides,
  } as unknown as Env;
}

interface Options {
  method?: string;
  email?: string;
  token?: string | null;
  headers?: Record<string, string>;
  body?: unknown;
  rawBody?: string;
}

async function call(path: string, options: Options = {}, environment: Env = env()) {
  const headers = new Headers(options.headers);
  const token =
    options.token === undefined
      ? await signer.sign(validClaims({ email: options.email ?? VIEWER }))
      : options.token;
  if (token !== null) {
    headers.set("Cf-Access-Jwt-Assertion", token);
  }
  const body =
    options.rawBody ?? (options.body === undefined ? undefined : JSON.stringify(options.body));
  const request = new Request(`${ORIGIN}${path}`, {
    method: options.method ?? "GET",
    headers,
    body: body ?? null,
  });
  return handleReview(request, environment, {
    fetchImpl: signer.certsFetch(),
    now: () => NOW_MS,
    decisionWaitMs: 40,
    logWaitMs: 200,
  });
}

async function decision(
  approve: boolean,
  overrides: Record<string, unknown> = {},
  o: Options = {},
) {
  const csrf = await mintCsrfToken(
    KEY,
    {
      email: VIEWER,
      repo: "demo",
      claim: 7,
      commit: COMMIT,
      action: approve ? "approve" : "reject",
    },
    Date.now(),
  );
  return call("/review/demo/7/decision", {
    method: "POST",
    headers: { "Sec-Fetch-Site": "same-origin", "Content-Type": "application/json" },
    body: { approve, note: "looks fine", commit: COMMIT, csrf, ...overrides },
    ...o,
  });
}

function reviews(): Frame[] {
  return sockets.flatMap((socket) => socket.sent).filter((frame) => frame["type"] === "review");
}

describe("sign-in", () => {
  const paths = [
    "/review/demo",
    "/review/demo/7/diff",
    "/review/demo/7/receipt",
    "/review/demo/7/decision",
  ];

  it.each(paths)(
    "answers 503 on %s until Access is configured, and reaches nothing",
    async (path) => {
      const response = await call(path, { method: "POST" }, env({ ACCESS_AUD: "" }));
      expect(response.status).toBe(503);
      expect(coordinatorCalls).toBe(0);
    },
  );

  it.each(paths)("answers 401 on %s without a token, and reaches nothing", async (path) => {
    expect((await call(path, { token: null, method: "POST" })).status).toBe(401);
    expect(coordinatorCalls).toBe(0);
    expect(diffCalls).toEqual([]);
  });

  it("answers 403 for an email that is not a listed viewer", async () => {
    expect((await call("/review/demo", { email: "eve@example.com" })).status).toBe(403);
    expect(coordinatorCalls).toBe(0);
  });

  it("is dispatched by the Worker, and refused there without a token", async () => {
    const response = await steward.fetch(new Request(`${ORIGIN}/review/demo`), env());
    expect(response.status).toBe(401);
  });

  it("rejects an invalid repo name and an unknown path", async () => {
    expect((await call("/review/-bad")).status).toBe(400);
    expect((await call("/review/demo/x/diff")).status).toBe(404);
  });
});

describe("the page", () => {
  it("shows a held claim with escaped agent text and a reject token for a reviewer", async () => {
    const response = await call("/review/demo");
    const html = await response.text();
    expect(response.status).toBe(200);
    expect(html).toContain("Claim 7");
    expect(html).toContain("&lt;script&gt;alert(1)&lt;/script&gt;");
    expect(html).not.toContain("<script>alert(1)");
    expect(html.match(/<script/g)).toHaveLength(1);
    expect(html).toContain("data-reject-csrf=");
    expect(html).not.toContain("data-approve");
    expect(sockets.flatMap((s) => s.sent).every((f) => f["type"] === "watch")).toBe(true);
  });

  it("shows a viewer who is not a reviewer the same submission without a token", async () => {
    const html = await (await call("/review/demo", { email: "viewer@example.com" })).text();
    expect(html).toContain("Claim 7");
    expect(html).not.toContain("data-reject-csrf");
  });

  it("gives nobody a token when REVIEWER_EMAILS is empty", async () => {
    const html = await (await call("/review/demo", {}, env({ REVIEWER_EMAILS: "" }))).text();
    expect(html).not.toContain("data-reject-csrf");
  });

  it("never starts a sandbox", async () => {
    await call("/review/demo");
    expect(diffCalls).toEqual([]);
  });

  it("answers 502 with a message when the log cannot be read", async () => {
    const broken = env({
      COORDINATOR: { fetch: () => Promise.resolve(new Response("x", { status: 500 })) },
    });
    const response = await call("/review/demo", {}, broken);
    expect(response.status).toBe(502);
    expect(await response.text()).toContain("500");
  });
});

describe("the diff", () => {
  it("runs the diff of the held claim's commit in the agent's fork, only when asked", async () => {
    const response = await call("/review/demo/7/diff");
    expect(response.status).toBe(200);
    expect(diffCalls).toEqual([["demo", "demo--a1", COMMIT]]);
  });

  it("refuses a claim that is not held, and starts no sandbox", async () => {
    expect((await call("/review/demo/8/diff")).status).toBe(404);
    expect(diffCalls).toEqual([]);
  });

  it("answers an error, never an empty diff, when the sandbox fails", async () => {
    diffImpl = () => Promise.reject(new Error("container did not start art_vSECRET123"));
    const response = await call("/review/demo/7/diff");
    const text = await response.text();
    expect(response.status).toBe(502);
    expect(text).toContain("container did not start");
    expect(text).not.toContain("SECRET123");
  });

  it("passes a diff-too-large outcome through with its flags", async () => {
    diffImpl = () =>
      Promise.resolve({
        outcome: "ok",
        files: [],
        diff: "",
        truncated: true,
        captureOverflow: true,
      });
    const body = (await (await call("/review/demo/7/diff")).json()) as Record<string, unknown>;
    expect(body).toMatchObject({ truncated: true, captureOverflow: true });
  });

  it("is GET only", async () => {
    expect((await call("/review/demo/7/diff", { method: "POST" })).status).toBe(405);
  });
});

describe("deciding", () => {
  it("sends exactly one Review for the claim as felix, the note naming the review UI", async () => {
    const response = await decision(true);
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ outcome: "decided", approve: true });
    expect(reviews()).toEqual([
      { type: "review", req: 1, claim: 7, approve: true, note: "felix via review UI: looks fine" },
    ]);
    const hello = sockets.flatMap((s) => s.sent).find((f) => f["type"] === "hello");
    expect(hello).toMatchObject({ agent: "felix" });
  });

  it("sends a rejection as approve false", async () => {
    reply = (claim, push) =>
      push({
        type: "event",
        event: {
          seq: 3,
          event: "review_decided",
          claim,
          approve: false,
          note: "",
          reviewer: "felix",
        },
      });
    const response = await decision(false, { note: "" });
    expect(await response.json()).toMatchObject({ outcome: "decided", approve: false });
    expect(reviews()).toMatchObject([{ approve: false, note: "felix via review UI:" }]);
  });

  it("shows the coordinator's refusal and does not call it decided", async () => {
    reply = (_claim, push) =>
      push({
        type: "error",
        req: 1,
        code: "not_awaiting_review",
        message: "claim 7 is not awaiting review",
      });
    const response = await decision(true);
    expect(response.status).toBe(409);
    expect(await response.json()).toMatchObject({
      outcome: "refused",
      code: "not_awaiting_review",
    });
  });

  it("reports unknown when the log never shows the decision", async () => {
    reply = () => {};
    const response = await decision(true);
    expect(response.status).toBe(202);
    expect(await response.json()).toMatchObject({ outcome: "unknown" });
  });

  it("answers 502 when the coordinator cannot be reached, with nothing sent", async () => {
    const down = env({ COORDINATOR: { fetch: () => Promise.reject(new Error("unreachable")) } });
    const csrf = await mintCsrfToken(
      KEY,
      { email: VIEWER, repo: "demo", claim: 7, commit: COMMIT, action: "approve" },
      Date.now(),
    );
    const response = await call(
      "/review/demo/7/decision",
      {
        method: "POST",
        headers: { "Sec-Fetch-Site": "same-origin", "Content-Type": "application/json" },
        body: { approve: true, commit: COMMIT, csrf },
      },
      down,
    );
    expect(response.status).toBe(502);
    expect(reviews()).toEqual([]);
  });
});

async function refused(
  expected: number,
  options: Options = {},
  overrides: Record<string, unknown> = {},
  environment?: Env,
) {
  const csrf = await mintCsrfToken(
    KEY,
    { email: VIEWER, repo: "demo", claim: 7, commit: COMMIT, action: "approve" },
    Date.now(),
  );
  const response = await call(
    "/review/demo/7/decision",
    {
      method: "POST",
      headers: { "Sec-Fetch-Site": "same-origin", "Content-Type": "application/json" },
      body: { approve: true, commit: COMMIT, csrf, ...overrides },
      ...options,
    },
    environment,
  );
  expect(response.status).toBe(expected);
  expect(reviews()).toEqual([]);
  expect(sockets).toEqual([]);
}

describe("forged and malformed decisions send nothing", () => {
  it.each(["cross-site", "same-site", "none"])("refuses Sec-Fetch-Site %s", (site) =>
    refused(403, {
      headers: { "Sec-Fetch-Site": site, "Content-Type": "application/json" },
    }),
  );

  it("refuses a request that proves nothing about where it came from", () =>
    refused(403, { headers: { "Content-Type": "application/json" } }));

  it("refuses another Origin when Sec-Fetch-Site is absent, and accepts the same one", async () => {
    await refused(403, {
      headers: { Origin: "https://evil.example", "Content-Type": "application/json" },
    });
    const csrf = await mintCsrfToken(
      KEY,
      { email: VIEWER, repo: "demo", claim: 7, commit: COMMIT, action: "approve" },
      Date.now(),
    );
    const ok = await call("/review/demo/7/decision", {
      method: "POST",
      headers: { Origin: ORIGIN, "Content-Type": "application/json" },
      body: { approve: true, commit: COMMIT, csrf },
    });
    expect(ok.status).toBe(200);
  });

  it("refuses a form-style content type", () =>
    refused(400, { headers: { "Sec-Fetch-Site": "same-origin", "Content-Type": "text/plain" } }));

  it("refuses a missing, foreign or expired decision token", async () => {
    await refused(403, {}, { csrf: undefined });
    await refused(403, {}, { csrf: "1.abc" });
    const other = await mintCsrfToken(
      KEY,
      { email: VIEWER, repo: "demo", claim: 8, commit: COMMIT, action: "approve" },
      Date.now(),
    );
    await refused(403, {}, { csrf: other });
    const old = await mintCsrfToken(
      KEY,
      { email: VIEWER, repo: "demo", claim: 7, commit: COMMIT, action: "approve" },
      1000,
    );
    await refused(403, {}, { csrf: old });
  });

  it("refuses a viewer who is not a reviewer, even with a token minted for them", async () => {
    const csrf = await mintCsrfToken(
      KEY,
      { email: "viewer@example.com", repo: "demo", claim: 7, commit: COMMIT, action: "approve" },
      Date.now(),
    );
    await refused(403, { email: "viewer@example.com" }, { csrf });
  });

  it("refuses everyone when REVIEWER_EMAILS is unset", () =>
    refused(403, {}, {}, env({ REVIEWER_EMAILS: undefined })));

  it.each([
    ["approve is not a boolean", { approve: "yes" }],
    ["the note is not a string", { note: 5 }],
    ["the note is too long", { note: "x".repeat(1100) }],
  ])("refuses when %s", (_name, overrides) => refused(400, {}, overrides));

  it("refuses a body that is not JSON", () =>
    refused(400, { body: undefined, rawBody: "approve=true" }));

  it("refuses a valid body over the byte cap that is under it in characters", async () => {
    const csrf = await mintCsrfToken(
      KEY,
      { email: VIEWER, repo: "demo", claim: 7, commit: COMMIT, action: "approve" },
      Date.now(),
    );
    const body = JSON.stringify({
      approve: true,
      commit: COMMIT,
      csrf,
      pad: "\u00e9".repeat(4500),
    });
    expect(body.length).toBeLessThan(8 * 1024);
    expect(new TextEncoder().encode(body).length).toBeGreaterThan(8 * 1024);
    await refused(413, { body: undefined, rawBody: body });
  });

  it("refuses a declared Content-Length over the cap without reading the body", () =>
    refused(413, {
      headers: {
        "Sec-Fetch-Site": "same-origin",
        "Content-Type": "application/json",
        "Content-Length": "100000",
      },
    }));

  it("is POST only", async () => {
    expect((await call("/review/demo/7/decision")).status).toBe(405);
  });
});

describe("a page left open", () => {
  const OTHER = "d".repeat(40);

  it("shows the commit it is for, and a token bound to it", async () => {
    const html = await (await call("/review/demo")).text();
    expect(html).toContain(`data-commit="${COMMIT}"`);
  });

  it("cannot approve a commit that replaced the one shown", async () => {
    log = [
      ...LOG,
      { seq: 3, event: "submit_rejected", claim: 7, reason: "x" },
      { ...LOG[1], seq: 4, fork_commit: OTHER },
      { ...LOG[2], seq: 5 },
    ];
    const response = await decision(true);
    expect(response.status).toBe(409);
    expect(await response.json()).toMatchObject({ error: "the submission changed: reload" });
    expect(reviews()).toEqual([]);
  });

  it("cannot approve a claim that is no longer held", async () => {
    log = [...LOG, { seq: 3, event: "merged", claim: 7, base: COMMIT, head: COMMIT }];
    expect((await decision(true)).status).toBe(409);
    expect(reviews()).toEqual([]);
  });

  it("refuses a token minted for another commit, even if that commit is held now", async () => {
    log = [
      ...LOG,
      { seq: 3, event: "submit_rejected", claim: 7, reason: "x" },
      { ...LOG[1], seq: 4, fork_commit: OTHER },
      { ...LOG[2], seq: 5 },
    ];
    expect((await decision(true, { commit: OTHER })).status).toBe(403);
    expect(reviews()).toEqual([]);
  });

  it("refuses a decision that names no commit", async () => {
    expect((await decision(true, { commit: undefined })).status).toBe(403);
    expect(reviews()).toEqual([]);
  });
});

describe("a viewer who is not a reviewer", () => {
  it("cannot start a diff, and sees no diff button", async () => {
    const viewer = { email: "viewer@example.com" };
    expect((await call("/review/demo/7/diff", viewer)).status).toBe(403);
    expect(diffCalls).toEqual([]);
    const html = await (await call("/review/demo", viewer)).text();
    expect(html).not.toContain('<button data-action="diff">');
  });
});

const OTHER_COMMIT = "d".repeat(40);
const OTHER_FILE = { kind: "file", path: "src/b.rs" };

function waitQueued(agent: string, seq: number, scope: unknown) {
  return {
    seq,
    event: "wait_queued",
    agent,
    req: seq,
    scopes: [{ scope, mode: "edit_body" }],
    intent: { summary: "w", task_ref: null, assumptions: [] },
    position: 1,
  };
}

async function pageToken(): Promise<string> {
  const html = await (await call("/review/demo")).text();
  return /data-reject-csrf="([^"]+)"/.exec(html)?.[1] ?? "";
}

async function diffToken(options: Options = {}): Promise<string | undefined> {
  const body = (await (await call("/review/demo/7/diff", options)).json()) as {
    approveToken?: string;
  };
  return body.approveToken;
}

describe("the approve token comes with the diff", () => {
  it("carries a token only in the diff response, and approving with it works", async () => {
    const token = await diffToken();
    expect(token).toEqual(expect.any(String));
    const response = await decision(true, { csrf: token });
    expect(response.status).toBe(200);
    expect(reviews()).toMatchObject([{ approve: true, claim: 7 }]);
  });

  it("refuses an approval made with the page's token", async () => {
    const response = await decision(true, { csrf: await pageToken() });
    expect(response.status).toBe(403);
    expect(reviews()).toEqual([]);
  });

  it("accepts a rejection made with the page's token", async () => {
    reply = (claim, push) =>
      push({
        type: "event",
        event: {
          seq: 3,
          event: "review_decided",
          claim,
          approve: false,
          note: "",
          reviewer: "felix",
        },
      });
    const response = await decision(false, { csrf: await pageToken() });
    expect(response.status).toBe(200);
    expect(reviews()).toMatchObject([{ approve: false }]);
  });

  it("refuses a rejection made with the diff's token", async () => {
    const response = await decision(false, { csrf: await diffToken() });
    expect(response.status).toBe(403);
    expect(reviews()).toEqual([]);
  });

  it("refuses the diff's token for another commit, even if that commit is held now", async () => {
    const token = await diffToken();
    log = [
      ...LOG,
      { seq: 3, event: "submit_rejected", claim: 7, reason: "x" },
      { ...LOG[1], seq: 4, fork_commit: OTHER_COMMIT },
      { ...LOG[2], seq: 5 },
    ];
    const response = await decision(true, { csrf: token, commit: OTHER_COMMIT });
    expect(response.status).toBe(403);
    expect(reviews()).toEqual([]);
  });

  it("gives a viewer who is not a reviewer no token", async () => {
    const response = await call("/review/demo/7/diff", { email: "viewer@example.com" });
    expect(response.status).toBe(403);
    expect(await response.text()).not.toContain("approveToken");
  });

  it("gives no token when the diff failed, so a failed diff cannot unlock approval", async () => {
    diffImpl = () => Promise.resolve({ outcome: "error", reason: "boom" });
    const response = await call("/review/demo/7/diff");
    expect(response.status).toBe(502);
    expect(await response.text()).not.toContain("approveToken");
  });
});

describe("the receipt", () => {
  const decided = { seq: 4, event: "review_decided", claim: 7, approve: true, note: null };

  it("shows a viewer nothing before the decision is logged", async () => {
    const response = await call("/review/demo/7/receipt", { email: "viewer@example.com" });
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      decided: null,
      closed: null,
      waiting: [],
      granted: [],
      complete: false,
    });
  });

  it("shows the decision, the merge and the grant to a waiting agent, each with its seq", async () => {
    log = [
      ...LOG,
      waitQueued("w1", 3, FILE),
      decided,
      { seq: 5, event: "merged", claim: 7, head: COMMIT },
      { seq: 6, event: "claim_granted", agent: "w1", claim: 9, fence: 1, scopes: [], intent: {} },
    ];
    const response = await call("/review/demo/7/receipt", { email: "viewer@example.com" });
    expect(await response.json()).toEqual({
      decided: { seq: 4, event: "review_decided", approve: true },
      closed: { seq: 5, event: "merged" },
      waiting: ["w1"],
      granted: [{ agent: "w1", seq: 6 }],
      complete: true,
    });
  });

  it("is GET only and starts no sandbox", async () => {
    expect((await call("/review/demo/7/receipt", { method: "POST" })).status).toBe(405);
    await call("/review/demo/7/receipt");
    expect(diffCalls).toEqual([]);
    expect(reviews()).toEqual([]);
  });
});

describe("the page orders the cards by who they unblock", () => {
  it("puts the claim with a waiting agent first, whatever its id", async () => {
    log = [
      ...LOG,
      {
        seq: 3,
        event: "claim_granted",
        agent: "a2",
        claim: 3,
        fence: 13,
        scopes: [],
        intent: { summary: "other", task_ref: null, assumptions: [] },
      },
      {
        seq: 4,
        event: "submitted",
        claim: 3,
        fork_commit: OTHER_COMMIT,
        touched: [{ scope: OTHER_FILE, mode: "edit_body" }],
        decisions: { evidence: [] },
      },
      { seq: 5, event: "review_requested", claim: 3, reasons: [{ reason: "no_test_evidence" }] },
      waitQueued("w1", 6, OTHER_FILE),
    ];
    const html = await (await call("/review/demo")).text();
    expect(html).toContain("2 waiting on you");
    expect(html).toContain("Claim 3 · a2 · unblocks 1 agent");
    expect(html).toContain("Claim 7 · a1 · unblocks nobody");
    expect(html.indexOf("Claim 3 ·")).toBeLessThan(html.indexOf("Claim 7 ·"));
    expect(html).toContain("1 of 2 in the queue, sorted by who it unblocks.");
  });
});
