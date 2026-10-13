import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

import {
  RECORD_REF,
  gitObjectId,
  hexDigest,
  planCommit,
  treeBody,
  type FileWrite,
  type ObjectReader,
  type TreeEntry,
} from "./evidence-objects";
import { parseSha, type Sha } from "./merge-types";

const encoder = new TextEncoder();
const scratch: string[] = [];

afterEach(() => {
  for (const dir of scratch.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

function git(dir: string, args: string[], input?: Uint8Array): string {
  return execFileSync("git", ["-C", dir, ...args], {
    encoding: "utf8",
    input,
    stdio: ["pipe", "pipe", "pipe"],
    env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_NOSYSTEM: "1" },
  }).trim();
}

function bareRepo(): string {
  const dir = mkdtempSync(join(tmpdir(), "tessel-objects-"));
  scratch.push(dir);
  git(dir, ["init", "--quiet", "--bare", "--initial-branch=main"]);
  return dir;
}

function sha(value: string): Sha {
  const parsed = parseSha(value);
  if (parsed === undefined) {
    throw new Error(`not a sha: ${value}`);
  }
  return parsed;
}

/**
 * Reads trees and commits of a local repo the way the Artifacts binding does, except that a
 * directory's mode stays as `ls-tree` prints it (`040000`): both spellings must give git's ids.
 */
function readerOf(dir: string): ObjectReader {
  return {
    readTree: async (hash) =>
      git(dir, ["ls-tree", hash])
        .split("\n")
        .filter((line) => line !== "")
        .map((line): TreeEntry => {
          const [meta = "", name = ""] = line.split("\t");
          const [mode = "", , id = ""] = meta.split(" ");
          return { name, mode, hash: id };
        }),
    readCommit: async (hash) => ({ treeHash: git(dir, ["rev-parse", `${hash}^{tree}`]) }),
  };
}

function file(path: string, text: string): FileWrite {
  return { path, bytes: encoder.encode(text) };
}

async function importInto(dir: string, parent: Sha | null, files: FileWrite[], seconds: number) {
  const planned = await planCommit(readerOf(dir), parent, files, {
    seconds,
    message: "Record merge\n",
  });
  git(dir, ["fast-import", "--quiet"], planned.stream);
  git(dir, ["update-ref", "-d", RECORD_REF]);
  git(dir, ["update-ref", "refs/heads/main", planned.commit]);
  return planned;
}

describe("gitObjectId", () => {
  it("gives the id git gives a blob", async () => {
    const dir = bareRepo();
    const bytes = encoder.encode("hello\n");
    expect(await gitObjectId("blob", bytes)).toBe(git(dir, ["hash-object", "--stdin"], bytes));
  });

  it("hashes SHA-256 as lowercase hex", async () => {
    expect(await hexDigest("SHA-256", encoder.encode("abc"))).toBe(
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );
  });
});

describe("treeBody", () => {
  it("orders a directory as if its name ended in a slash", async () => {
    const dir = bareRepo();
    const blob = git(dir, ["hash-object", "-w", "--stdin"], encoder.encode("x"));
    const subtree = git(dir, ["mktree"], encoder.encode(`100644 blob ${blob}\tf\n`));
    const entries: TreeEntry[] = [
      { name: "a", mode: "40000", hash: subtree },
      { name: "a.b", mode: "100644", hash: blob },
      { name: "a-b", mode: "100644", hash: blob },
    ];
    const listing = entries
      .map(
        (entry) =>
          `${entry.mode === "40000" ? "040000 tree" : "100644 blob"} ${entry.hash}\t${entry.name}`,
      )
      .join("\n");
    const expected = git(dir, ["mktree"], encoder.encode(`${listing}\n`));
    expect(await gitObjectId("tree", treeBody(entries))).toBe(expected);
  });
});

describe("planCommit", () => {
  it("computes the commit git fast-import builds in an empty repo", async () => {
    const dir = bareRepo();
    const files = [file("merges/INDEX.md", "one\n"), file("merges/abc.md", "# Merge\n")];
    const planned = await importInto(dir, null, files, 1_791_800_000);
    expect(git(dir, ["rev-parse", "refs/heads/main"])).toBe(planned.commit);
    expect(git(dir, ["show", "main:merges/abc.md"])).toBe("# Merge");
    expect(git(dir, ["log", "-1", "--format=%an <%ae> %at %cn %ct"])).toBe(
      "Tessel Steward <steward@tessel.invalid> 1791800000 Tessel Steward 1791800000",
    );
  });

  it("computes the child commit that adds and replaces files beside existing ones", async () => {
    const dir = bareRepo();
    const first = await importInto(
      dir,
      null,
      [file("merges/INDEX.md", "one\n"), file("agent-1/1.md", "t1")],
      1_791_800_000,
    );
    const second = await importInto(
      dir,
      sha(first.commit),
      [file("merges/INDEX.md", "one\ntwo\n"), file("merges/def.md", "x"), file("a/2.md", "t2")],
      1_791_800_060,
    );
    expect(git(dir, ["rev-parse", "main"])).toBe(second.commit);
    expect(git(dir, ["rev-parse", "main^"])).toBe(first.commit);
    expect(git(dir, ["ls-tree", "-r", "--name-only", "main"]).split("\n")).toEqual([
      "a/2.md",
      "agent-1/1.md",
      "merges/INDEX.md",
      "merges/def.md",
    ]);
    expect(git(dir, ["show", "main:merges/INDEX.md"])).toBe("one\ntwo");
  });

  it.each([["../x.md"], ["merges/../x.md"], ["x.md"], ["a/b/c.md"], ["merges/.hidden"]])(
    "refuses the unsafe path %s",
    async (path) => {
      const reader: ObjectReader = { readTree: async () => [], readCommit: async () => null };
      await expect(
        planCommit(reader, null, [file(path, "x")], { seconds: 1, message: "m\n" }),
      ).rejects.toThrow("path");
    },
  );

  it("refuses to write a file where the parent has a file of the directory's name", async () => {
    const dir = bareRepo();
    const blob = git(dir, ["hash-object", "-w", "--stdin"], encoder.encode("x"));
    const tree = git(dir, ["mktree"], encoder.encode(`100644 blob ${blob}\tmerges\n`));
    const reader: ObjectReader = {
      readTree: async () => [{ name: "merges", mode: "100644", hash: blob }],
      readCommit: async () => ({ treeHash: tree }),
    };
    await expect(
      planCommit(reader, sha("1".repeat(40)), [file("merges/a.md", "x")], {
        seconds: 1,
        message: "m\n",
      }),
    ).rejects.toThrow("another kind of entry");
  });
});
