import { execFile, execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { afterEach, describe, expect, it } from "vitest";

import { readGatePlan, type FileSource, type GatePlan } from "./gate-plan";
import { testCommands } from "./gate-steps";
import type { GitCommand } from "./merge-commands";
import type { ClaimedScope } from "./merge-coverage";
import { runMerge, runTrial, type MergeDeps } from "./merge-steps";
import { parseSha, type GitResult, type Sha } from "./merge-types";
import type { StepOutcome } from "./run-steps";

const execFileAsync = promisify(execFile);
const IDENTITY = ["-c", "user.name=Agent", "-c", "user.email=agent@example.invalid"];
const WHOLE_REPO: ClaimedScope[] = [
  { scope: { kind: "dir", path: "" }, mode: "edit_signature" },
  { scope: { kind: "dir", path: "" }, mode: "create" },
];
const STRONG = `instance = "standard-4"
[[test]]
argv = ["cargo", "test", "--workspace", "--locked", "--offline"]
`;
const WEAK = `instance = "standard-1"
[[test]]
argv = ["npm", "test"]
`;

const scratch: string[] = [];
afterEach(() => {
  for (const dir of scratch.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

function git(cwd: string, ...args: string[]): string {
  return execFileSync("git", [...IDENTITY, ...args], {
    cwd,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null" },
  }).trim();
}

function sha(value: string): Sha {
  const parsed = parseSha(value);
  if (parsed === undefined) {
    throw new Error(`not a sha: ${value}`);
  }
  return parsed;
}

/** Answers `readFile` the way Artifacts does: the file at a ref of one bare repo, or null. */
function filesOf(bare: string): FileSource {
  return {
    readFile: async ({ ref, path }) => {
      try {
        return new Blob([execFileSync("git", ["show", `${ref}:${path}`], { cwd: bare })]);
      } catch {
        return null;
      }
    },
  };
}

async function runLocally(command: GitCommand): Promise<GitResult> {
  const [binary, ...args] = command.argv;
  if (binary === undefined) {
    throw new Error("empty argv");
  }
  try {
    const { stdout, stderr } = await execFileAsync(binary, args, {
      env: { ...process.env, ...command.env },
      timeout: command.timeoutSeconds * 1000,
    });
    return { exitCode: 0, stdout, stderr, stdoutTruncated: false, stderrTruncated: false };
  } catch (error) {
    const failure = error as { code?: unknown; stdout?: string; stderr?: string };
    return {
      exitCode: typeof failure.code === "number" ? failure.code : 1,
      stdout: failure.stdout ?? "",
      stderr: failure.stderr ?? "",
      stdoutTruncated: false,
      stderrTruncated: false,
    };
  }
}

/** Main holds the strong gate; the fork's only commit weakens it and adds a file. */
function world() {
  const root = mkdtempSync(join(tmpdir(), "tessel-gate-"));
  scratch.push(root);
  const main = join(root, "main.git");
  const fork = join(root, "fork.git");
  const work = join(root, "work");
  for (const bare of [main, fork]) {
    mkdirSync(bare);
    git(bare, "init", "--bare", "-b", "main");
  }
  mkdirSync(work);
  git(work, "init", "-b", "main");
  writeFileSync(join(work, "tessel.toml"), STRONG);
  git(work, "add", "tessel.toml");
  git(work, "commit", "-m", "base");
  git(work, "push", main, "main");
  git(work, "push", fork, "main");
  writeFileSync(join(work, "tessel.toml"), WEAK);
  writeFileSync(join(work, "feature.txt"), "feature\n");
  git(work, "add", "tessel.toml", "feature.txt");
  git(work, "commit", "-m", "fork weakens the gate");
  git(work, "push", fork, "main");
  return {
    main,
    fork,
    base: sha(git(main, "rev-parse", "main")),
    commit: sha(git(fork, "rev-parse", "main")),
    root,
  };
}

/** What the sandbox saw when the test step ran: the gate's commands and the tree's tessel.toml. */
function recordingDeps(
  w: ReturnType<typeof world>,
  plan: GatePlan,
  seen: Array<{ commands: string[][]; treeToml: string }>,
): MergeDeps {
  const workspace = join(w.root, `workspace-${seen.length}`);
  return {
    pinnedMain: w.base,
    sources: { workspace, mainRemote: w.main, forkRemote: w.fork, forkBranch: "main" },
    run: runLocally,
    revokeReadTokens: async () => true,
    runPackageStep: async (step): Promise<StepOutcome> => {
      if (step === "test") {
        seen.push({
          commands: plan.kind === "configured" ? testCommands(plan.config).map((c) => c.argv) : [],
          treeToml: readFileSync(join(workspace, "tessel.toml"), "utf8"),
        });
      }
      return {
        step,
        exitCode: 0,
        stdout: "",
        stderr: "",
        stdoutTruncated: false,
        stderrTruncated: false,
        passed: step === "test",
      };
    },
    withPushAccess: async (_update, use) => use(),
    currentMain: async () => sha(git(w.main, "rev-parse", "main")),
  };
}

describe("a fork that weakens tessel.toml", () => {
  it("is still judged by the trunk's gate, in a merge and in a trial", async () => {
    const w = world();
    const plan = await readGatePlan(filesOf(w.main), w.base);
    expect(plan).toMatchObject({ kind: "configured", config: { instance: "standard-4" } });

    const seen: Array<{ commands: string[][]; treeToml: string }> = [];
    const trial = await runTrial(recordingDeps(w, plan, seen), w.base, w.commit);
    expect(trial).toMatchObject({ outcome: "clean" });
    const merged = await runMerge(recordingDeps(w, plan, seen), w.commit, WHOLE_REPO);
    expect(merged).toMatchObject({ outcome: "merged" });

    expect(seen).toHaveLength(2);
    for (const run of seen) {
      expect(run.treeToml).toBe(WEAK);
      expect(run.commands).toEqual([["cargo", "test", "--workspace", "--locked", "--offline"]]);
    }
  });

  it("would get the weak gate if it were read from the fork: the control for the test above", async () => {
    const w = world();
    const fromFork = await readGatePlan(filesOf(w.fork), w.commit);
    expect(fromFork).toMatchObject({ kind: "configured", config: { instance: "standard-1" } });
    const fromTrunk = await readGatePlan(filesOf(w.main), w.base);
    expect(fromTrunk).not.toEqual(fromFork);
  });

  it("is judged by main as it was at the pinned commit, not by a later main", async () => {
    const w = world();
    const later = join(w.root, "later");
    git(w.root, "clone", w.main, later);
    writeFileSync(
      join(later, "tessel.toml"),
      'instance = "standard-2"\n[[test]]\nargv = ["npm", "test"]\n',
    );
    git(later, "commit", "-am", "main changes its gate");
    git(later, "push", w.main, "main");

    const plan = await readGatePlan(filesOf(w.main), w.base);
    expect(plan).toMatchObject({ kind: "configured", config: { instance: "standard-4" } });
  });
});
