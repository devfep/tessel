import { describe, expect, it } from "vitest";

import {
  LEGACY_IMAGE,
  TOOLCHAIN_IMAGE,
  readGatePlan,
  startOptions,
  type GatePlan,
} from "./gate-plan";
import { parseSha } from "./merge-types";
import { MAX_CONFIG_BYTES } from "./tessel-config";

const COMMIT = (() => {
  const sha = parseSha("a".repeat(40));
  if (sha === undefined) {
    throw new Error("bad sha");
  }
  return sha;
})();

const GOOD = 'instance = "standard-4"\n[[test]]\nargv = ["cargo", "test", "--locked"]\n';

function source(read: (args: { ref: string; path: string }) => Promise<Blob | null>) {
  return { readFile: read };
}

async function failsWith(): Promise<Blob | null> {
  throw Object.assign(new Error("down"), { code: "INTERNAL_ERROR" });
}

describe("readGatePlan", () => {
  it("reads tessel.toml at the commit it is given", async () => {
    const calls: Array<{ ref: string; path: string }> = [];
    const plan = await readGatePlan(
      source(async (args) => {
        calls.push(args);
        return new Blob([GOOD]);
      }),
      COMMIT,
    );
    expect(calls).toEqual([{ ref: COMMIT, path: "tessel.toml" }]);
    expect(plan).toMatchObject({ kind: "configured", config: { instance: "standard-4" } });
  });

  it("is legacy, 'missing', when main has no tessel.toml", async () => {
    expect(
      await readGatePlan(
        source(async () => null),
        COMMIT,
      ),
    ).toEqual({
      kind: "legacy",
      issue: "missing",
    });
  });

  it("is legacy, 'invalid', for a file that is invalid, oversized, not UTF-8, or too big to read", async () => {
    const cases: Array<() => Promise<Blob | null>> = [
      async () => new Blob(['instance = "lite"\n']),
      async () => new Blob(["x".repeat(MAX_CONFIG_BYTES + 1)]),
      async () => new Blob([new Uint8Array([0xff, 0xfe, 0x00])]),
      async () => {
        throw Object.assign(new Error("too big"), { code: "MEMORY_LIMIT" });
      },
    ];
    for (const read of cases) {
      expect(await readGatePlan(source(read), COMMIT)).toEqual({
        kind: "legacy",
        issue: "invalid",
      });
    }
  });

  it("does not read the bytes of a file over the size bound", async () => {
    let bytesRead = false;
    const oversized = {
      size: MAX_CONFIG_BYTES + 1,
      arrayBuffer: async () => {
        bytesRead = true;
        return new TextEncoder().encode(GOOD).buffer;
      },
    } as unknown as Blob;
    expect(
      await readGatePlan(
        source(async () => oversized),
        COMMIT,
      ),
    ).toEqual({
      kind: "legacy",
      issue: "invalid",
    });
    expect(bytesRead).toBe(false);
  });

  it("throws on an unexpected Artifacts failure instead of guessing a gate", async () => {
    await expect(readGatePlan(source(failsWith), COMMIT)).rejects.toThrow("down");
  });
});

describe("startOptions", () => {
  const images = { [LEGACY_IMAGE]: "lite-ref", [TOOLCHAIN_IMAGE]: "toolchain-ref" };
  const configured: GatePlan = {
    kind: "configured",
    config: { instance: "standard-4", installCargo: false, installPnpm: [], tests: [] },
  };

  it("starts a configured repo on the toolchain image and the instance it asked for", () => {
    expect(startOptions(configured, images)).toEqual({
      image: "toolchain-ref",
      enableInternet: false,
      instance: "standard-4",
    });
  });

  it("starts a repo without a gate on lite, naming the instance explicitly, with no Internet", () => {
    for (const issue of ["missing", "invalid"] as const) {
      expect(startOptions({ kind: "legacy", issue }, images)).toEqual({
        image: "lite-ref",
        enableInternet: false,
        instance: "lite",
      });
    }
  });

  it("throws when the image a plan needs is not configured", () => {
    expect(() => startOptions(configured, { [LEGACY_IMAGE]: "x" })).toThrow("toolchain");
    expect(() => startOptions({ kind: "legacy", issue: "missing" }, {})).toThrow("tests");
  });
});
