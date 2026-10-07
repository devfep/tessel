import { describe, expect, it } from "vitest";

import { CSRF_TTL_MS, mintCsrfToken, parseReviewerEmails, verifyCsrfToken } from "./review-csrf";

const SECRET = "test-signing-key-not-a-secret";
const NOW = 1_790_000_000_000;
const SUBJECT = { email: "Felix@Example.com", repo: "demo", claim: 7 };

describe("parseReviewerEmails", () => {
  it("maps lowercased emails to agents", () => {
    const map = parseReviewerEmails(" Felix@Example.com = felix , b@x.io=bee ");
    expect([...map]).toEqual([
      ["felix@example.com", "felix"],
      ["b@x.io", "bee"],
    ]);
  });

  it.each([undefined, "", "  ", "felix@example.com", "=felix", "a@b=", "a@b=x=y", "a@b=bad name"])(
    "grants nobody for %j",
    (raw) => {
      expect(parseReviewerEmails(raw).size).toBe(0);
    },
  );

  it("skips a bad entry and keeps the good ones", () => {
    expect([...parseReviewerEmails("oops,a@b.io=ann")]).toEqual([["a@b.io", "ann"]]);
  });
});

describe("csrf token", () => {
  it("verifies for the same person, repo and claim, whatever the email's case", async () => {
    const token = await mintCsrfToken(SECRET, SUBJECT, NOW);
    expect(await verifyCsrfToken(SECRET, token, SUBJECT, NOW + 1)).toBe(true);
    expect(
      await verifyCsrfToken(SECRET, token, { ...SUBJECT, email: "felix@example.com" }, NOW),
    ).toBe(true);
  });

  it.each([
    ["another email", { email: "eve@example.com" }],
    ["another repo", { repo: "other" }],
    ["another claim", { claim: 8 }],
  ])("is refused for %s", async (_name, change) => {
    const token = await mintCsrfToken(SECRET, SUBJECT, NOW);
    expect(await verifyCsrfToken(SECRET, token, { ...SUBJECT, ...change }, NOW)).toBe(false);
  });

  it("is refused at and after expiry", async () => {
    const token = await mintCsrfToken(SECRET, SUBJECT, NOW);
    expect(await verifyCsrfToken(SECRET, token, SUBJECT, NOW + CSRF_TTL_MS - 1)).toBe(true);
    expect(await verifyCsrfToken(SECRET, token, SUBJECT, NOW + CSRF_TTL_MS)).toBe(false);
  });

  it("is refused under another key", async () => {
    const token = await mintCsrfToken(SECRET, SUBJECT, NOW);
    expect(await verifyCsrfToken("another-key", token, SUBJECT, NOW)).toBe(false);
  });

  it("is refused when the expiry is edited to extend it", async () => {
    const token = await mintCsrfToken(SECRET, SUBJECT, NOW);
    const [, mac] = token.split(".");
    expect(await verifyCsrfToken(SECRET, `${NOW + 10 * CSRF_TTL_MS}.${mac}`, SUBJECT, NOW)).toBe(
      false,
    );
  });

  it.each([undefined, null, 5, "", "x", "1.2.3", ".", "abc.def", `${NOW + 9}.@@@`])(
    "is refused for the malformed token %j",
    async (token) => {
      expect(await verifyCsrfToken(SECRET, token, SUBJECT, NOW)).toBe(false);
    },
  );
});
