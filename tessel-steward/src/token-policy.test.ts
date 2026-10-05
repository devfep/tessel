import { describe, expect, it } from "vitest";

import { isForkRepo } from "./token-policy";

describe("isForkRepo", () => {
  it("accepts a repo with a fork source", () => {
    expect(isForkRepo({ source: "artifacts:ns/demo" })).toBe(true);
  });

  it("rejects a repo that is not a fork", () => {
    expect(isForkRepo({ source: null })).toBe(false);
  });
});
