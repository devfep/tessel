import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import { TOOLCHAIN_ENV, TOOLCHAIN_USER, WORKSPACE, execCaptured } from "./container-step";
import { TOOLCHAIN_ENTRYPOINT, TOOLCHAIN_IMAGE } from "./gate-plan";

const steward = (name: string) => join(import.meta.dirname, "..", name);
const dockerfile = readFileSync(steward("toolchain.Dockerfile"), "utf8");

/** The `ENV` instruction of the last stage, as a key/value map. */
function finalEnv(): Record<string, string> {
  const lastStage = dockerfile.slice(dockerfile.lastIndexOf("FROM node"));
  const instruction = /^ENV ((?:.*\\\n)*.*)$/m.exec(lastStage)?.[1] ?? "";
  const env: Record<string, string> = {};
  for (const pair of instruction.replaceAll("\\\n", " ").trim().split(/\s+/)) {
    const [key, ...value] = pair.split("=");
    if (key !== undefined) {
      env[key] = value.join("=");
    }
  }
  return env;
}

describe("the toolchain image", () => {
  it("runs commands as numeric uid:gid, the only form the runtime accepts", () => {
    expect(TOOLCHAIN_USER).toMatch(/^\d+:\d+$/);
    expect(dockerfile).toContain("--chown=node:node");
  });

  it("names the step when the runtime cannot start a process, and redacts tokens", async () => {
    const container = {
      exec: async () => {
        throw new Error("internal error; reference = abc art_v1_secret");
      },
    } as unknown as Container;
    const failure = execCaptured(container, "clone", "60", ["git", "clone"], {});
    await expect(failure).rejects.toThrow("The clone step could not be started");
    await expect(failure).rejects.not.toThrow("art_v1_secret");
    await expect(failure).rejects.toThrow("internal error; reference = abc");
  });

  it("sets the environment the steward repeats on every command", () => {
    const env = finalEnv();
    for (const [key, value] of Object.entries(TOOLCHAIN_ENV)) {
      if (key !== "HOME") {
        expect(env[key], key).toBe(value);
      }
    }
  });

  it("has GNU time for the peak-memory read and tini as PID 1 to reap detached processes", () => {
    expect(dockerfile).toMatch(/apt-get install[^\n]*\btime\b/);
    expect(dockerfile).toMatch(/apt-get install[^\n]*\btini\b/);
    expect(dockerfile).toContain('ENTRYPOINT ["/usr/bin/tini", "--"]');
    expect(TOOLCHAIN_ENTRYPOINT.slice(0, 2)).toEqual(["/usr/bin/tini", "--"]);
  });

  it("clones into the workspace directory the steward uses", () => {
    expect(dockerfile).toContain(`WORKDIR ${WORKSPACE}`);
  });

  it("bakes the dependencies from the lockfiles, with Cargo.lock honoured and no network later", () => {
    expect(dockerfile).toContain("cargo chef cook --tests --workspace --locked");
    expect(dockerfile).toContain("cargo fetch --locked");
    expect(dockerfile).toContain("pnpm fetch");
  });

  it("is built from the repository root, which holds the lockfiles it needs", () => {
    const wrangler = readFileSync(steward("wrangler.jsonc"), "utf8");
    expect(wrangler).toContain(
      `"${TOOLCHAIN_IMAGE}": { "dockerfile": "./toolchain.Dockerfile", "build_context": ".." }`,
    );
    expect(existsSync(steward("../Cargo.lock"))).toBe(true);
    expect(existsSync(steward("pnpm-lock.yaml"))).toBe(true);
  });

  it("keeps the build context lean: no git history, targets or node_modules", () => {
    const ignore = readFileSync(steward("../.dockerignore"), "utf8").split("\n");
    for (const entry of [".git", "target", "**/node_modules", ".dev.vars"]) {
      expect(ignore).toContain(entry);
    }
  });
});
