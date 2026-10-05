import { describe, expect, it, vi } from "vitest";

import { revokeOnce } from "./revoke-once";

describe("revokeOnce", () => {
  it("revokes on the first call and ignores later calls", async () => {
    const revoke = vi.fn().mockResolvedValue(true);
    const report = vi.fn();
    const run = revokeOnce(revoke, report);
    await run();
    await run();
    expect(revoke).toHaveBeenCalledTimes(1);
    expect(report).not.toHaveBeenCalled();
  });

  it("reports a refused revocation", async () => {
    const report = vi.fn();
    await revokeOnce(() => Promise.resolve(false), report)();
    expect(report).toHaveBeenCalledExactlyOnceWith("revokeToken returned false");
  });

  it("reports a thrown error instead of throwing, and does not retry", async () => {
    const revoke = vi.fn().mockRejectedValue(new Error("service down"));
    const report = vi.fn();
    const run = revokeOnce(revoke, report);
    await expect(run()).resolves.toBeUndefined();
    await run();
    expect(report).toHaveBeenCalledExactlyOnceWith("service down");
    expect(revoke).toHaveBeenCalledTimes(1);
  });

  it("reports a non-Error rejection as text", async () => {
    const report = vi.fn();
    await revokeOnce(() => Promise.reject("boom"), report)();
    expect(report).toHaveBeenCalledExactlyOnceWith("boom");
  });
});
