import { describe, expect, it } from "vitest";

import { matchRoute } from "./routes";

describe("matchRoute", () => {
  it.each([
    ["/repos/demo", { kind: "create", repo: "demo" }],
    ["/repos/demo/tokens", { kind: "token", repo: "demo" }],
    ["/repos/demo/forks/demo--agent-1", { kind: "fork", repo: "demo", fork: "demo--agent-1" }],
    ["/repos/demo/test-runs", { kind: "test-run", repo: "demo" }],
    ["/repos/demo/test-runs/", { kind: "test-run", repo: "demo" }],
    ["/repos/demo/agents/a1/identity", { kind: "identity", repo: "demo", agent: "a1" }],
  ])("matches %s", (pathname, route) => {
    expect(matchRoute(pathname)).toEqual(route);
  });

  it.each([
    ["the root", "/"],
    ["another root segment", "/repo/demo/test-runs"],
    ["a missing repo", "/repos"],
    ["test-runs with a trailing segment", "/repos/demo/test-runs/1"],
    ["test-runs with a nested path", "/repos/demo/test-runs/1/log"],
    ["a misspelled action", "/repos/demo/test-run"],
    ["forks with a trailing segment", "/repos/demo/forks/x/y"],
    ["a fork without a name", "/repos/demo/forks"],
    ["tokens with a trailing segment", "/repos/demo/tokens/x"],
    ["identity with a trailing segment", "/repos/demo/agents/a1/identity/x"],
    ["identity without an agent", "/repos/demo/agents/identity"],
    ["agents with a misspelled leaf", "/repos/demo/agents/a1/identities"],
    ["agents without a leaf", "/repos/demo/agents/a1"],
    ["a leaf under tokens", "/repos/demo/tokens/x/identity"],
  ])("rejects %s", (_label, pathname) => {
    expect(matchRoute(pathname)).toBeUndefined();
  });
});
