import { describe, expect, it } from "vitest";

import { foldReceipt } from "./review-receipt";

const FILE = { kind: "file", path: "src/a.rs" };
const TOUCHED = [{ scope: FILE, mode: "edit_body" }];
const COMMIT = "a".repeat(40);

let seq = 0;
function at<T extends Record<string, unknown>>(event: T) {
  seq += 1;
  return { seq, ...event };
}

const submitted = (claim: number) =>
  at({ event: "submitted", claim, fork_commit: COMMIT, touched: TOUCHED, decisions: {} });
const requested = (claim: number) => at({ event: "review_requested", claim, reasons: [] });
const decided = (claim: number, approve = true) =>
  at({ event: "review_decided", claim, approve, note: null });
const merged = (claim: number) => at({ event: "merged", claim, head: COMMIT });
const rejected = (claim: number) => at({ event: "submit_rejected", claim, reason: "no" });
const waits = (agent: string, req: number) =>
  at({
    event: "wait_queued",
    agent,
    req,
    scopes: TOUCHED,
    intent: { summary: "w", task_ref: null, assumptions: [] },
    position: req,
  });
const grants = (agent: string, claim: number) =>
  at({
    event: "claim_granted",
    agent,
    claim,
    fence: 1,
    scopes: TOUCHED,
    intent: { summary: "w", task_ref: null, assumptions: [] },
  });

const EMPTY = { decided: null, closed: null, waiting: [], granted: [], complete: false };

describe("foldReceipt", () => {
  it("shows nothing before the decision is logged", () => {
    seq = 0;
    expect(foldReceipt([submitted(1), requested(1), waits("w1", 1)], 1)).toEqual(EMPTY);
  });

  it("shows nothing for a claim the log does not know", () => {
    seq = 0;
    expect(foldReceipt([submitted(1), requested(1), decided(1)], 2)).toEqual(EMPTY);
  });

  it("fills the decision alone, with its seq, before the steward acts", () => {
    seq = 0;
    const receipt = foldReceipt([submitted(1), requested(1), waits("w1", 1), decided(1)], 1);
    expect(receipt).toEqual({
      decided: { seq: 4, event: "review_decided", approve: true },
      closed: null,
      waiting: ["w1"],
      granted: [],
      complete: false,
    });
  });

  it("fills the merge, then each waiting agent's grant after it, and completes", () => {
    seq = 0;
    const events = [
      submitted(1),
      requested(1),
      waits("w1", 1),
      waits("w2", 2),
      decided(1),
      merged(1),
      grants("w2", 9),
    ];
    const partial = foldReceipt(events, 1);
    expect(partial.closed).toEqual({ seq: 6, event: "merged" });
    expect(partial.granted).toEqual([{ agent: "w2", seq: 7 }]);
    expect(partial.complete).toBe(false);
    const full = foldReceipt([...events, grants("w1", 10)], 1);
    expect(full.granted).toEqual([
      { agent: "w2", seq: 7 },
      { agent: "w1", seq: 8 },
    ]);
    expect(full.complete).toBe(true);
  });

  it("is complete at the merge when nobody was waiting", () => {
    seq = 0;
    const receipt = foldReceipt([submitted(1), requested(1), decided(1), merged(1)], 1);
    expect(receipt.complete).toBe(true);
    expect(receipt.waiting).toEqual([]);
  });

  it("does not count a grant that came before the merge", () => {
    seq = 0;
    const events = [
      submitted(1),
      requested(1),
      waits("w1", 1),
      decided(1),
      grants("w1", 9),
      merged(1),
    ];
    expect(foldReceipt(events, 1).granted).toEqual([]);
  });

  it("does not count a grant to an agent that was not waiting at the decision", () => {
    seq = 0;
    const events = [submitted(1), requested(1), decided(1), merged(1), grants("late", 9)];
    expect(foldReceipt(events, 1)).toMatchObject({ waiting: [], granted: [], complete: true });
  });

  it("completes at a rejection and expects no grant: the claim is active again", () => {
    seq = 0;
    const events = [submitted(1), requested(1), waits("w1", 1), decided(1, false), rejected(1)];
    const receipt = foldReceipt(events, 1);
    expect(receipt.decided).toMatchObject({ approve: false });
    expect(receipt.closed).toEqual({ seq: 5, event: "submit_rejected" });
    expect(receipt.waiting).toEqual(["w1"]);
    expect(receipt.granted).toEqual([]);
    expect(receipt.complete).toBe(true);
  });

  it("ignores a merge that no decision preceded", () => {
    seq = 0;
    expect(foldReceipt([submitted(1), requested(1), merged(1)], 1).closed).toBeNull();
  });

  it("starts over from the newest hold after a rejection and resubmission", () => {
    seq = 0;
    const events = [
      submitted(1),
      requested(1),
      decided(1, false),
      rejected(1),
      submitted(1),
      requested(1),
    ];
    expect(foldReceipt(events, 1)).toEqual(EMPTY);
  });

  it("ignores frames that are not events", () => {
    expect(foldReceipt([null, "x", [], { event: "merged" }], 1)).toEqual(EMPTY);
  });
});
