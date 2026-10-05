import { describe, expect, it } from "vitest";

import { isForkOf, isSafeBranchName, parseMergeRequest, parseSha } from "./merge-types";

const SHA = "0123456789abcdef0123456789abcdef01234567";
const SCOPES = [{ scope: { kind: "dir", path: "" }, mode: "edit_body" }] as const;

describe("parseSha", () => {
  it("accepts 40 lowercase hex characters", () => {
    expect(parseSha(SHA)).toBe(SHA);
  });

  it.each([
    ["uppercase", SHA.toUpperCase()],
    ["39 characters", SHA.slice(1)],
    ["41 characters", `${SHA}0`],
    ["a trailing newline", `${SHA}\n`],
    ["an option-looking value", `--upload-pack=${SHA}`],
    ["a shell metacharacter", `${SHA.slice(0, 39)};`],
    ["a ref name", "main"],
    ["empty", ""],
    ["a number", 123],
    ["null", null],
  ])("rejects %s", (_label, value) => {
    expect(parseSha(value)).toBeUndefined();
  });
});

describe("parseMergeRequest", () => {
  it("accepts a fork name and a sha", () => {
    expect(parseMergeRequest({ fork: "demo--agent-1", commit: SHA, scopes: SCOPES })).toEqual({
      ok: true,
      request: { fork: "demo--agent-1", commit: SHA, scopes: SCOPES },
    });
  });

  it.each([
    ["a missing body", null],
    ["a string body", "x"],
    ["a missing fork", { commit: SHA, scopes: SCOPES }],
    ["a path as the fork", { fork: "../main", commit: SHA, scopes: SCOPES }],
    ["an option as the fork", { fork: "--help", commit: SHA, scopes: SCOPES }],
    ["a non-string fork", { fork: 1, commit: SHA, scopes: SCOPES }],
    ["a missing commit", { fork: "f", scopes: SCOPES }],
    ["a branch name as the commit", { fork: "f", commit: "main", scopes: SCOPES }],
    ["an abbreviated commit", { fork: "f", commit: SHA.slice(0, 7), scopes: SCOPES }],
    ["missing scopes", { fork: "f", commit: SHA }],
    ["scopes that are not an array", { fork: "f", commit: SHA, scopes: "all" }],
    [
      "a scope with an unknown mode",
      { fork: "f", commit: SHA, scopes: [{ scope: { kind: "dir", path: "" }, mode: "x" }] },
    ],
  ])("rejects %s", (_label, body) => {
    expect(parseMergeRequest(body).ok).toBe(false);
  });
});

describe("isForkOf", () => {
  it("accepts a fork of the repo", () => {
    expect(isForkOf("demo", { source: "artifacts:tessel/demo" })).toBe(true);
  });

  it.each([
    ["not a fork", null],
    ["an imported repo", "github:owner/demo"],
    ["a fork of another repo", "artifacts:tessel/other"],
    ["a fork of a repo whose name ends like this one", "artifacts:tessel/xdemo"],
  ])("rejects %s", (_label, source) => {
    expect(isForkOf("demo", { source })).toBe(false);
  });
});

describe("isSafeBranchName", () => {
  it.each(["main", "release/1.0", "feature_x-2"])("accepts %s", (name) => {
    expect(isSafeBranchName(name)).toBe(true);
  });

  it.each(["", "-main", "a..b", "a b", "a;b", "a:b", "dir/", "a\nb"])("rejects %j", (name) => {
    expect(isSafeBranchName(name)).toBe(false);
  });
});
