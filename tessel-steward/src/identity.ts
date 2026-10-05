/** How long an identity token lives. */
export const IDENTITY_TTL_MS = 24 * 60 * 60 * 1000;

const TOKEN_VERSION = 1;
const MAX_NAME_LENGTH = 128;
const NAME_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;

/**
 * Whether `name` can be a repo or an agent id: 1 to 128 characters from `A-Z a-z 0-9 . _ -`,
 * starting with a letter or digit. The coordinator accepts the same characters in an agent id
 * and carries it in an HTTP header.
 */
export function isValidName(name: string): boolean {
  return name.length <= MAX_NAME_LENGTH && NAME_PATTERN.test(name);
}

export interface IdentityClaims {
  repo: string;
  agent: string;
  expMs: number;
}

function toBase64Url(bytes: Uint8Array): string {
  const binary = String.fromCharCode(...bytes);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

/**
 * Signs an identity token for one agent on one repo. The format is documented once, in the
 * doc comment of `src/identity.rs` in the coordinator, which verifies it:
 * `<payload>.<mac>` with payload the base64url JSON `{"v":1,"repo","agent","exp_ms"}` and mac
 * HMAC-SHA256 of the payload string keyed with `signingKey`.
 *
 * @param signingKey The `IDENTITY_SIGNING_KEY` secret; must not be empty.
 * @param claims Repo, agent and expiry (Unix milliseconds) to sign.
 * @throws If the key is empty, a name is invalid or the expiry is not a safe integer.
 */
export async function signIdentityToken(
  signingKey: string,
  { repo, agent, expMs }: IdentityClaims,
): Promise<string> {
  if (signingKey === "") {
    throw new Error("IDENTITY_SIGNING_KEY is empty");
  }
  if (!isValidName(repo) || !isValidName(agent)) {
    throw new Error("repo and agent must be 1 to 128 characters of A-Z a-z 0-9 . _ -");
  }
  if (!Number.isSafeInteger(expMs) || expMs <= 0) {
    throw new Error("expMs must be a positive safe integer");
  }
  const encoder = new TextEncoder();
  const json = JSON.stringify({ v: TOKEN_VERSION, repo, agent, exp_ms: expMs });
  const payload = toBase64Url(encoder.encode(json));
  const key = await crypto.subtle.importKey(
    "raw",
    encoder.encode(signingKey),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const mac = await crypto.subtle.sign("HMAC", key, encoder.encode(payload));
  return `${payload}.${toBase64Url(new Uint8Array(mac))}`;
}
