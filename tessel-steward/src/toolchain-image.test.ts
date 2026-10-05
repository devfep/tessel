import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import { TOOLCHAIN_ENV, WORKSPACE } from "./container-step";
import { TOOLCHAIN_IMAGE } from "./gate-plan";

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
  it("sets the environment the steward repeats on every command", () => {
    const env = finalEnv();
    for (const [key, value] of Object.entries(TOOLCHAIN_ENV)) {
      if (key !== "HOME") {
        expect(env[key], key).toBe(value);
      }
    }
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
