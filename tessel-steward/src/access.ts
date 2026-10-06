/** How long the team's signing keys are reused before they are fetched again. */
export const CERTS_TTL_MS = 5 * 60 * 1000;
/** An unknown `kid` may mean the team rotated its keys; refetch for it, but not more often. */
export const CERTS_REFETCH_MIN_MS = 60 * 1000;

export interface AccessConfig {
  /** `<team>.cloudflareaccess.com`: the issuer and the host that serves the signing keys. */
  teamDomain: string;
  /** The Access application's AUD tag; the token's `aud` must include it. */
  audience: string;
  /** Emails allowed to view, compared case-insensitively. */
  viewers: readonly string[];
}

export type AccessVerdict =
  | { ok: true; email: string }
  | { ok: false; status: 401 | 403 | 503; reason: string };

export interface AccessDeps {
  fetchImpl?: typeof fetch;
  /** Unix milliseconds. */
  now?: () => number;
}

interface CachedKeys {
  fetchedAtMs: number;
  keys: Map<string, CryptoKey>;
}

const keysByTeam = new Map<string, CachedKeys>();
/** When each team's certs were last requested, whether or not the request succeeded. */
const lastFetchAttemptMs = new Map<string, number>();

/** Forgets cached signing keys. For tests. */
export function clearAccessKeyCache(): void {
  keysByTeam.clear();
  lastFetchAttemptMs.clear();
}

/**
 * Reads the Access settings from the Worker's vars. Returns `undefined` when the team domain or
 * the AUD tag is unset or blank, so the caller can fail closed.
 */
