import { beforeEach, describe, expect, it } from "vitest";

import { PAGE_SCRIPT } from "./dashboard-page";

const EVIL = `<script>alert(1)</script><img src=x onerror=alert(2)>`;

class FakeNode {
  tagName: string;
  textContent = "";
  className = "";
  children: FakeNode[] = [];

  constructor(tagName: string) {
    this.tagName = tagName;
  }

  set innerHTML(value: string) {
    throw new Error(`innerHTML assigned: ${value}`);
  }

  append(...nodes: (FakeNode | string)[]): void {
    for (const node of nodes) {
      if (typeof node === "string") {
        const text = new FakeNode("#text");
        text.textContent = node;
        this.children.push(text);
      } else {
        this.children.push(node);
      }
    }
  }

  replaceChildren(...nodes: FakeNode[]): void {
    this.children = [];
    this.append(...nodes);
  }

  text(): string {
    return [this.textContent, ...this.children.map((child) => child.text())].join("");
  }
}

function statLines(page: Page): string[] {
  const stats = page.nodes.get("summary")?.children ?? [];
  return stats.map((stat) => stat.children.map((part) => part.text()).join(" "));
}

class FakeEventSource {
  static last: FakeEventSource;
  onmessage: ((message: { data: string }) => void) | undefined;
  onopen: (() => void) | undefined;
  onerror: (() => void) | undefined;
  listeners = new Map<string, (event: { data: string }) => void>();

  constructor(readonly url: string) {
    FakeEventSource.last = this;
  }

  addEventListener(type: string, listener: (event: { data: string }) => void): void {
    this.listeners.set(type, listener);
  }
}

interface Page {
  nodes: Map<string, FakeNode>;
  created: string[];
  fetched: string[];
  text(id: string): string;
  send(kind: string, fields?: Record<string, unknown>): void;
}

let summaryBody: unknown;

function scopeClaim(path: string, mode = "edit_body"): unknown {
  return { scope: { kind: "file", path }, mode };
}

function intent(summary: string, assumptions: unknown[] = []): unknown {
  return { summary, task_ref: null, assumptions };
}

async function load(): Promise<Page> {
  const nodes = new Map<string, FakeNode>();
  const created: string[] = [];
  const fetched: string[] = [];
  const document = {
    getElementById(id: string): FakeNode {
      const node = nodes.get(id) ?? new FakeNode("div");
      nodes.set(id, node);
      return node;
    },
    createElement(tag: string): FakeNode {
      created.push(tag);
      return new FakeNode(tag);
    },
  };
  const fakeFetch = (url: string): Promise<unknown> => {
    fetched.push(url);
    return Promise.resolve({ ok: true, json: () => Promise.resolve(summaryBody) });
  };
  const run = new Function(
    "document",
    "EventSource",
    "fetch",
    "location",
    "setTimeout",
    PAGE_SCRIPT,
  );
  run(
    document,
    FakeEventSource,
    fakeFetch,
    { pathname: "/dashboard/demo/" },
    (callback: () => void) => {
      callback();
      return 0;
    },
  );
  await Promise.resolve();
  let seq = 0;
  return {
    nodes,
    created,
    fetched,
    text: (id) => document.getElementById(id).text(),
    send(kind, fields = {}) {
      const event = { seq: seq++, at_ms: 1, run: "r", event: kind, ...fields };
      FakeEventSource.last.onmessage?.({ data: JSON.stringify(event) });
    },
  };
}

async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

beforeEach(() => {
  summaryBody = { summary: {}, head_seq: 0 };
});

describe("untrusted text", () => {
  it("shows a script in an intent as text and builds no script element", async () => {
    const page = await load();
    page.send("claim_granted", {
      agent: "a1",
      claim: 1,
      fence: 1,
      scopes: [scopeClaim(`src/${EVIL}.ts`)],
      intent: intent(EVIL),
      race: null,
      at_risk: [],
    });
    expect(page.text("claims")).toContain(EVIL);
    expect(page.created).not.toContain("script");
    expect(page.created).not.toContain("img");
  });

  it("shows the holder's intent in a denial as text", async () => {
    const page = await load();
    page.send("claim_denied", {
      agent: "a2",
      scopes: [scopeClaim("src/a.ts")],
      intent: intent("mine"),
      conflicts: [
        {
          requested: scopeClaim("src/a.ts"),
          held: scopeClaim("src/a.ts"),
          held_by: "a1",
          their_intent: intent(EVIL),
          race: null,
        },
      ],
    });
    expect(page.text("denials")).toContain(EVIL);
    expect(page.created).not.toContain("script");
  });

  it("never touches innerHTML or other markup parsers", () => {
    for (const forbidden of [
      "innerHTML",
      "outerHTML",
      "insertAdjacentHTML",
      "document.write",
      "eval(",
    ]) {
      expect(PAGE_SCRIPT).not.toContain(forbidden);
    }
  });
});

