import { describe, expect, it } from "vitest";

import { parseRecordFields } from "./evidence-fields";
import { parseMergeRequest } from "./merge-types";

/**
 * The body `request_body` in `src/merge.rs` builds for this dispatch. The Rust test
 * `request_body_carries_the_record_fields_the_steward_reads` pins the same JSON: change both.
 */
const PINNED_BODY = {
  repo: "demo",
  fork: "demo--a1",
  commit: "3333333333333333333333333333333333333333",
  scopes: [{ scope: { kind: "file", path: "src/a.rs" }, mode: "edit_body" }],
  claim: 7,
  intent: {
    summary: "Fix the parser",
    task_ref: "T-1",
    assumptions: [
      {
        scope: { kind: "symbol", path: "src/b.rs", qualified_name: "b::parse" },
        statement: "returns Ok on empty input",
      },
    ],
  },
  decisions: {
    rejected: [{ approach: "regex", reason: "too slow" }],
    evidence: ["cargo test passed (12 tests)"],
    transcript: { repo: "demo--a1", path: "transcripts/7.md", sha256: "ab".repeat(32) },
  },
  review: {
    reasons: [{ reason: "no_test_evidence" }],
    decision: { reviewer: "felix", approve: true, note: "ok" },
  },
};

describe("parseRecordFields", () => {
  it("reads every field of the body the coordinator sends", () => {
    const parsed = parseMergeRequest(PINNED_BODY);
    expect(parsed).toEqual({
      ok: true,
      request: {
        fork: "demo--a1",
        commit: PINNED_BODY.commit,
        scopes: PINNED_BODY.scopes,
        record: {
          claim: 7,
          intent: {
            summary: "Fix the parser",
            taskRef: "T-1",
            assumptions: [
              {
                scope: { kind: "symbol", path: "src/b.rs", qualified_name: "b::parse" },
                statement: "returns Ok on empty input",
              },
            ],
          },
          decisions: PINNED_BODY.decisions,
          review: PINNED_BODY.review,
          malformed: [],
        },
      },
    });
  });

  it("leaves out what an older coordinator does not send", () => {
    expect(parseRecordFields({ fork: "f" })).toEqual({ malformed: [] });
  });

  it("reads review null as a submission that was not held", () => {
    expect(parseRecordFields({ review: null })).toEqual({ review: null, malformed: [] });
  });

  it("fills the defaults the coordinator may omit", () => {
    expect(
      parseRecordFields({ intent: { summary: "s" }, decisions: {}, review: { reasons: [] } }),
    ).toEqual({
      intent: { summary: "s", taskRef: null, assumptions: [] },
      decisions: { rejected: [], evidence: [], transcript: null },
      review: { reasons: [], decision: null },
      malformed: [],
    });
  });

  it.each([
    ["a negative claim", { claim: -1 }, "claim"],
    ["a fractional claim", { claim: 1.5 }, "claim"],
    ["a claim as text", { claim: "7" }, "claim"],
    ["an intent without a summary", { intent: { task_ref: "x" } }, "intent"],
    [
      "an assumption with no scope",
      { intent: { summary: "s", assumptions: [{ statement: "x" }] } },
      "intent",
    ],
    ["evidence that is not text", { decisions: { evidence: [1] } }, "decisions"],
    [
      "a transcript without a hash",
      { decisions: { transcript: { repo: "r", path: "p" } } },
      "decisions",
    ],
    [
      "a rejected approach without a reason",
      { decisions: { rejected: [{ approach: "a" }] } },
      "decisions",
    ],
    [
      "a review decision without approve",
      { review: { reasons: [], decision: { reviewer: "r" } } },
      "review",
    ],
    ["review reasons that are not objects", { review: { reasons: ["x"] } }, "review"],
  ])("names %s as malformed and drops it", (_label, body, field) => {
    const parsed = parseRecordFields(body);
    expect(parsed.malformed).toEqual([field]);
    expect(parsed).not.toHaveProperty(field);
  });
});
