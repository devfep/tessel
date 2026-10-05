import { describe, expect, it } from "vitest";

import { isForkRepo, mintForkWriteToken, type TokenHandle } from "./token-policy";

interface Info {
  source: string | null;
}

function fakeHandle(source: string | null, infoError?: Error) {
  const calls: string[] = [];
  const handle: TokenHandle<Info, string> = {
    async info() {
      calls.push("info");
      if (infoError) {
        throw infoError;
      }
      return { source };
    },
    async createToken(scope, ttlSeconds) {
      calls.push(`createToken ${scope} ${ttlSeconds}`);
      return "token";
    },
  };
  return { calls, handle };
}

describe("isForkRepo", () => {
  it("accepts a fork of an Artifacts repo", () => {
    expect(isForkRepo({ source: "artifacts:tessel/demo" })).toBe(true);
  });

  it("rejects a repo that is not a fork", () => {
    expect(isForkRepo({ source: null })).toBe(false);
  });

  it("rejects an imported repo", () => {
    expect(isForkRepo({ source: "github:owner/repo" })).toBe(false);
  });

  it("rejects an empty source", () => {
    expect(isForkRepo({ source: "" })).toBe(false);
  });
});

describe("mintForkWriteToken", () => {
  it("reads info before creating the token", async () => {
    const { calls, handle } = fakeHandle("artifacts:tessel/demo");
    const outcome = await mintForkWriteToken(handle, 3600);
    expect(calls).toEqual(["info", "createToken write 3600"]);
    expect(outcome).toEqual({
      minted: true,
      info: { source: "artifacts:tessel/demo" },
      token: "token",
    });
  });

  it("never creates a token for a repo that is not a fork", async () => {
    const { calls, handle } = fakeHandle(null);
    expect(await mintForkWriteToken(handle, 3600)).toEqual({ minted: false });
    expect(calls).toEqual(["info"]);
  });

  it("never creates a token for an imported repo", async () => {
    const { calls, handle } = fakeHandle("github:owner/repo");
    expect(await mintForkWriteToken(handle, 3600)).toEqual({ minted: false });
    expect(calls).toEqual(["info"]);
  });

  it("creates no token and propagates the error when info throws", async () => {
    const failure = new Error("info down");
    const { calls, handle } = fakeHandle("artifacts:tessel/demo", failure);
    await expect(mintForkWriteToken(handle, 3600)).rejects.toBe(failure);
    expect(calls).toEqual(["info"]);
  });
});
