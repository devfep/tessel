import { describe, expect, it, vi } from "vitest";

import { revokeOnce } from "./revoke-once";

describe("revokeOnce", () => {
  it("revokes on the first call, answers true, and ignores later calls", async () => {
    const revoke = vi.fn().mockResolvedValue(true);
    const report = vi.fn();
    const run = revokeOnce(revoke, report);
    expect(await run()).toBe(true);
    expect(await run()).toBe(true);
    expect(revoke).toHaveBeenCalledTimes(1);
    expect(report).not.toHaveBeenCalled();
  });

  it("reports a refused revocation and answers false", async () => {
    const report = vi.fn();
    expect(await revokeOnce(() => Promise.resolve(false), report)()).toBe(false);
    expect(report).toHaveBeenCalledExactlyOnceWith("revokeToken returned false");
  });

  it("reports a thrown error instead of throwing, answers false, and does not retry", async () => {
    const revoke = vi.fn().mockRejectedValue(new Error("service down"));
    const report = vi.fn();
    const run = revokeOnce(revoke, report);
    expect(await run()).toBe(false);
    expect(await run()).toBe(false);
    expect(report).toHaveBeenCalledExactlyOnceWith("service down");
    expect(revoke).toHaveBeenCalledTimes(1);
  });

  it("reports a non-Error rejection as text", async () => {
    const report = vi.fn();
    await revokeOnce(() => Promise.reject("boom"), report)();
    expect(report).toHaveBeenCalledExactlyOnceWith("boom");
  });

  it("does not revoke twice when two calls overlap", async () => {
    const revoke = vi.fn().mockResolvedValue(true);
    const run = revokeOnce(revoke, vi.fn());
    await Promise.all([run(), run()]);
    expect(revoke).toHaveBeenCalledTimes(1);
  });
});