export function readAccessConfig(env: {
  ACCESS_TEAM_DOMAIN?: string | undefined;
  ACCESS_AUD?: string | undefined;
  DASHBOARD_VIEWERS?: string | undefined;
}): AccessConfig | undefined {
  const teamDomain = (env.ACCESS_TEAM_DOMAIN ?? "").trim().replace(/^https:\/\//, "");
  const audience = (env.ACCESS_AUD ?? "").trim();
  if (teamDomain === "" || audience === "") {
    return undefined;
  }
  const viewers = (env.DASHBOARD_VIEWERS ?? "")
    .split(",")
    .map((email) => email.trim().toLowerCase())
    .filter((email) => email !== "");
  return { teamDomain, audience, viewers };
}

function fromBase64Url(text: string): Uint8Array<ArrayBuffer> {
  const padded = text.replaceAll("-", "+").replaceAll("_", "/");
  const binary = atob(padded + "=".repeat((4 - (padded.length % 4)) % 4));
  return Uint8Array.from(binary, (char) => char.codePointAt(0) ?? 0);
}

function decodeJson(text: string): Record<string, unknown> | undefined {
  try {
    const value: unknown = JSON.parse(new TextDecoder().decode(fromBase64Url(text)));
    if (typeof value === "object" && value !== null && !Array.isArray(value)) {
      return value as Record<string, unknown>;
    }
  } catch {
    // Not base64url or not JSON: the caller rejects the token.
  }
  return undefined;
}

async function loadKeys(
  teamDomain: string,
  fetchImpl: typeof fetch,
  nowMs: number,
  refresh = false,
): Promise<Map<string, CryptoKey>> {
  const cached = keysByTeam.get(teamDomain);
  if (!refresh && cached !== undefined && nowMs - cached.fetchedAtMs < CERTS_TTL_MS) {
    return cached.keys;
  }
  lastFetchAttemptMs.set(teamDomain, nowMs);
  const response = await fetchImpl(`https://${teamDomain}/cdn-cgi/access/certs`);
  if (!response.ok) {
    throw new Error(`certs endpoint answered ${response.status}`);
  }
  const body = (await response.json()) as { keys?: (JsonWebKey & { kid?: string })[] };
  const keys = new Map<string, CryptoKey>();
  for (const jwk of body.keys ?? []) {
    if (jwk.kty !== "RSA" || jwk.kid === undefined) {
      continue;
    }
    const key = await crypto.subtle.importKey(
      "jwk",
      jwk,
      { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
      false,
      ["verify"],
    );
    keys.set(jwk.kid, key);
  }
  keysByTeam.set(teamDomain, { fetchedAtMs: nowMs, keys });
  return keys;
}

function denied(status: 401 | 403 | 503, reason: string): AccessVerdict {
  return { ok: false, status, reason };
}

function audienceIncludes(aud: unknown, expected: string): boolean {
  if (typeof aud === "string") {
    return aud === expected;
  }
  return Array.isArray(aud) && aud.includes(expected);
}

function checkClaims(
  claims: Record<string, unknown>,
  config: AccessConfig,
  nowSeconds: number,
): AccessVerdict {
  if (claims["iss"] !== `https://${config.teamDomain}`) {
    return denied(401, "token issuer is not this team");
  }
  if (!audienceIncludes(claims["aud"], config.audience)) {
    return denied(401, "token is for another application");
  }
  const { exp, nbf, email } = claims;
  if (typeof exp !== "number" || exp <= nowSeconds) {
    return denied(401, "token expired");
  }
  if (nbf !== undefined && (typeof nbf !== "number" || nbf > nowSeconds)) {
    return denied(401, "token not yet valid");
  }
  if (typeof email !== "string" || email === "") {
    return denied(401, "token carries no email");
  }
  // Some non-ASCII characters lowercase to ASCII ones (U+212A to "k"), so they must not reach
  // the compare.
  const isAscii = [...email].every((char) => (char.codePointAt(0) ?? 128) < 128);
  if (!isAscii || !config.viewers.includes(email.toLowerCase())) {
    return denied(403, "this account may not view the dashboard");
  }
  return { ok: true, email };
}

/**
 * Verifies a `Cf-Access-Jwt-Assertion` token: RS256 signature against the team's published keys,
 * issuer, audience, expiry and not-before, then the email against the viewer list.
 *
 * @param token The header value; `null` when the header is absent.
 * @returns The viewer's email, or a 401 (no usable token), 403 (valid token, email not listed)
 *   or 503 (the team's keys could not be fetched; fails closed).
 */
export async function verifyAccessToken(
  token: string | null,
  config: AccessConfig,
  { fetchImpl = fetch, now = Date.now }: AccessDeps = {},
): Promise<AccessVerdict> {
  if (token === null || token === "") {
    return denied(401, "missing Cf-Access-Jwt-Assertion");
  }
  const [header, payload, signature, ...rest] = token.split(".");
  if (header === undefined || payload === undefined || signature === undefined || rest.length > 0) {
    return denied(401, "malformed token");
  }
  const head = decodeJson(header);
  const claims = decodeJson(payload);
  if (head === undefined || claims === undefined || head["alg"] !== "RS256") {
    return denied(401, "malformed token");
  }
  const kid = head["kid"];
  let keys: Map<string, CryptoKey>;
  try {
    keys = await loadKeys(config.teamDomain, fetchImpl, now());
  } catch (error) {
    console.error(JSON.stringify({ event: "access_certs_failed", message: String(error) }));
    return denied(503, "could not fetch the team's signing keys");
  }
  let key = typeof kid === "string" ? keys.get(kid) : undefined;
  const attemptedAtMs = lastFetchAttemptMs.get(config.teamDomain) ?? 0;
  if (
    key === undefined &&
    typeof kid === "string" &&
    now() - attemptedAtMs >= CERTS_REFETCH_MIN_MS
  ) {
    try {
      key = (await loadKeys(config.teamDomain, fetchImpl, now(), true)).get(kid);
    } catch (error) {
      console.error(JSON.stringify({ event: "access_certs_failed", message: String(error) }));
      return denied(503, "could not fetch the team's signing keys");
    }
  }
  if (key === undefined) {
    return denied(401, "unknown signing key");
  }
  const signed = new TextEncoder().encode(`${header}.${payload}`);
  let valid = false;
  try {
    valid = await crypto.subtle.verify("RSASSA-PKCS1-v1_5", key, fromBase64Url(signature), signed);
  } catch {
    // A signature that is not valid base64url fails verification.
  }
  if (!valid) {
    return denied(401, "bad signature");
  }
  return checkClaims(claims, config, Math.floor(now() / 1000));
}
