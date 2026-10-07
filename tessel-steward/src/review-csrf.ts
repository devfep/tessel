import { isValidName } from "./identity";

/** How long a page's decision tokens stay valid. */
export const CSRF_TTL_MS = 60 * 60 * 1000;

const DOMAIN = "tessel-review-csrf:v1";
const encoder = new TextEncoder();

/**
 * Reads `REVIEWER_EMAILS`: comma-separated `email=agent` pairs, emails compared case-insensitively.
 * An entry that is not a pair with a valid agent id is left out, so a typo never grants a
 * decision. Unset or blank means nobody may decide.
 */
export function parseReviewerEmails(raw: string | undefined): Map<string, string> {
  const reviewers = new Map<string, string>();
  for (const entry of (raw ?? "").split(",")) {
    const [email, agent, ...extra] = entry.split("=").map((part) => part.trim());
    if (email === undefined || email === "" || agent === undefined || extra.length > 0) {
      continue;
    }
    if (isValidName(agent)) {
      reviewers.set(email.toLowerCase(), agent);
    }
  }
  return reviewers;
}

function toBase64Url(bytes: Uint8Array): string {
  return btoa(String.fromCharCode(...bytes))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replaceAll("=", "");
}

function fromBase64Url(text: string): Uint8Array | undefined {
  try {
    const binary = atob(text.replaceAll("-", "+").replaceAll("_", "/"));
    return Uint8Array.from(binary, (char) => char.charCodeAt(0));
  } catch {
    return undefined;
  }
}

function key(secret: string): Promise<CryptoKey> {
  return crypto.subtle.importKey(
    "raw",
    encoder.encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign", "verify"],
  );
}

function message(email: string, repo: string, claim: number, expMs: number): Uint8Array {
  return encoder.encode(`${DOMAIN}|${email.toLowerCase()}|${repo}|${claim}|${expMs}`);
}

/**
 * A token that lets the signed-in `email` decide `claim` of `repo` until it expires, as
 * `<expMs>.<mac>`.
 * The MAC is HMAC-SHA256 under `secret` over a domain-separated message, so a token cannot be used
 * for another person, repo or claim, and an identity token cannot be used as one.
 */
export async function mintCsrfToken(
  secret: string,
  subject: { email: string; repo: string; claim: number },
  nowMs: number,
): Promise<string> {
  const expMs = nowMs + CSRF_TTL_MS;
  const { email, repo, claim } = subject;
  const mac = await crypto.subtle.sign(
    "HMAC",
    await key(secret),
    message(email, repo, claim, expMs),
  );
  return `${expMs}.${toBase64Url(new Uint8Array(mac))}`;
}

/** Whether `token` was minted for exactly this person, repo and claim and has not expired. */
export async function verifyCsrfToken(
  secret: string,
  token: unknown,
  subject: { email: string; repo: string; claim: number },
  nowMs: number,
): Promise<boolean> {
  if (typeof token !== "string") {
    return false;
  }
  const [expText, macText, ...extra] = token.split(".");
  if (
    expText === undefined ||
    macText === undefined ||
    extra.length > 0 ||
    !/^\d+$/.test(expText)
  ) {
    return false;
  }
  const expMs = Number(expText);
  const mac = fromBase64Url(macText);
  if (mac === undefined || !Number.isSafeInteger(expMs) || expMs <= nowMs) {
    return false;
  }
  const { email, repo, claim } = subject;
  return crypto.subtle.verify("HMAC", await key(secret), mac, message(email, repo, claim, expMs));
}
