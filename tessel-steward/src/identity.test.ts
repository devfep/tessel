import { describe, expect, it } from "vitest";

import { IDENTITY_TTL_MS, isValidName, signIdentityToken } from "./identity";

const KEY = "test-signing-key-not-a-secret";

describe("signIdentityToken", () => {
  // The same vector is asserted in the `identity` tests of the coordinator (src/identity.rs).
  it("signs the vector the coordinator verifies", async () => {
    const token = await signIdentityToken(KEY, {
      repo: "demo",
      agent: "agent-1",
      expMs: 1_790_000_000_000,
    });
    expect(token).toBe(
      "eyJ2IjoxLCJyZXBvIjoiZGVtbyIsImFnZW50IjoiYWdlbnQtMSIsImV4cF9tcyI6MTc5MDAwMDAwMDAwMH0.Cz0zjmq3Tc7-jsR5M2Ss4FRRkJnwFdLgoh4_1C74Omo",
    );
  });

  it("puts the claims in the payload in the documented key order", async () => {
    const token = await signIdentityToken(KEY, { repo: "r", agent: "a", expMs: 5 });
    const [payload] = token.split(".");
    const base64 = (payload ?? "").replaceAll("-", "+").replaceAll("_", "/");
    const json = atob(base64);
    expect(json).toBe('{"v":1,"repo":"r","agent":"a","exp_ms":5}');
  });

  it("changes with the key", async () => {
    const claims = { repo: "demo", agent: "a1", expMs: 10 };
    const one = await signIdentityToken("key-one", claims);
    const two = await signIdentityToken("key-two", claims);
    expect(one.split(".")[0]).toBe(two.split(".")[0]);
    expect(one.split(".")[1]).not.toBe(two.split(".")[1]);
  });

  it("emits url-safe base64 without padding", async () => {
    const token = await signIdentityToken(KEY, { repo: "demo", agent: "a1", expMs: 10 });
    expect(token).toMatch(/^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]{43}$/);
  });

  it("refuses an empty key", async () => {
    await expect(signIdentityToken("", { repo: "demo", agent: "a1", expMs: 10 })).rejects.toThrow(
      "IDENTITY_SIGNING_KEY is empty",
    );
  });

  it.each([
    ["an empty agent", { repo: "demo", agent: "", expMs: 10 }],
    ["an agent with a space", { repo: "demo", agent: "a b", expMs: 10 }],
    ["a repo with a slash", { repo: "a/b", agent: "a1", expMs: 10 }],
    ["a zero expiry", { repo: "demo", agent: "a1", expMs: 0 }],
    ["a fractional expiry", { repo: "demo", agent: "a1", expMs: 1.5 }],
    ["an unsafe expiry", { repo: "demo", agent: "a1", expMs: 2 ** 53 }],
  ])("refuses %s", async (_label, claims) => {
    await expect(signIdentityToken(KEY, claims)).rejects.toThrow();
  });
});

describe("isValidName", () => {
  it.each(["a", "agent-1", "demo--agent.1_x", "A9", "a".repeat(128)])("accepts %s", (name) => {
    expect(isValidName(name)).toBe(true);
  });

  it.each([
    ["empty", ""],
    ["too long", "a".repeat(129)],
    ["a leading dash", "-a"],
    ["a leading dot", ".a"],
    ["a leading underscore", "_a"],
    ["a space", "a b"],
    ["a slash", "a/b"],
    ["a percent escape", "a%20b"],
    ["a newline", "a\nb"],
    ["a trailing newline", "a1\n"],
    ["a non-ASCII letter", "ägent"],
  ])("rejects %s", (_label, name) => {
    expect(isValidName(name)).toBe(false);
  });
});

describe("IDENTITY_TTL_MS", () => {
  it("is 24 hours", () => {
    expect(IDENTITY_TTL_MS).toBe(86_400_000);
  });
});