describe("denials", () => {
  it("labels a denial as a denial and never as a prevented conflict", async () => {
    const page = await load();
    page.send("claim_denied", {
      agent: "a2",
      scopes: [scopeClaim("src/a.ts")],
      intent: intent("mine"),
      conflicts: [
        {
          requested: scopeClaim("src/a.ts"),
          held: scopeClaim("src/a.ts", "edit_signature"),
          held_by: "a1",
          their_intent: intent("rename it"),
          race: null,
        },
      ],
    });
    const text = page.text("denials");
    expect(text).toContain("DENIED");
    expect(text).toContain("held by a1");
    expect(text).toContain("rename it");
    expect(text.toLowerCase()).not.toContain("prevented");
  });

  it("marks a shadowed denial as continuing in a shadow fork", async () => {
    const page = await load();
    page.send("claim_shadowed", {
      agent: "a2",
      claim: 5,
      scopes: [scopeClaim("src/a.ts")],
      conflicts: [],
    });
    expect(page.text("denials")).toContain("shadow fork");
  });

  it.each([
    ["clean", "false alarm"],
    ["textual_conflict", "conflict verified"],
    ["build_failed", "conflict verified"],
    ["tests_failed", "conflict verified"],
    ["inconclusive", "inconclusive (not counted)"],
    ["bogus", "unrecognized outcome (bogus)"],
  ])("shows a %s verification as: %s", async (outcome, label) => {
    const page = await load();
    page.send("denial_verified", { shadow_claim: 5, blocking_claim: 1, outcome });
    const text = page.text("verified");
    expect(text).toContain(label);
    expect(text).toContain(outcome);
    if (outcome === "bogus") {
      expect(text).not.toContain("conflict verified");
    }
  });
});

describe("headline numbers", () => {
  it("shows exactly what /summary says and counts nothing itself", async () => {
    summaryBody = {
      head_seq: 3,
      summary: {
        claims_granted: 99,
        denials: 7,
        conflicts_prevented_verified: 0,
        false_alarms: 4,
        precision: 0.125,
        merges: 11,
        reviews_requested: 2,
      },
    };
    const page = await load();
    page.send("denial_verified", {
      shadow_claim: 5,
      blocking_claim: 1,
      outcome: "textual_conflict",
    });
    page.send("claim_denied", { agent: "a", scopes: [], intent: intent("x"), conflicts: [] });
    await settle();
    expect(page.fetched).toContain("/dashboard/demo/summary");
    expect(statLines(page)).toEqual([
      "99 Claims granted",
      "7 Denials",
      "0 Conflicts prevented (verified)",
      "4 False alarms",
      "12.5% Precision",
      "11 Merges",
      "2 Reviews requested",
    ]);
  });

  it("renders an empty log, where head_seq is null", async () => {
    summaryBody = { head_seq: null, summary: { claims_granted: 0, precision: null } };
    const page = await load();
    await settle();
    expect(statLines(page)).toContain("0 Claims granted");
  });

  it("shows n/a for precision before anything is verified", async () => {
    summaryBody = { head_seq: 0, summary: { precision: null } };
    const page = await load();
    await settle();
    expect(statLines(page)).toContain("n/a Precision");
  });
});

describe("live state", () => {
  it("opens the event stream next to the page and shows the repo", async () => {
    const page = await load();
    expect(FakeEventSource.last.url).toBe("/dashboard/demo/events");
    expect(page.text("repo")).toBe("demo");
  });

  it("tracks a claim until it is released", async () => {
    const page = await load();
    page.send("claim_granted", {
      agent: "a1",
      claim: 1,
      fence: 1,
      scopes: [scopeClaim("src/a.ts")],
      intent: intent("fix a"),
      race: null,
      at_risk: [],
    });
    expect(page.text("claims")).toContain("a1 holds claim 1");
    page.send("claim_released", { claim: 1, reason: "merged" });
    expect(page.text("claims")).toContain("No active claims");
  });

  it("tracks waits until the agent is granted", async () => {
    const page = await load();
    page.send("wait_queued", {
      agent: "a2",
      req: 4,
      scopes: [scopeClaim("src/a.ts")],
      intent: intent("later"),
      position: 1,
    });
    expect(page.text("waits")).toContain("a2 waiting at position 1");
    page.send("claim_granted", {
      agent: "a2",
      claim: 2,
      fence: 1,
      scopes: [scopeClaim("src/a.ts")],
      intent: intent("later"),
      race: null,
      at_risk: [],
    });
    expect(page.text("waits")).toContain("Nobody is waiting");
  });

  it("shows a race opening and being decided", async () => {
    const page = await load();
    page.send("race_opened", { race: 3, scopes: [scopeClaim("src/a.ts")], criteria: [] });
    expect(page.text("races")).toContain("race 3 on edit_body src/a.ts: open");
    page.send("race_decided", { race: 3, winner: 8, ranking: [8] });
    expect(page.text("races")).toContain("decided: claim 8 won");
  });

  it("shows a review hold with its reasons until it is decided", async () => {
    const page = await load();
    page.send("review_requested", {
      claim: 6,
      reasons: [
        { reason: "signature_change", scope: { kind: "file", path: "src/a.ts" } },
        { reason: "threatens_assumptions", count: 2 },
        { reason: "no_test_evidence" },
      ],
    });
    const held = page.text("reviews");
    expect(held).toContain("claim 6 is held for a human");
    expect(held).toContain("signature_change: src/a.ts");
    expect(held).toContain("2 assumption(s)");
    page.send("review_decided", { claim: 6, approve: true, note: null });
    expect(page.text("reviews")).toContain("No submissions held");
  });

  it("lists merges with the agent that held the claim", async () => {
    const page = await load();
    page.send("claim_granted", {
      agent: "a1",
      claim: 1,
      fence: 1,
      scopes: [],
      intent: intent("x"),
      race: null,
      at_risk: [],
    });
    page.send("merged", { claim: 1, head: "abc123" });
    expect(page.text("merges")).toContain("claim 1 by a1 merged, head abc123");
  });

  it("shows connection state", async () => {
    const page = await load();
    FakeEventSource.last.onopen?.();
    expect(page.text("status")).toBe("Live");
    FakeEventSource.last.onerror?.();
    expect(page.text("status")).toBe("Reconnecting");
    FakeEventSource.last.listeners.get("upstream-error")?.({ data: '"too old"' });
    expect(page.text("status")).toContain("Coordinator");
  });
});
