import { describe, expect, it } from "vitest";

import type { GitCommand } from "./merge-commands";
import type { TrialDeps } from "./merge-steps";
import { parseSha, type GitResult, type Sha } from "./merge-types";
import { DIFF_CAP_BYTES, parseChangedFiles, runDiff } from "./review-diff";

const COMMIT = parseSha("c".repeat(40)) as Sha;
const MAIN = "a".repeat(40);
const BASE = "b".repeat(40);

function ok(stdout = "", extra: Partial<GitResult> = {}): GitResult {
  return {
    exitCode: 0,
    stdout,
    stderr: "",
    stdoutTruncated: false,
    stderrTruncated: false,
    ...extra,
  };
}

type Responder = (argv: string[]) => GitResult | undefined;

function fakeDeps(respond: Responder, revoked = true) {
  const log: string[][] = [];
  const events: string[] = [];
  const deps: TrialDeps = {
    sources: {
      workspace: "/workspace",
      mainRemote: "https://git/main",
      forkRemote: "https://git/fork",
      forkBranch: "main",
    },
    run(command: GitCommand) {
      log.push(command.argv);
      events.push(
        `git ${command.argv.find((a) => !a.startsWith("-") && a !== "git" && a !== "/workspace")}`,
      );
      return Promise.resolve(respond(command.argv) ?? ok());
    },
    revokeReadTokens() {
      events.push("revoke");
      return Promise.resolve(revoked);
    },
    runPackageStep: () => Promise.reject(new Error("a diff never runs repo code")),
  };
  return { deps, log, events };
}

function happy(patch: GitResult = ok("diff --git a/x b/x\n+hi\n")): Responder {
  return (argv) => {
    if (argv.includes("rev-parse") && argv.some((a) => a.startsWith("refs/remotes/origin"))) {
      return ok(`${MAIN}\n`);
    }
    if (argv.includes("merge-base")) {
      return ok(`${BASE}\n`);
    }
    if (argv.includes("--name-status")) {
      return ok("M\0src/x.rs\0A\0new.rs\0");
    }
    if (argv.includes("--no-color")) {
      return patch;
    }
    return undefined;
  };
}

describe("runDiff", () => {
  it("reads the diff from the merge-base to the commit and lists the changed files", async () => {
    const { deps, log } = fakeDeps(happy());
    const outcome = await runDiff(deps, COMMIT);
    expect(outcome).toEqual({
      outcome: "ok",
      base: BASE,
      commit: COMMIT,
      files: [
        { status: "M", path: "src/x.rs" },
        { status: "A", path: "new.rs" },
      ],
      diff: "diff --git a/x b/x\n+hi\n",
      truncated: false,
      captureOverflow: false,
    });
    expect(log.at(-1)?.at(-2)).toBe(`${BASE}..${COMMIT}`);
  });

  it("revokes the read tokens after the fetch and before any local git step", async () => {
    const { deps, events } = fakeDeps(happy());
    await runDiff(deps, COMMIT);
    expect(events.slice(0, 3)).toEqual(["git clone", "git fetch", "revoke"]);
  });

  it("cuts a diff over the cap and says so", async () => {
    const big = "x".repeat(DIFF_CAP_BYTES + 50);
    const outcome = await runDiff(fakeDeps(happy(ok(big))).deps, COMMIT);
    if (outcome.outcome !== "ok") {
      throw new Error("expected ok");
    }
    expect(outcome.truncated).toBe(true);
    expect(outcome.captureOverflow).toBe(false);
    expect(new TextEncoder().encode(outcome.diff).length).toBe(DIFF_CAP_BYTES);
  });

  it("keeps a diff exactly at the cap whole", async () => {
    const exact = "y".repeat(DIFF_CAP_BYTES);
    const outcome = await runDiff(fakeDeps(happy(ok(exact))).deps, COMMIT);
    expect(outcome).toMatchObject({ outcome: "ok", truncated: false, diff: exact });
  });

  it("shows no diff text when the sandbox capture overflowed, only the file list", async () => {
    const overflow = ok("tail of a huge diff", { stdoutTruncated: true });
    const outcome = await runDiff(fakeDeps(happy(overflow)).deps, COMMIT);
    expect(outcome).toMatchObject({
      outcome: "ok",
      diff: "",
      truncated: true,
      captureOverflow: true,
    });
  });

  it("answers an error, never an empty diff, when the clone fails", async () => {
    const { deps, events } = fakeDeps((argv) =>
      argv.includes("clone") ? { ...ok(), exitCode: 128 } : undefined,
    );
    expect(await runDiff(deps, COMMIT)).toEqual({
      outcome: "error",
      reason: "cloning main failed (git exit 128)",
    });
    expect(events).toEqual(["git clone", "revoke"]);
  });

  it("answers an error when the fetch, the commit lookup, or the diff itself fails", async () => {
    const cases: Array<[string, string]> = [
      ["fetch", "fetching the fork failed (git exit 1)"],
      ["--quiet", "the commit is not in the fork"],
      ["--no-color", "reading the diff failed (git exit 1)"],
      ["--name-status", "listing the changed files failed (git exit 1)"],
    ];
    for (const [needle, reason] of cases) {
      const respond: Responder = (argv) =>
        argv.includes(needle) ? { ...ok(), exitCode: 1 } : happy()(argv);
      expect(await runDiff(fakeDeps(respond).deps, COMMIT)).toEqual({ outcome: "error", reason });
    }
  });

  it("throws when a read token cannot be revoked", async () => {
    await expect(runDiff(fakeDeps(happy(), false).deps, COMMIT)).rejects.toThrow("revoked");
  });
});

describe("parseChangedFiles", () => {
  it("reads renames with both paths, and an empty diff as no files", () => {
    expect(parseChangedFiles("R100\0old.rs\0new.rs\0D\0gone.rs\0")).toEqual([
      { status: "R", path: "new.rs", from: "old.rs" },
      { status: "D", path: "gone.rs" },
    ]);
    expect(parseChangedFiles("")).toEqual([]);
  });

  it("refuses output that ends mid-record", () => {
    expect(parseChangedFiles("R100\0old.rs\0")).toBeUndefined();
    expect(parseChangedFiles("M\0")).toBeUndefined();
  });
});
