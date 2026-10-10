import { describe, expect, it } from "vitest";

import { matchDashboardRoute, matchReviewRoute, matchRoute } from "./routes";

describe("matchRoute", () => {
  it.each([
    ["/repos/demo", { kind: "create", repo: "demo" }],
    ["/repos/demo/tokens", { kind: "token", repo: "demo" }],
    ["/repos/demo/read-tokens", { kind: "read-token", repo: "demo" }],
    ["/repos/demo/forks/demo--agent-1", { kind: "fork", repo: "demo", fork: "demo--agent-1" }],
    ["/repos/demo/test-runs", { kind: "test-run", repo: "demo" }],
    ["/repos/demo/test-runs/", { kind: "test-run", repo: "demo" }],
    ["/repos/demo/merges", { kind: "merge", repo: "demo" }],
    ["/repos/demo/agents/a1/identity", { kind: "identity", repo: "demo", agent: "a1" }],
  ])("matches %s", (pathname, route) => {
    expect(matchRoute(pathname)).toEqual(route);
  });

  it.each([
    ["the root", "/"],
    ["another root segment", "/repo/demo/test-runs"],
    ["a missing repo", "/repos"],
    ["merges with a trailing segment", "/repos/demo/merges/1"],
    ["merges naming a fork", "/repos/demo/merges/demo--a1"],
    ["test-runs with a trailing segment", "/repos/demo/test-runs/1"],
    ["test-runs with a nested path", "/repos/demo/test-runs/1/log"],
    ["a misspelled action", "/repos/demo/test-run"],
    ["forks with a trailing segment", "/repos/demo/forks/x/y"],
    ["a fork without a name", "/repos/demo/forks"],
    ["tokens with a trailing segment", "/repos/demo/tokens/x"],
    ["read-tokens with a trailing segment", "/repos/demo/read-tokens/x"],
    ["read-tokens naming a fork", "/repos/demo/read-tokens/demo--a1"],
    ["identity with a trailing segment", "/repos/demo/agents/a1/identity/x"],
    ["identity without an agent", "/repos/demo/agents/identity"],
    ["agents with a misspelled leaf", "/repos/demo/agents/a1/identities"],
    ["agents without a leaf", "/repos/demo/agents/a1"],
    ["a leaf under tokens", "/repos/demo/tokens/x/identity"],
  ])("rejects %s", (_label, pathname) => {
    expect(matchRoute(pathname)).toBeUndefined();
  });
});

describe("matchDashboardRoute", () => {
  it.each([
    ["/dashboard/demo", { kind: "page", repo: "demo" }],
    ["/dashboard/demo/", { kind: "page", repo: "demo" }],
    ["/dashboard/demo/events", { kind: "events", repo: "demo" }],
    ["/dashboard/demo/summary", { kind: "summary", repo: "demo" }],
  ])("matches %s", (pathname, route) => {
    expect(matchDashboardRoute(pathname)).toEqual(route);
  });

  it.each([
    ["the bare root", "/dashboard"],
    ["another root", "/repos/demo"],
    ["an unknown leaf", "/dashboard/demo/stream"],
    ["a nested path", "/dashboard/demo/events/1"],
  ])("rejects %s", (_label, pathname) => {
    expect(matchDashboardRoute(pathname)).toBeUndefined();
  });
});

describe("matchReviewRoute", () => {
  it.each([
    ["/review/demo", { kind: "page", repo: "demo" }],
    ["/review/demo/", { kind: "page", repo: "demo" }],
    ["/review/demo/12/diff", { kind: "diff", repo: "demo", claim: 12 }],
    ["/review/demo/0/decision", { kind: "decision", repo: "demo", claim: 0 }],
    ["/review/demo/3/receipt", { kind: "receipt", repo: "demo", claim: 3 }],
  ])("matches %s", (pathname, route) => {
    expect(matchReviewRoute(pathname)).toEqual(route);
  });

  it.each([
    ["the bare root", "/review"],
    ["another root", "/dashboard/demo"],
    ["a claim without a leaf", "/review/demo/12"],
    ["an unknown leaf", "/review/demo/12/approve"],
    ["a non-numeric claim", "/review/demo/abc/diff"],
    ["a signed claim", "/review/demo/-1/diff"],
    ["a claim past safe integers", "/review/demo/99999999999999999999/diff"],
    ["a nested path", "/review/demo/1/diff/x"],
  ])("rejects %s", (_label, pathname) => {
    expect(matchReviewRoute(pathname)).toBeUndefined();
  });
});
