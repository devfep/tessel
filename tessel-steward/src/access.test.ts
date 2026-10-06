import { beforeAll, beforeEach, describe, expect, it } from "vitest";

import {
  CERTS_TTL_MS,
  clearAccessKeyCache,
  readAccessConfig,
  verifyAccessToken,
  type AccessVerdict,
} from "./access";
import {
  AUD,
  CONFIG,
  NOW_MS,
  TEAM,
  TestSigner,
  VIEWER,
  validClaims,
  type Claims,
} from "./access-fixtures";

let signer: TestSigner;

beforeAll(async () => {
  signer = await TestSigner.create();
});

beforeEach(() => {
  clearAccessKeyCache();
});

function verify(token: string | null, fetchImpl = signer.certsFetch()): Promise<AccessVerdict> {
  return verifyAccessToken(token, CONFIG, { fetchImpl, now: () => NOW_MS });
}

describe("verifyAccessToken", () => {
  it("accepts a valid token and returns the email", async () => {
    expect(await verify(await signer.sign(validClaims()))).toEqual({ ok: true, email: VIEWER });
  });

  it("accepts aud as a plain string and email in another case", async () => {
    const token = await signer.sign(validClaims({ aud: AUD, email: VIEWER.toUpperCase() }));
    expect(await verify(token)).toMatchObject({ ok: true });
  });

  it.each<[string, Claims, number]>([
    ["a different audience", { aud: ["other-app"] }, 401],
    ["a different issuer", { iss: "https://evil.cloudflareaccess.com" }, 401],
    ["an expired token", { exp: NOW_MS / 1000 - 1 }, 401],
    ["a token valid only from later", { nbf: NOW_MS / 1000 + 60 }, 401],
    ["no exp", { exp: undefined }, 401],
    ["no email", { email: undefined }, 401],
    ["an email that is not listed", { email: "mallory@example.com" }, 403],
  ])("rejects %s", async (_label, overrides, status) => {
    const verdict = await verify(await signer.sign(validClaims(overrides)));
    expect(verdict).toMatchObject({ ok: false, status });
  });

  it("rejects a token signed by another key", async () => {
    const impostor = await TestSigner.create();
    expect(await verify(await impostor.sign(validClaims()))).toMatchObject({
      ok: false,
      status: 401,
      reason: "bad signature",
    });
  });

  it("rejects a token whose payload was changed after signing", async () => {
    const [head, , signature] = (await signer.sign(validClaims())).split(".");
    const forged = btoa(JSON.stringify(validClaims({ email: "mallory@example.com" })));
    const verdict = await verify(`${head}.${forged}.${signature}`);
    expect(verdict).toMatchObject({ ok: false, status: 401 });
  });

  it("rejects alg none and other algorithms", async () => {
    for (const alg of ["none", "HS256"]) {
      const verdict = await verify(await signer.sign(validClaims(), { alg }));
      expect(verdict).toMatchObject({ ok: false, status: 401 });
    }
  });

  it("rejects an unknown kid", async () => {
    const verdict = await verify(await signer.sign(validClaims(), { kid: "nope" }));
    expect(verdict).toMatchObject({ ok: false, status: 401, reason: "unknown signing key" });
  });

  it.each([null, "", "abc", "a.b", "a.b.c.d", "!!!.???.***"])("rejects %j", async (token) => {
    expect(await verify(token)).toMatchObject({ ok: false, status: 401 });
  });

  it("answers 503 when the certs cannot be fetched", async () => {
    const failing = (() => Promise.resolve(new Response("x", { status: 500 }))) as typeof fetch;
    const verdict = await verify(await signer.sign(validClaims()), failing);
    expect(verdict).toMatchObject({ ok: false, status: 503 });
  });

  it("fetches the certs from the team domain and reuses them within the TTL", async () => {
    const urls: string[] = [];
    const fetchImpl = signer.certsFetch(urls);
    const token = await signer.sign(validClaims());
    await verify(token, fetchImpl);
    await verify(token, fetchImpl);
    expect(urls).toEqual([`https://${TEAM}/cdn-cgi/access/certs`]);

    await verifyAccessToken(token, CONFIG, { fetchImpl, now: () => NOW_MS + CERTS_TTL_MS + 1 });
    expect(urls).toHaveLength(2);
  });
});

describe("readAccessConfig", () => {
  const full = {
    ACCESS_TEAM_DOMAIN: `https://${TEAM}`,
    ACCESS_AUD: AUD,
    DASHBOARD_VIEWERS: " Felix@Example.com, other@example.com ,",
  };

  it("normalises the domain and the viewer list", () => {
    expect(readAccessConfig(full)).toEqual({
      teamDomain: TEAM,
      audience: AUD,
      viewers: ["felix@example.com", "other@example.com"],
    });
  });

  it.each([
    ["no team domain", { ...full, ACCESS_TEAM_DOMAIN: undefined }],
    ["a blank team domain", { ...full, ACCESS_TEAM_DOMAIN: "  " }],
    ["no AUD tag", { ...full, ACCESS_AUD: undefined }],
    ["an empty AUD tag", { ...full, ACCESS_AUD: "" }],
  ])("is undefined with %s", (_label, env) => {
    expect(readAccessConfig(env)).toBeUndefined();
  });

  it("allows nobody when the viewer list is empty", () => {
    expect(readAccessConfig({ ...full, DASHBOARD_VIEWERS: undefined })?.viewers).toEqual([]);
  });
});
