import { describe, expect, it } from "vitest";

import {
  STEWARD_COMMITTER_DATE,
  baseCommand,
  cloneCommand,
  conflictsCommand,
  diffCommand,
  fetchForkCommand,
  parseNulSeparated,
  pushCommand,
  reachableCommand,
  rebaseCommand,
  type MergeSources,
} from "./merge-commands";
import { parseSha, type Sha } from "./merge-types";
import { MAX_PUSH_BODY_BYTES } from "./receive-pack-policy";

function sha(character: string): Sha {
  const parsed = parseSha(character.repeat(40));
  if (parsed === undefined) {
    throw new Error("bad test sha");
  }
  return parsed;
}

const sources: MergeSources = {
  workspace: "/workspace",
  mainRemote: "https://git.example/tessel/demo.git",
  forkRemote: "https://git.example/tessel/demo--a1.git",
  forkBranch: "main",
};

describe("git commands", () => {
  it("clones main with `--` before the remote so it cannot be read as an option", () => {
    const { argv } = cloneCommand(sources);
    expect(argv.indexOf("--")).toBeLessThan(argv.indexOf(sources.mainRemote));
    expect(argv).toContain("--branch=main");
  });

  it("fetches only the fork's default branch into refs/remotes/fork", () => {
    expect(fetchForkCommand(sources).argv.slice(-2)).toEqual([
      sources.forkRemote,
      "+refs/heads/main:refs/remotes/fork/main",
    ]);
  });

  it("refuses a fork branch name that is not safe in a refspec", () => {
    const hostile = { ...sources, forkBranch: "main --upload-pack=x" };
    expect(() => fetchForkCommand(hostile)).toThrow("unsafe fork branch");
    expect(() => reachableCommand(hostile, sha("a"))).toThrow("unsafe fork branch");
  });

  it("passes a commit as one argv element, never inside shell text", () => {
    const { argv } = reachableCommand(sources, sha("a"));
    expect(argv).toEqual([
      "git",
      "-C",
      "/workspace",
      "merge-base",
      "--is-ancestor",
      "a".repeat(40),
      "refs/remotes/fork/main",
    ]);
  });

  it("rebases with a fixed committer identity and date and no hooks or signing", () => {
    const command = rebaseCommand("/workspace", sha("b"), sha("c"), sha("d"));
    expect(command.env).toMatchObject({
      GIT_COMMITTER_NAME: "Tessel Steward",
      GIT_COMMITTER_EMAIL: "steward@tessel.invalid",
      GIT_COMMITTER_DATE: STEWARD_COMMITTER_DATE,
      GIT_CONFIG_GLOBAL: "/dev/null",
    });
    expect(command.argv).toContain("core.hooksPath=/dev/null");
    expect(command.argv.slice(-4)).toEqual([
      "--onto",
      "b".repeat(40),
      "c".repeat(40),
      "d".repeat(40),
    ]);
  });

  it("sizes git's POST buffer to the gateway's body limit so git sends no probe request", () => {
    const { argv } = pushCommand(sources, sha("b"), sha("e"));
    const flag = argv.indexOf(`http.postBuffer=${MAX_PUSH_BODY_BYTES}`);
    expect(argv[flag - 1]).toBe("-c");
    expect(flag).toBeLessThan(argv.indexOf("push"));
  });

  it("pushes with a lease on the sha main had when it was cloned, and never forces", () => {
    const { argv } = pushCommand(sources, sha("b"), sha("e"));
    expect(argv).toContain(`--force-with-lease=refs/heads/main:${"b".repeat(40)}`);
    expect(argv).not.toContain("--force");
    expect(argv).not.toContain("-f");
    expect(argv.slice(-3)).toEqual(["--", sources.mainRemote, `${"e".repeat(40)}:refs/heads/main`]);
  });

  it("runs every local command against the workspace, not the current directory", () => {
    expect(baseCommand("/w").argv.slice(0, 3)).toEqual(["git", "-C", "/w"]);
    expect(conflictsCommand("/w").argv.slice(0, 3)).toEqual(["git", "-C", "/w"]);
  });
});

describe("parseNulSeparated", () => {
  it("splits paths on NUL and keeps names with spaces and newlines whole", () => {
    expect(parseNulSeparated("a b.txt\0dir/x\ny\0")).toEqual(["a b.txt", "dir/x\ny"]);
  });

  it("returns no paths for empty output", () => {
    expect(parseNulSeparated("")).toEqual([]);
  });
});

describe("diffCommand", () => {
  it("diffs the merge-base against the commit with no repo-named program, one argv element each", () => {
    const { argv } = diffCommand("/workspace", sha("a"), sha("b"));
    expect(argv).toEqual([
      "git",
      "-C",
      "/workspace",
      "diff",
      "--no-ext-diff",
      "--no-textconv",
      "--no-color",
      "-M",
      `${"a".repeat(40)}..${"b".repeat(40)}`,
      "--",
    ]);
  });
});
