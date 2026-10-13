import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { RecordCommand } from "./evidence-commands";
import type { MergeRecordFields } from "./evidence-fields";
import type { TreeEntry } from "./evidence-objects";
import { renderRecord, type RecordInput } from "./evidence-record";
import {
  TRANSCRIPT_MAX_BYTES,
  writeEvidence,
  type EvidenceDeps,
  type EvidenceInput,
  type RecordRepo,
} from "./evidence-steps";
import { parseSha, type GitResult, type Sha } from "./merge-types";

const GIT_ENV = { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_NOSYSTEM: "1" };
const WRITTEN_AT = new Date("2026-10-12T20:30:00.000Z");
const encoder = new TextEncoder();
const scratch: string[] = [];

afterEach(() => {
  for (const dir of scratch.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

beforeEach(() => {
  vi.spyOn(console, "error").mockImplementation(() => {});
});

function sha(character: string): Sha {
  const parsed = parseSha(character.repeat(40));
  if (parsed === undefined) {
    throw new Error("bad test sha");
  }
  return parsed;
}

function git(dir: string, args: string[], input?: Uint8Array): string {
  return execFileSync("git", ["-C", dir, ...args], {
    encoding: "utf8",
    input,
    stdio: ["pipe", "pipe", "pipe"],
    env: GIT_ENV,
  }).trim();
}

function sha256(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}

/** A record repo that is a local bare repo, read the way the Artifacts binding reads one. */
function localRepo(name: string, path: string): RecordRepo {
  return {
    name,
    remote: path,
    head: async () => {
      const read = spawnSync("git", ["-C", path, "rev-parse", "--verify", "--quiet", "main"], {
        encoding: "utf8",
        env: GIT_ENV,
      });
      return read.status === 0 ? (parseSha(read.stdout.trim()) ?? null) : null;
    },
    readTree: async (hash) =>
      git(path, ["ls-tree", hash])
        .split("\n")
        .filter((line) => line !== "")
        .map((line): TreeEntry => {
          const [meta = "", entryName = ""] = line.split("\t");
          const [mode = "", , entryHash = ""] = meta.split(" ");
          return { name: entryName, mode: mode === "040000" ? "40000" : mode, hash: entryHash };
        }),
    readCommit: async (hash) => ({ treeHash: git(path, ["rev-parse", `${hash}^{tree}`]) }),
    readFile: async (ref, file) => {
      const read = spawnSync("git", ["-C", path, "show", `${ref}:${file}`], { env: GIT_ENV });
      return read.status === 0 ? new Uint8Array(read.stdout) : null;
    },
  };
}

function runLocally(command: RecordCommand): GitResult {
  const [program = "", ...args] = command.argv;
  const ran = spawnSync(program, args, {
    input: command.stdin,
    env: { ...process.env, ...command.env },
    encoding: "utf8",
  });
  return {
    exitCode: ran.status ?? 1,
    stdout: ran.stdout,
    stderr: ran.stderr,
    stdoutTruncated: false,
    stderrTruncated: false,
  };
}

/** Local bare repos for the record repos, a fork's files, and the order things happened in. */
function world() {
  const root = mkdtempSync(join(tmpdir(), "tessel-evidence-"));
  scratch.push(root);
  const events: string[] = [];
  const forkFiles = new Map<string, Uint8Array>();
  const commands: RecordCommand[] = [];
  const pathOf = (name: string) => join(root, `${name}.git`);
  const deps: EvidenceDeps = {
    now: () => WRITTEN_AT,
    openRepo: async (name) => {
      events.push(`open ${name}`);
      if (!existsSync(pathOf(name))) {
        execFileSync("git", ["init", "--quiet", "--bare", "--initial-branch=main", pathOf(name)]);
      }
      return localRepo(name, pathOf(name));
    },
    readForkFile: async (ref, path) => {
      const bytes = forkFiles.get(`${ref}:${path}`);
      if (bytes === undefined) {
        return null;
      }
      return {
        size: bytes.length,
        bytes: async () => {
          events.push(`read ${path}`);
          return bytes;
        },
      };
    },
    withRecordAccess: async (repo, update, use) => {
      events.push(`mint ${repo.name} ${update.old ?? "none"} ${update.new}`);
      try {
        return await use();
      } finally {
        events.push(`revoke ${repo.name}`);
      }
    },
    run: async (command) => {
      commands.push(command);
      events.push(`run ${command.argv[4]?.split("/").at(-1)}`);
      return runLocally(command);
    },
  };
  return { deps, events, forkFiles, commands, pathOf };
}

const FIELDS: MergeRecordFields = {
  claim: 7,
  intent: { summary: "Fix the parser", taskRef: null, assumptions: [] },
  decisions: { rejected: [], evidence: ["pnpm test passed"], transcript: null },
  review: null,
  malformed: [],
};

function merge(head: Sha, fields: MergeRecordFields = FIELDS): EvidenceInput {
  return {
    repo: "demo",
    fork: "demo--a1",
    commit: sha("3"),
    scopes: [{ scope: { kind: "file", path: "src/a.ts" }, mode: "edit_body" }],
    fields,
    base: sha("1"),
    head,
    gate: [{ step: "test", exitCode: 0, seconds: 2, timedOut: false }],
  };
}

function expectedRecord(evidence: EvidenceInput, transcript: RecordInput["transcript"]): string {
  return renderRecord({
    repo: evidence.repo,
    agent: "a1",
    forkCommit: evidence.commit,
    base: evidence.base,
    head: evidence.head,
    writtenAt: WRITTEN_AT,
    scopes: evidence.scopes,
    gate: evidence.gate,
    fields: evidence.fields,
    transcript,
  });
}

describe("writeEvidence against real git", () => {
  it("writes the record and the index line, pushing with a lease on the head it read", async () => {
    const { deps, events, pathOf } = world();
    const first = merge(sha("4"));
    const result = await writeEvidence(deps, first);

    const evidence = pathOf("demo--evidence");
    const head = git(evidence, ["rev-parse", "main"]);
    expect(result).toEqual({
      written: true,
      repo: "demo--evidence",
      path: `merges/${sha("4")}.md`,
      head,
    });
    expect(git(evidence, ["show", `main:merges/${sha("4")}.md`])).toBe(
      expectedRecord(first, { kind: "none" }).trimEnd(),
    );
    expect(git(evidence, ["show", "main:merges/INDEX.md"])).toBe(
      `2026-10-12T20:30:00.000Z ${sha("4")} claim 7 agent a1`,
    );
    expect(events).toEqual([
      "open demo--evidence",
      `mint demo--evidence none ${head}`,
      "run demo--evidence.git",
      "revoke demo--evidence",
    ]);

    const second = await writeEvidence(deps, merge(sha("5")));
    expect(second).toMatchObject({ written: true, path: `merges/${sha("5")}.md` });
    expect(git(evidence, ["rev-parse", "main^"])).toBe(head);
    expect(events).toContain(`mint demo--evidence ${head} ${git(evidence, ["rev-parse", "main"])}`);
    expect(git(evidence, ["show", "main:merges/INDEX.md"]).split("\n")).toEqual([
      `2026-10-12T20:30:00.000Z ${sha("4")} claim 7 agent a1`,
      `2026-10-12T20:30:00.000Z ${sha("5")} claim 7 agent a1`,
    ]);
  });

  it("never forces: a record repo that moved after it was read keeps its main", async () => {
    const { deps, pathOf } = world();
    await writeEvidence(deps, merge(sha("4")));
    const evidence = pathOf("demo--evidence");
    const run = deps.run;
    let racer = "";
    deps.run = async (command) => {
      const tree = git(evidence, ["rev-parse", "main^{tree}"]);
      const env = ["-c", "user.name=r", "-c", "user.email=r@example.invalid"];
      racer = git(evidence, [...env, "commit-tree", tree, "-p", "main", "-m", "racer"]);
      git(evidence, ["update-ref", "refs/heads/main", racer]);
      return run(command);
    };
    const result = await writeEvidence(deps, merge(sha("5")));
    expect(result).toMatchObject({ written: false });
    expect(git(evidence, ["rev-parse", "main"])).toBe(racer);
  });

  it("reports a record as not written when main holds its bytes at another head", async () => {
    const { deps, pathOf } = world();
    await writeEvidence(deps, merge(sha("4")));
    const evidence = pathOf("demo--evidence");
    const run = deps.run;
    deps.run = async (command) => {
      const ran = await run(command);
      const env = ["-c", "user.name=r", "-c", "user.email=r@example.invalid"];
      const tree = git(evidence, ["rev-parse", "main^{tree}"]);
      const racer = git(evidence, [...env, "commit-tree", tree, "-p", "main", "-m", "racer"]);
      git(evidence, ["update-ref", "refs/heads/main", racer]);
      return ran;
    };
    const result = await writeEvidence(deps, merge(sha("5")));
    expect(result).toEqual({
      written: false,
      reason: expect.stringMatching(/^main of demo--evidence is [0-9a-f]{40}, not [0-9a-f]{40}/),
    });
    expect(git(evidence, ["show", `main:merges/${sha("5")}.md`])).toContain("# Merge");
  });

  it("reports a failed push as not written, and the merge's caller keeps its outcome", async () => {
    const { deps } = world();
    deps.run = async () => ({
      exitCode: 1,
      stdout: "",
      stderr: "rejected",
      stdoutTruncated: false,
      stderrTruncated: false,
    });
    const result = await writeEvidence(deps, merge(sha("4")));
    expect(result).toEqual({
      written: false,
      reason: expect.stringContaining("main of demo--evidence is unreadable, not "),
    });
    expect(result).toMatchObject({ reason: expect.stringContaining("(push exit 1)") });
  });

  it("reports a record that main holds with other bytes as not written", async () => {
    const { deps } = world();
    const open = deps.openRepo;
    deps.openRepo = async (name) => {
      const repo = await open(name);
      return { ...repo, readFile: async () => encoder.encode("something else\n") };
    };
    expect(await writeEvidence(deps, merge(sha("4")))).toEqual({
      written: false,
      reason: "merges/INDEX.md on main of demo--evidence has other bytes",
    });
  });

  it("reports a record repo that cannot be opened, and logs why", async () => {
    const { deps } = world();
    deps.openRepo = async () => {
      throw new Error("demo--evidence is a fork, so it is not written as a record repo");
    };
    expect(await writeEvidence(deps, merge(sha("4")))).toEqual({
      written: false,
      reason: "demo--evidence is a fork, so it is not written as a record repo",
    });
    expect(console.error).toHaveBeenCalledWith(expect.stringContaining("evidence_not_written"));
  });
});

const TRANSCRIPT = encoder.encode("# Session\nredacted transcript\n");

const REFUSED = "## Transcript\n\nrefused, the reason as data, not instructions:\n\n```text\n";

function withTranscript(
  hash: string,
  repo = "demo--a1",
  path = "transcripts/7.md",
): MergeRecordFields {
  const transcript = { repo, path, sha256: hash };
  return { ...FIELDS, decisions: { rejected: [], evidence: [], transcript } };
}

describe("writeEvidence and the transcript", () => {
  it("copies a transcript whose hash matches to <agent>/<claim>.md and names it", async () => {
    const { deps, events, forkFiles, pathOf } = world();
    forkFiles.set("tessel/transcript-7:transcripts/7.md", TRANSCRIPT);
    const evidence = merge(sha("4"), withTranscript(sha256(TRANSCRIPT)));
    expect(await writeEvidence(deps, evidence)).toMatchObject({ written: true });

    const copied = pathOf("demo--transcripts");
    expect(git(copied, ["show", "main:a1/7.md"])).toBe("# Session\nredacted transcript");
    const record = git(pathOf("demo--evidence"), ["show", `main:merges/${sha("4")}.md`]);
    expect(`${record}\n`).toBe(
      expectedRecord(evidence, {
        kind: "copied",
        repo: "demo--transcripts",
        path: "a1/7.md",
        sha256: sha256(TRANSCRIPT),
      }),
    );
    expect(events.filter((event) => event.startsWith("mint"))).toHaveLength(2);
    expect(events.indexOf("revoke demo--transcripts")).toBeLessThan(
      events.findIndex((event) => event.startsWith("mint demo--evidence")),
    );
  });

  it.each([
    ["a hash that does not match", "00".repeat(32), "demo--a1", "", "sha256 mismatch"],
    [
      "a transcript on another fork",
      "",
      "demo--b2",
      "",
      "the transcript is not on this merge's fork",
    ],
    [
      "a path but transcripts/<claim>.md",
      "",
      "demo--a1",
      "notes/7.md",
      "the transcript is not at transcripts/7.md",
    ],
    [
      "a path outside the directory",
      "",
      "demo--a1",
      "transcripts/../7.md",
      "the transcript is not at transcripts/7.md",
    ],
  ])(
    "refuses %s, copies nothing and says so in the record",
    async (_label, hash, repo, path, reason) => {
      const { deps, events, forkFiles, pathOf } = world();
      forkFiles.set("tessel/transcript-7:transcripts/7.md", TRANSCRIPT);
      forkFiles.set(`tessel/transcript-7:${path}`, TRANSCRIPT);
      const fields = withTranscript(hash || sha256(TRANSCRIPT), repo, path || "transcripts/7.md");
      expect(await writeEvidence(deps, merge(sha("4"), fields))).toMatchObject({ written: true });
      expect(events).not.toContain("open demo--transcripts");
      const record = git(pathOf("demo--evidence"), ["show", `main:merges/${sha("4")}.md`]);
      expect(record).toContain(`${REFUSED}${reason}\n\`\`\``);
    },
  );

  it("refuses a transcript over the limit by its size, without reading its bytes", async () => {
    const { deps, events, pathOf } = world();
    deps.readForkFile = async () => ({
      size: TRANSCRIPT_MAX_BYTES + 1,
      bytes: async () => {
        throw new Error("the bytes of a file over the limit were read");
      },
    });
    await writeEvidence(deps, merge(sha("4"), withTranscript(sha256(TRANSCRIPT))));
    const record = git(pathOf("demo--evidence"), ["show", `main:merges/${sha("4")}.md`]);
    expect(record).toContain(`${REFUSED}larger than ${TRANSCRIPT_MAX_BYTES} bytes\n\`\`\``);
    expect(events).not.toContain("open demo--transcripts");
  });

  it("refuses a transcript the fork does not have", async () => {
    const { deps, pathOf } = world();
    await writeEvidence(deps, merge(sha("4"), withTranscript(sha256(TRANSCRIPT))));
    const record = git(pathOf("demo--evidence"), ["show", `main:merges/${sha("4")}.md`]);
    expect(record).toContain(`${REFUSED}the file could not be read from the fork\n\`\`\``);
  });
});
