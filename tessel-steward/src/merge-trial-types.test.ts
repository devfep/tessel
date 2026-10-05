import { describe, expect, it } from "vitest";

import type { TrialSandbox } from "./merge-executor";
import type { TrialDeps } from "./merge-steps";

/** Compile-time proofs: `pnpm typecheck` fails if a `@ts-expect-error` below stops being an error. */
async function tokensFromASandbox(sandbox: TrialSandbox): Promise<void> {
  // @ts-expect-error a trial has no handle on main
  await sandbox.main.createToken("write", 60);
  // @ts-expect-error nor on the fork
  await sandbox.fork.createToken("read", 60);
}

function pushFromDeps(deps: TrialDeps): void {
  // @ts-expect-error pushing is a merge's dependency
  deps.withPushAccess({ base: "", head: "" }, async () => undefined);
  // @ts-expect-error and so is reading main back
  deps.currentMain();
}

describe("what a trial is given", () => {
  it("offers no repo handle and no way to push, so no write token can be minted", () => {
    expect([tokensFromASandbox, pushFromDeps]).toHaveLength(2);
  });
});
