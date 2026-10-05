import { describe, expect, it } from "vitest";

import {
  isForkOf,
  isSafeBranchName,
  parseMergeRequest,
  parseSha,
  parseTrialRequest,
  parseTrialSide,
} from "./merge-types";

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

describe("parseTrialRequest", () => {
  it("accepts a fork, a main sha and a commit sha", () => {
    expect(parseTrialRequest({ fork: "demo--a1", before: SHA, main: SHA, commit: SHA })).toEqual({
      ok: true,
      request: { fork: "demo--a1", before: SHA, main: SHA, commit: SHA },
    });
  });

  it("accepts a request without a commit, or with a null one, as 'the fork's head'", () => {
    const expected = { ok: true, request: { fork: "demo--a1", before: SHA, main: SHA } };
    expect(parseTrialRequest({ fork: "demo--a1", before: SHA, main: SHA })).toEqual(expected);
    expect(parseTrialRequest({ fork: "demo--a1", before: SHA, main: SHA, commit: null })).toEqual(
      expected,
    );
  });

  it.each([
    ["a missing body", null],
    ["a string body", "x"],
    ["a missing fork", { before: SHA, main: SHA }],
    ["a fork that is not a name", { fork: "../x", before: SHA, main: SHA }],
    ["a missing main", { fork: "demo--a1", before: SHA }],
    ["a missing baseline", { fork: "demo--a1", main: SHA }],
    ["a baseline that is a ref name", { fork: "demo--a1", before: "main", main: SHA }],
    ["a main that is a ref name", { fork: "demo--a1", before: SHA, main: "main" }],
    ["a commit that is a ref name", { fork: "demo--a1", before: SHA, main: SHA, commit: "main" }],
    ["a commit that is an empty string", { fork: "demo--a1", before: SHA, main: SHA, commit: "" }],
    [
      "a commit that is an option",
      { fork: "demo--a1", before: SHA, main: SHA, commit: `--${SHA}` },
    ],
  ])("rejects %s", (_label, body) => {
    expect(parseTrialRequest(body)).toMatchObject({ ok: false });
  });
});

describe("parseTrialSide", () => {
  it("accepts a fork and a main sha, with or without a commit", () => {
    expect(parseTrialSide({ fork: "demo--a1", main: SHA })).toEqual({
      ok: true,
      request: { fork: "demo--a1", main: SHA },
    });
    expect(parseTrialSide({ fork: "demo--a1", main: SHA, commit: SHA })).toEqual({
      ok: true,
      request: { fork: "demo--a1", main: SHA, commit: SHA },
    });
  });

  it.each([
    ["a missing main", { fork: "demo--a1" }],
    ["a commit that is a ref name", { fork: "demo--a1", main: SHA, commit: "main" }],
  ])("rejects %s", (_label, body) => {
    expect(parseTrialSide(body)).toMatchObject({ ok: false });
  });
});
