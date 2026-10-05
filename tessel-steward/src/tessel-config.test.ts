import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import { MAX_CONFIG_BYTES, parseGateConfig } from "./tessel-config";

const GOOD = `
instance = "standard-4"

[install]
cargo = true
pnpm = ["tessel-steward"]

[[test]]
argv = ["cargo", "test", "--workspace", "--locked", "--offline"]

[[test]]
dir = "tessel-steward"
argv = ["pnpm", "test"]
`;

function rejected(text: string): string {
  const parsed = parseGateConfig(text);
  if (parsed.ok) {
    throw new Error(`expected a rejection, got ${JSON.stringify(parsed.config)}`);
  }
  return parsed.error;
}

describe("parseGateConfig", () => {
  it("reads the gate, defaulting the directory of a command to the repo root", () => {
    expect(parseGateConfig(GOOD)).toEqual({
      ok: true,
      config: {
        instance: "standard-4",
        installCargo: true,
        installPnpm: ["tessel-steward"],
        tests: [
          { dir: ".", argv: ["cargo", "test", "--workspace", "--locked", "--offline"] },
          { dir: "tessel-steward", argv: ["pnpm", "test"] },
        ],
      },
    });
  });

  it("accepts the tessel.toml committed at the root of this repo", () => {
    const text = readFileSync(join(import.meta.dirname, "..", "..", "tessel.toml"), "utf8");
    const parsed = parseGateConfig(text);
    expect(parsed).toMatchObject({ ok: true, config: { instance: "standard-4" } });
  });

  it("needs no install table", () => {
    const parsed = parseGateConfig('instance = "standard-1"\n[[test]]\nargv = ["npm", "test"]\n');
    expect(parsed).toMatchObject({
      ok: true,
      config: { installCargo: false, installPnpm: [] },
    });
  });

  it("rejects unknown keys at every level", () => {
    expect(rejected(GOOD + "timeout = 9999\n")).toContain("only the keys");
    expect(rejected(GOOD.replace("cargo = true", 'cargo = true\nnpm = ["x"]'))).toContain(
      "install",
    );
    expect(rejected(GOOD.replace('argv = ["pnpm"', 'env = "A=b"\nargv = ["pnpm"'))).toContain(
      "[[test]]",
    );
  });

  it("rejects a missing or unknown instance, and lite", () => {
    expect(rejected('[[test]]\nargv = ["npm", "test"]\n')).toContain("instance");
    for (const instance of ["lite", "standard-9", "huge", ""]) {
      expect(rejected(`instance = "${instance}"\n[[test]]\nargv = ["npm", "test"]\n`)).toContain(
        "instance",
      );
    }
  });

  it("rejects a file over the size bound before parsing it", () => {
    const padded = `${GOOD}\n# ${"x".repeat(MAX_CONFIG_BYTES)}\n`;
    expect(rejected(padded)).toContain("larger than");
  });

  it("rejects text that is not TOML", () => {
    expect(rejected("instance = [")).toContain("not valid TOML");
  });

  it("rejects a command given as a shell string instead of an argv array", () => {
    expect(rejected('instance = "standard-4"\n[[test]]\nargv = "cargo test"\n')).toContain("array");
    expect(rejected('instance = "standard-4"\n[[test]]\nargv = []\n')).toContain("array");
  });

  it("rejects shell metacharacters and whitespace in any argument", () => {
    for (const bad of [
      "test && rm",
      "test;rm",
      "$HOME",
      "`id`",
      "a|b",
      "a>b",
      "(x)",
      "*.rs",
      "a b",
      "a\nb",
      "'x'",
      '"x"',
      "a\\b",
      "~",
      "!x",
    ]) {
      const text = `instance = "standard-4"\n[[test]]\nargv = ["cargo", ${JSON.stringify(bad)}]\n`;
      expect(rejected(text), bad).toContain("shell characters");
    }
  });

  it("rejects a shell and any program outside the allowlist", () => {
    for (const program of ["sh", "bash", "/bin/sh", "rm", "env", "curl", "./run.sh", "node"]) {
      const text = `instance = "standard-4"\n[[test]]\nargv = [${JSON.stringify(program)}, "-c"]\n`;
      expect(rejected(text), program).toMatch(/must be pnpm test|shell characters/);
    }
  });

  it("rejects every argv shape that is not pnpm test, npm test or cargo test with listed flags", () => {
    const refused = [
      ["cargo", "test", "--config=build.rustc-wrapper=evil"],
      ["cargo", "test", "--config", "x"],
      ["cargo", "test", "--config"],
      ["cargo", "test", "--workspace", "--config"],
      ["cargo", "test", "-Zunstable-options"],
      ["cargo", "test", "-Z", "build-std"],
      ["cargo", "+nightly", "test"],
      ["cargo", "test", "--manifest-path=other/Cargo.toml"],
      ["cargo", "test", "--target=x86_64-unknown-linux-gnu"],
      ["cargo", "test", "--"],
      ["cargo", "test", "--", "--nocapture"],
      ["cargo", "build"],
      ["cargo", "run"],
      ["cargo"],
      ["cargo", "test", "--workspace", "--features=x"],
      ["RUSTC_WRAPPER=evil", "cargo", "test"],
      ["pnpm", "test", "--filter=x"],
      ["pnpm", "run", "test"],
      ["pnpm", "exec", "vitest"],
      ["pnpm"],
      ["npm", "test", "--ignore-scripts"],
      ["npm", "run", "test"],
    ];
    for (const argv of refused) {
      const text = `instance = "standard-4"\n[[test]]\nargv = ${JSON.stringify(argv)}\n`;
      expect(rejected(text), argv.join(" ")).toMatch(/must be pnpm test|shell characters/);
    }
  });

  it("accepts cargo test with each listed flag, and pnpm test and npm test", () => {
    for (const argv of [
      ["cargo", "test"],
      ["cargo", "test", "--workspace", "--locked", "--offline", "--no-fail-fast"],
      [
        "cargo",
        "test",
        "--all-targets",
        "--all-features",
        "--release",
        "--lib",
        "--bins",
        "--tests",
      ],
      ["pnpm", "test"],
      ["npm", "test"],
    ]) {
      const text = `instance = "standard-4"\n[[test]]\nargv = ${JSON.stringify(argv)}\n`;
      expect(parseGateConfig(text).ok, argv.join(" ")).toBe(true);
    }
  });

  it("rejects directories that leave the repo or are not plain paths", () => {
    for (const dir of ["..", "../x", "/etc", "a/../b", ".git", "a//b", "a/", "a b", "~"]) {
      const text = `instance = "standard-4"\n[[test]]\ndir = ${JSON.stringify(dir)}\nargv = ["npm", "test"]\n`;
      expect(rejected(text), dir).toContain("directory");
      const install = `instance = "standard-4"\n[install]\npnpm = [${JSON.stringify(dir)}]\n[[test]]\nargv = ["npm", "test"]\n`;
      expect(rejected(install), dir).toContain("directory");
    }
  });

  it("bounds the number of commands, directories and arguments", () => {
    const tests = '[[test]]\nargv = ["npm", "test"]\n'.repeat(7);
    expect(rejected(`instance = "standard-4"\n${tests}`)).toContain("test must be");
    const dirs = '["a","b","c","d","e"]';
    expect(
      rejected(
        `instance = "standard-4"\n[install]\npnpm = ${dirs}\n[[test]]\nargv = ["npm", "test"]\n`,
      ),
    ).toContain("at most");
    const many = Array.from({ length: 25 }, () => '"a"').join(",");
    expect(rejected(`instance = "standard-4"\n[[test]]\nargv = ["npm", ${many}]\n`)).toContain(
      "1 to 24",
    );
  });

  it("requires at least one test", () => {
    expect(rejected('instance = "standard-4"\n')).toContain("test must be");
  });

  it("rejects wrong types", () => {
    expect(rejected('instance = 4\n[[test]]\nargv = ["npm", "test"]\n')).toContain("instance");
    expect(
      rejected(
        'instance = "standard-4"\n[install]\ncargo = "yes"\n[[test]]\nargv = ["npm", "test"]\n',
      ),
    ).toContain("install.cargo");
    expect(rejected('instance = "standard-4"\ntest = "npm test"\n')).toContain("test must be");
  });
});
