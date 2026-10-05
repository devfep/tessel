import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import {
  LOCAL_GIT_COMMANDS_MAX,
  MARGIN_SECONDS,
  STEP_SECONDS,
  STEWARD_CALL_TIMEOUT_SECONDS,
  isTimedOut,
  worstCaseSeconds,
} from "./step-budget";

describe("the step budget", () => {
  it("fits one merge's worst case, plus the margin, inside the coordinator's call timeout", () => {
    expect(worstCaseSeconds() + MARGIN_SECONDS).toBeLessThanOrEqual(STEWARD_CALL_TIMEOUT_SECONDS);
  });

  it("counts every step once, and the local commands at their maximum", () => {
    const { clone, fetch, local, rebase, install, test, push } = STEP_SECONDS;
    expect(worstCaseSeconds()).toBe(
      clone + fetch + LOCAL_GIT_COMMANDS_MAX * local + rebase + install + test + push,
    );
  });

  it("keeps the dependency check inside the install share", () => {
    expect(STEP_SECONDS.dependencyCheck).toBeLessThanOrEqual(STEP_SECONDS.install);
  });

  it("equals the coordinator's STEWARD_CALL_TIMEOUT_MS in src/merge.rs", () => {
    const source = readFileSync(join(import.meta.dirname, "..", "..", "src", "merge.rs"), "utf8");
    const match = /pub const STEWARD_CALL_TIMEOUT_MS: u64 = (\d+) \* 60 \* 1000;/.exec(source);
    expect(match?.[1]).toBeDefined();
    expect(Number(match?.[1]) * 60).toBe(STEWARD_CALL_TIMEOUT_SECONDS);
  });

  it("stays under the Durable Object alarm's 15-minute wall limit", () => {
    expect(STEWARD_CALL_TIMEOUT_SECONDS).toBeLessThan(15 * 60);
  });

  it("treats exit 124 and 137 as a timeout and nothing else", () => {
    expect(isTimedOut(124)).toBe(true);
    expect(isTimedOut(137)).toBe(true);
    for (const code of [0, 1, 2, 101, 125, 126, 127, 128, 130, 139]) {
      expect(isTimedOut(code)).toBe(false);
    }
  });
});
