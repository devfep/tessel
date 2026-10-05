import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import {
  KILL_AFTER_SECONDS,
  LOCAL_GIT_COMMANDS_MAX,
  MARGIN_SECONDS,
  RESERVED_SECONDS,
  STEP_SECONDS,
  STEWARD_CALL_TIMEOUT_SECONDS,
  isTimedOut,
  totalReservedSeconds,
  worstCaseSeconds,
} from "./step-budget";

describe("the step budget", () => {
  it("fits one merge's worst case, plus the margin, inside the coordinator's call timeout", () => {
    expect(worstCaseSeconds() + MARGIN_SECONDS).toBeLessThanOrEqual(STEWARD_CALL_TIMEOUT_SECONDS);
  });

  it("counts every step once, each single command with its kill grace, the local ones at their maximum", () => {
    const { clone, fetch, local, rebase, install, test, push } = STEP_SECONDS;
    const singles = clone + fetch + rebase + push + 4 * KILL_AFTER_SECONDS;
    expect(worstCaseSeconds()).toBe(
      singles + LOCAL_GIT_COMMANDS_MAX * (local + KILL_AFTER_SECONDS) + install + test,
    );
  });

  it("spends the whole call timeout: steps, reserved time and margin add up exactly", () => {
    expect(worstCaseSeconds() + totalReservedSeconds() + MARGIN_SECONDS).toBe(
      STEWARD_CALL_TIMEOUT_SECONDS,
    );
  });

  it("leaves the test step 327 seconds, the remainder after every other share", () => {
    expect(STEP_SECONDS.test).toBe(327);
  });

  it("reserves time for the container start, tokens, reads, teardown and measurement", () => {
    expect(Object.keys(RESERVED_SECONDS).toSorted()).toEqual([
      "containerStart",
      "measurement",
      "reads",
      "teardown",
      "tokens",
    ]);
    expect(RESERVED_SECONDS.containerStart).toBeGreaterThanOrEqual(60);
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
