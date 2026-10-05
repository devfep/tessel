import { execFile, execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { afterEach, describe, expect, it } from "vitest";

import type { GitCommand } from "./merge-commands";
import { runMerge, type MergeDeps } from "./merge-steps";
import { parseSha, type GitResult, type Sha } from "./merge-types";
import type { StepOutcome } from "./run-steps";

const execFileAsync = promisify(execFile);
const IDENTITY = ["-c", "user.name=Agent", "-c", "user.email=agent@example.invalid"];
const PASSING: StepOutcome = {
  step: "test",
  exitCode: 0,
  stdout: "",
  stderr: "",
  stdoutTruncated: false,
  stderrTruncated: false,
  passed: true,
};

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

/** A work tree on a local bare "main" and a bare "fork" of it, both with default branch main. */
class Repos {
  readonly root = mkdtempSync(join(tmpdir(), "tessel-merge-"));
  readonly main = join(this.root, "main.git");
  readonly fork = join(this.root, "fork.git");
  readonly work = join(this.root, "work");
  private runs = 0;

  constructor() {
    scratch.push(this.root);
    for (const bare of [this.main, this.fork]) {
      mkdirSync(bare);
      git(bare, "init", "--bare", "-b", "main");
    }
    mkdirSync(this.work);
    git(this.work, "init", "-b", "main");
    this.commit("a.txt", "one\ntwo\nthree\n", "base");
    git(this.work, "push", this.main, "main");
    git(this.work, "push", this.fork, "main");
  }

  commit(file: string, content: string, message: string): Sha {
    writeFileSync(join(this.work, file), content);
    git(this.work, "add", file);
    git(this.work, "commit", "-m", message);
    return sha(git(this.work, "rev-parse", "HEAD"));
  }

  resetTo(commit: string): void {
    git(this.work, "reset", "--hard", commit);
  }

  push(remote: string): void {
    git(this.work, "push", "--force", remote, "main");
  }

  mainHead(): Sha {
    return sha(git(this.main, "rev-parse", "main"));
  }

  deps(overrides: Partial<MergeDeps> = {}): MergeDeps {
    this.runs += 1;
    return {
      sources: {
        workspace: join(this.root, `workspace-${this.runs}`),
        mainRemote: this.main,
        forkRemote: this.fork,
        forkBranch: "main",
      },
      run: runLocally,
      revokeReadTokens: async () => true,
      runPackageStep: async () => PASSING,
      withPushAccess: async (_update, use) => use(),
      currentMain: async () => this.mainHead(),
      ...overrides,
    };
  }
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

/** main: base -> m2 (c.txt). fork: base -> f1 (b.txt). */
function divergedWithoutConflict(repos: Repos): Sha {
  const base = git(repos.work, "rev-parse", "HEAD");
  const forked = repos.commit("b.txt", "fork work\n", "fork work");
  repos.push(repos.fork);
  repos.resetTo(base);
  repos.commit("c.txt", "main moved on\n", "main moves");
  repos.push(repos.main);
  return forked;
}

describe("runMerge against real git", () => {
  it("rebases the fork commit onto main and pushes it to main", async () => {
    const repos = new Repos();
    const forked = divergedWithoutConflict(repos);
    const mainBefore = repos.mainHead();

    const outcome = await runMerge(repos.deps(), forked);

    expect(outcome).toMatchObject({ outcome: "merged", base: mainBefore });
    expect(repos.mainHead()).toBe(outcome.outcome === "merged" ? outcome.head : "");
    expect(git(repos.main, "rev-parse", "main~1")).toBe(mainBefore);
    expect(git(repos.main, "show", "main:b.txt")).toBe("fork work");
    expect(git(repos.main, "show", "main:c.txt")).toBe("main moved on");
  });

  it("gives the same head when the same merge is retried a second later", async () => {
    const repos = new Repos();
    const forked = divergedWithoutConflict(repos);
    const mainBefore = repos.mainHead();
    expect((await runMerge(repos.deps(), forked)).outcome).toBe("merged");
    const first = repos.mainHead();

    git(repos.main, "update-ref", "refs/heads/main", mainBefore);
    await new Promise((resolve) => setTimeout(resolve, 1100));
    expect((await runMerge(repos.deps(), forked)).outcome).toBe("merged");

    expect(repos.mainHead()).toBe(first);
  });

  it("reports the unmerged files when the rebase conflicts, and leaves main alone", async () => {
    const repos = new Repos();
    const base = git(repos.work, "rev-parse", "HEAD");
    const forked = repos.commit("a.txt", "one\nFORK\nthree\n", "fork edit");
    repos.push(repos.fork);
    repos.resetTo(base);
    repos.commit("a.txt", "one\nMAIN\nthree\n", "main edit");
    repos.push(repos.main);
    const mainBefore = repos.mainHead();

    const outcome = await runMerge(repos.deps(), forked);

    expect(outcome).toEqual({ outcome: "conflict", base: mainBefore, files: ["a.txt"] });
    expect(repos.mainHead()).toBe(mainBefore);
  });

  it("refuses a commit that is not on the fork's default branch", async () => {
    const repos = new Repos();
    divergedWithoutConflict(repos);
    const onlyOnMain = repos.mainHead();
    const unknown = sha("1".repeat(40));

    expect(await runMerge(repos.deps(), onlyOnMain)).toEqual({ outcome: "commit_not_in_fork" });
    expect(await runMerge(repos.deps(), unknown)).toEqual({ outcome: "commit_not_in_fork" });
  });

  it("does not overwrite main when it moved after the clone", async () => {
    const repos = new Repos();
    const forked = divergedWithoutConflict(repos);
    const base = repos.mainHead();
    let racer: Sha = base;
    const deps = repos.deps({
      withPushAccess: async (_update, use) => {
        repos.resetTo(base);
        racer = repos.commit("d.txt", "someone else\n", "race");
        repos.push(repos.main);
        return use();
      },
    });

    const outcome = await runMerge(deps, forked);

    expect(outcome).toEqual({ outcome: "main_moved", expected: base, actual: racer });
    expect(repos.mainHead()).toBe(racer);
  });

  it("reports a fork commit that main already contains as already_merged", async () => {
    const repos = new Repos();
    const forked = repos.commit("b.txt", "fork work\n", "fork work");
    repos.push(repos.fork);
    repos.push(repos.main);
    const mainBefore = repos.mainHead();

    const outcome = await runMerge(repos.deps(), forked);

    expect(outcome).toEqual({ outcome: "already_merged", base: mainBefore });
  });
});
