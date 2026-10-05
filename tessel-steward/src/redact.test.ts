import { describe, expect, it } from "vitest";

import { redactTokens } from "./redact";

describe("redactTokens", () => {
  it("replaces a token wherever it appears", () => {
    const text = "fetch https://x/art_v1_abcDEF123-_.~+/=?y failed; Bearer art_v0_zzz";
    const redacted = redactTokens(text);
    expect(redacted).not.toContain("abcDEF123");
    expect(redacted).not.toContain("zzz");
    expect(redacted).toContain("failed");
  });

  it("leaves text without a token alone", () => {
    expect(redactTokens("all tests passed")).toBe("all tests passed");
  });
});
