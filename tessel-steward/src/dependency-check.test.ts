import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterAll, describe, expect, it } from "vitest";

import {
  DEPENDENCIES_DECLARED_EXIT_CODE,
  DEPENDENCY_CHECK_SCRIPT,
  PACKAGE_JSON_INVALID_EXIT_CODE,
  PACKAGE_JSON_MISSING_EXIT_CODE,
} from "./dependency-check";

const directories: string[] = [];

afterAll(() => {
  for (const directory of directories) {
    rmSync(directory, { recursive: true, force: true });
  }
});

function check(packageJson: string | undefined): number | null {
  const directory = mkdtempSync(join(tmpdir(), "dependency-check-"));
  directories.push(directory);
  if (packageJson !== undefined) {
    writeFileSync(join(directory, "package.json"), packageJson);
  }
  return spawnSync(
    process.execPath,
    ["-e", DEPENDENCY_CHECK_SCRIPT, join(directory, "package.json")],
    { cwd: tmpdir() },
  ).status;
}

function checkWith(fields: Record<string, unknown>): number | null {
  return check(JSON.stringify({ name: "demo", scripts: { test: "node --test" }, ...fields }));
}

describe("dependency check script", () => {
  it("reports a missing package.json", () => {
    expect(check(undefined)).toBe(PACKAGE_JSON_MISSING_EXIT_CODE);
  });

  it("reports an unparsable package.json", () => {
    expect(check("{ not json")).toBe(PACKAGE_JSON_INVALID_EXIT_CODE);
  });

  it("reports a package.json that is not an object", () => {
    expect(check("[]")).toBe(PACKAGE_JSON_INVALID_EXIT_CODE);
    expect(check("null")).toBe(PACKAGE_JSON_INVALID_EXIT_CODE);
  });

  it("reports a package.json that is a directory as invalid, not missing", () => {
    const directory = mkdtempSync(join(tmpdir(), "dependency-check-"));
    directories.push(directory);
    mkdirSync(join(directory, "package.json"));
    const { status } = spawnSync(process.execPath, ["-e", DEPENDENCY_CHECK_SCRIPT], {
      cwd: directory,
    });
    expect(status).toBe(PACKAGE_JSON_INVALID_EXIT_CODE);
  });

  it("accepts a package.json with no dependency keys", () => {
    expect(checkWith({})).toBe(0);
  });

  it.each(["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"])(
    "refuses a non-empty %s",
    (key) => {
      expect(checkWith({ [key]: { left: "1.0.0" } })).toBe(DEPENDENCIES_DECLARED_EXIT_CODE);
    },
  );

  it.each(["bundleDependencies", "bundledDependencies", "workspaces"])(
    "refuses a non-empty array in %s",
    (key) => {
      expect(checkWith({ [key]: ["packages/a"] })).toBe(DEPENDENCIES_DECLARED_EXIT_CODE);
    },
  );

  it("refuses workspaces given as an object with packages", () => {
    expect(checkWith({ workspaces: { packages: ["packages/a"] } })).toBe(
      DEPENDENCIES_DECLARED_EXIT_CODE,
    );
  });

  it("accepts empty objects and arrays", () => {
    expect(
      checkWith({
        dependencies: {},
        devDependencies: {},
        peerDependencies: {},
        bundleDependencies: [],
        workspaces: [],
      }),
    ).toBe(0);
  });

  it("refuses a dependency key that is not an object or array", () => {
    expect(checkWith({ dependencies: "left-pad" })).toBe(DEPENDENCIES_DECLARED_EXIT_CODE);
    expect(checkWith({ devDependencies: null })).toBe(DEPENDENCIES_DECLARED_EXIT_CODE);
    expect(checkWith({ bundleDependencies: true })).toBe(DEPENDENCIES_DECLARED_EXIT_CODE);
  });
});
