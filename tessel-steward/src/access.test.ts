import {
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
  type MockInstance,
} from "vitest";

import {
  CERTS_REFETCH_MIN_MS,
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
  NOW_S,
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

  it("rejects an email that only matches after Unicode case folding", async () => {
    // U+212A (Kelvin sign) lowercases to "k".
    const viewers = ["kim@example.com"];
    const token = await signer.sign(validClaims({ email: "\u212Aim@example.com" }));
    const verdict = await verifyAccessToken(
      token,
      { ...CONFIG, viewers },
      { fetchImpl: signer.certsFetch(), now: () => NOW_MS },
    );
    expect(verdict).toMatchObject({ ok: false, status: 403 });
  });

  it("refetches the certs once for an unknown kid, at most once a minute", async () => {
    const rotated = await TestSigner.create("kid-2");
    let published = [signer];
    let fetches = 0;
    const fetchImpl = (() => {
      fetches += 1;
      const keys = published.map((s) => ({ ...s.publicJwk, kid: s.kid }));
      return Promise.resolve(Response.json({ keys }));
    }) as typeof fetch;
    const at = (offsetMs: number) => ({ fetchImpl, now: () => NOW_MS + offsetMs });
    const old = await signer.sign(validClaims());
    const fresh = await rotated.sign(validClaims());

    expect(await verifyAccessToken(old, CONFIG, at(0))).toMatchObject({ ok: true });
    published = [signer, rotated];
    expect(await verifyAccessToken(fresh, CONFIG, at(30_000))).toMatchObject({ status: 401 });
    expect(fetches).toBe(1);
    expect(await verifyAccessToken(fresh, CONFIG, at(61_000))).toMatchObject({ ok: true });
    expect(fetches).toBe(2);
  });

  it("keeps the one-a-minute limit while the certs endpoint is down", async () => {
    let healthy = true;
    let fetches = 0;
    const good = signer.certsFetch();
    const fetchImpl = ((input: RequestInfo | URL) => {
      fetches += 1;
      return healthy ? good(input) : Promise.resolve(new Response("x", { status: 500 }));
    }) as typeof fetch;
    const at = (offsetMs: number) => ({ fetchImpl, now: () => NOW_MS + offsetMs });
    const unknown = await signer.sign(validClaims(), { kid: "nope" });

    await verifyAccessToken(await signer.sign(validClaims()), CONFIG, at(0));
    healthy = false;
    expect(await verifyAccessToken(unknown, CONFIG, at(61_000))).toMatchObject({ status: 401 });
    expect(fetches).toBe(2);
    expect(await verifyAccessToken(unknown, CONFIG, at(62_000))).toMatchObject({ status: 401 });
    expect(await verifyAccessToken(unknown, CONFIG, at(100_000))).toMatchObject({ status: 401 });
    expect(fetches).toBe(2);
    expect(await verifyAccessToken(unknown, CONFIG, at(122_000))).toMatchObject({ status: 401 });
    expect(fetches).toBe(3);
  });

  it("answers 401, not 503, when the refetch for an unknown kid fails with a cache", async () => {
    await verify(await signer.sign(validClaims()));
    const failing = (() => Promise.resolve(new Response("x", { status: 500 }))) as typeof fetch;
    const unknown = await signer.sign(validClaims(), { kid: "nope" });
    const verdict = await verifyAccessToken(unknown, CONFIG, {
      fetchImpl: failing,
      now: () => NOW_MS + CERTS_REFETCH_MIN_MS + 1_000,
    });
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

/** Fills the cache at `NOW_MS`, then serves `next` for every later fetch and counts them. */
async function primedFetch(next: () => Promise<Response>) {
  const good = signer.certsFetch();
  let fetches = 0;
  let serveGood = true;
  const fetchImpl = ((input: RequestInfo | URL) => {
    if (serveGood) {
      return good(input);
    }
    fetches += 1;
    return next();
  }) as typeof fetch;
  const token = await signer.sign(validClaims({ exp: NOW_S + 3_600 }));
  await verifyAccessToken(token, CONFIG, { fetchImpl, now: () => NOW_MS });
  serveGood = false;
  return {
    token,
    fetchImpl,
    fetches: () => fetches,
    at: (ms: number) => ({ fetchImpl, now: () => ms }),
  };
}

describe("verifyAccessToken with a stale key cache", () => {
  const AFTER_TTL_MS = NOW_MS + CERTS_TTL_MS + 1;
  const failures = {
    "a 500 answer": () => Promise.resolve(new Response("x", { status: 500 })),
    "a rejected fetch": () => Promise.reject(new Error("network down")),
    "a body that is not JSON": () => Promise.resolve(new Response("<html>", { status: 200 })),
  };
  let errorSpy: MockInstance<typeof console.error>;

  beforeEach(() => {
    errorSpy = vi.spyOn(console, "error").mockImplementation(() => undefined);
  });

  afterEach(() => {
    errorSpy.mockRestore();
  });

  const staleLines = () =>
    errorSpy.mock.calls
      .map(([line]) => String(line))
      .filter((line) => line.includes("access_certs_stale"));

  it.each(Object.entries(failures))("serves the cached keys after %s", async (_label, next) => {
    const primed = await primedFetch(next);
    const verdict = await verifyAccessToken(primed.token, CONFIG, primed.at(AFTER_TTL_MS));
    expect(verdict).toEqual({ ok: true, email: VIEWER });
    expect(primed.fetches()).toBe(1);
    expect(staleLines()).toHaveLength(1);
    const logged = JSON.parse(staleLines()[0] ?? "{}") as Record<string, unknown>;
    expect(logged).toMatchObject({ event: "access_certs_stale", team: TEAM });
    expect(typeof logged["message"]).toBe("string");
    expect(staleLines()[0]).not.toContain(primed.token);
  });

  it("does not fetch again within the minute after a failed refetch", async () => {
    const primed = await primedFetch(failures["a 500 answer"]);
    await verifyAccessToken(primed.token, CONFIG, primed.at(AFTER_TTL_MS));
    const later = AFTER_TTL_MS + CERTS_REFETCH_MIN_MS - 1;
    const verdict = await verifyAccessToken(primed.token, CONFIG, primed.at(later));
    expect(verdict).toMatchObject({ ok: true });
    expect(primed.fetches()).toBe(1);
    expect(staleLines()).toHaveLength(1);
  });

  it("fetches again after the minute and replaces the cache when the endpoint is back", async () => {
    const rotated = await TestSigner.create("kid-2");
    const primed = await primedFetch(failures["a 500 answer"]);
    await verifyAccessToken(primed.token, CONFIG, primed.at(AFTER_TTL_MS));

    const retryAt = AFTER_TTL_MS + CERTS_REFETCH_MIN_MS;
    const recovered = rotated.certsFetch();
    const first = await verifyAccessToken(await rotated.sign(validClaims()), CONFIG, {
      fetchImpl: recovered,
      now: () => retryAt,
    });
    expect(first).toMatchObject({ ok: true });

    const old = await verifyAccessToken(primed.token, CONFIG, {
      fetchImpl: recovered,
      now: () => retryAt + 1_000,
    });
    expect(old).toMatchObject({ ok: false, status: 401 });
  });

  it("retries on the next request after the minute when the endpoint is still down", async () => {
    const primed = await primedFetch(failures["a rejected fetch"]);
    await verifyAccessToken(primed.token, CONFIG, primed.at(AFTER_TTL_MS));
    const retryAt = AFTER_TTL_MS + CERTS_REFETCH_MIN_MS;
    const verdict = await verifyAccessToken(primed.token, CONFIG, primed.at(retryAt));
    expect(verdict).toMatchObject({ ok: true });
    expect(primed.fetches()).toBe(2);
    expect(staleLines()).toHaveLength(2);
  });

  it("answers 503 when there is no cache and the endpoint fails", async () => {
    const failing = (() => Promise.resolve(new Response("x", { status: 500 }))) as typeof fetch;
    const verdict = await verify(await signer.sign(validClaims()), failing);
    expect(verdict).toMatchObject({ ok: false, status: 503 });
    expect(staleLines()).toHaveLength(0);
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
