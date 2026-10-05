import { describe, expect, it } from "vitest";

import {
  MODES,
  modePermits,
  parseNameStatus,
  parseScopes,
  uncoveredPaths,
  type ClaimedScope,
  type Mode,
} from "./merge-coverage";

/** A fixed copy of `Mode::permits` in src/protocol.rs: rows are held, columns are needed. */
const RUST_PERMITS_TABLE: Record<Mode, Record<Mode, boolean>> = {
  depend: { depend: true, edit_body: false, edit_signature: false, create: false },
  edit_body: { depend: true, edit_body: true, edit_signature: false, create: false },
  edit_signature: { depend: true, edit_body: true, edit_signature: true, create: false },
  create: { depend: true, edit_body: false, edit_signature: false, create: true },
};

const dir = (path: string, mode: Mode): ClaimedScope => ({ scope: { kind: "dir", path }, mode });
const file = (path: string, mode: Mode): ClaimedScope => ({ scope: { kind: "file", path }, mode });
const symbol = (path: string, mode: Mode): ClaimedScope => ({
  scope: { kind: "symbol", path, qualified_name: "m::f" },
  mode,
});

function uncovered(status: string, claim: ClaimedScope[]): string[] {
  const required = parseNameStatus(status);
  if (required === undefined) {
    throw new Error("unparsed");
  }
  return uncoveredPaths(required, claim);
}

describe("modePermits", () => {
  it("matches Mode::permits in protocol.rs for all 16 pairs", () => {
    for (const held of MODES) {
      for (const needed of MODES) {
        expect(modePermits(held, needed), `${held} permits ${needed}`).toBe(
          RUST_PERMITS_TABLE[held][needed],
        );
      }
    }
  });
});

describe("parseNameStatus", () => {
  it.each([
    ["added", "A\0a.ts\0", [{ path: "a.ts", mode: "create" }]],
    ["modified", "M\0a.ts\0", [{ path: "a.ts", mode: "edit_body" }]],
    ["deleted", "D\0a.ts\0", [{ path: "a.ts", mode: "edit_signature" }]],
    ["type changed", "T\0a.ts\0", [{ path: "a.ts", mode: "edit_signature" }]],
    [
      "renamed",
      "R087\0old.ts\0new.ts\0",
      [
        { path: "old.ts", mode: "edit_signature" },
        { path: "new.ts", mode: "create" },
      ],
    ],
    ["copied", "C075\0src.ts\0copy.ts\0", [{ path: "copy.ts", mode: "create" }]],
    [
      "several records and a name with a space and a newline",
      "A\0a b.ts\0M\0c\nd.ts\0",
      [
        { path: "a b.ts", mode: "create" },
        { path: "c\nd.ts", mode: "edit_body" },
      ],
    ],
    ["nothing", "", []],
  ])("maps %s", (_label, stdout, expected) => {
    expect(parseNameStatus(stdout)).toEqual(expected);
  });

  it.each([
    ["an unmerged record", "U\0a.ts\0"],
    ["an unknown status", "Z\0a.ts\0"],
    ["a status without a path", "M\0"],
    ["a rename with one path", "R100\0old.ts\0"],
    ["an empty path", "M\0\0"],
  ])("returns undefined for %s", (_label, stdout) => {
    expect(parseNameStatus(stdout)).toBeUndefined();
  });
});

describe("uncoveredPaths", () => {
  it("covers a file by an exact file scope in a permitting mode", () => {
    expect(uncovered("M\0src/a.ts\0", [file("src/a.ts", "edit_body")])).toEqual([]);
    expect(uncovered("M\0src/b.ts\0", [file("src/a.ts", "edit_body")])).toEqual(["src/b.ts"]);
  });

  it("covers everything beneath an ancestor dir, but not a sibling or a longer name", () => {
    const claim = [dir("src/auth", "edit_signature")];
    expect(uncovered("M\0src/auth/a.ts\0M\0src/auth/deep/b.ts\0", claim)).toEqual([]);
    expect(uncovered("M\0src/authz/a.ts\0M\0src/other/b.ts\0M\0src\0", claim)).toEqual([
      "src/authz/a.ts",
      "src/other/b.ts",
      "src",
    ]);
  });

  it("covers every path with the root dir", () => {
    expect(uncovered("M\0a.ts\0M\0x/y/z.ts\0", [dir("", "edit_body")])).toEqual([]);
  });

  it("covers a file by a symbol scope in it, and no other file", () => {
    const claim = [symbol("src/a.ts", "edit_body")];
    expect(uncovered("M\0src/a.ts\0", claim)).toEqual([]);
    expect(uncovered("M\0src/b.ts\0", claim)).toEqual(["src/b.ts"]);
  });

  it("rejects a change whose mode the claim does not permit", () => {
    expect(uncovered("D\0a.ts\0", [file("a.ts", "edit_body")])).toEqual(["a.ts"]);
    expect(uncovered("A\0a.ts\0", [file("a.ts", "edit_body")])).toEqual(["a.ts"]);
    expect(uncovered("M\0a.ts\0", [file("a.ts", "depend")])).toEqual(["a.ts"]);
  });

  it("covers a modified file by edit_body or by create, but not by depend", () => {
    expect(uncovered("M\0a.ts\0", [file("a.ts", "edit_body")])).toEqual([]);
    expect(uncovered("M\0a.ts\0", [file("a.ts", "create")])).toEqual([]);
    expect(uncovered("M\0a.ts\0", [file("a.ts", "depend")])).toEqual(["a.ts"]);
  });

  it("does not let create cover a delete, a type change or the old path of a rename", () => {
    const claim = [file("x.ts", "create")];
    expect(uncovered("D\0x.ts\0", claim)).toEqual(["x.ts"]);
    expect(uncovered("T\0x.ts\0", claim)).toEqual(["x.ts"]);
    expect(uncovered("R100\0x.ts\0y.ts\0", [...claim, file("y.ts", "create")])).toEqual(["x.ts"]);
  });

  it("lets one scope in a permitting mode cover what another scope in a weak mode does not", () => {
    const claim = [dir("src", "depend"), file("src/a.ts", "edit_body")];
    expect(uncovered("M\0src/a.ts\0M\0src/b.ts\0", claim)).toEqual(["src/b.ts"]);
  });

  it("needs edit_signature on the old path and create on the new path for a rename", () => {
    const rename = "R090\0old.ts\0new.ts\0";
    expect(uncovered(rename, [file("old.ts", "edit_signature")])).toEqual(["new.ts"]);
    expect(uncovered(rename, [file("new.ts", "create")])).toEqual(["old.ts"]);
    expect(uncovered(rename, [file("old.ts", "edit_signature"), file("new.ts", "create")])).toEqual(
      [],
    );
  });

  it("needs create on the new path only for a copy", () => {
    expect(uncovered("C100\0src.ts\0copy.ts\0", [file("copy.ts", "create")])).toEqual([]);
  });

  it("lists a path once and covers nothing with an empty claim", () => {
    expect(uncovered("M\0a.ts\0D\0a.ts\0", [])).toEqual(["a.ts"]);
  });
});

describe("parseScopes", () => {
  it("reads the scopes the coordinator sends, symbol scopes flattened by serde", () => {
    const sent = [
      { scope: { kind: "dir", path: "" }, mode: "depend" },
      { scope: { kind: "file", path: "a.ts" }, mode: "edit_body" },
      { scope: { kind: "symbol", path: "a.ts", qualified_name: "m::f" }, mode: "create" },
    ];
    expect(parseScopes(sent)).toEqual({ ok: true, scopes: sent });
  });

  it("accepts an empty list, which covers nothing", () => {
    expect(parseScopes([])).toEqual({ ok: true, scopes: [] });
  });

  it.each([
    ["undefined", undefined],
    ["an object", {}],
    ["a string", "dir"],
    ["a non-object item", ["dir"]],
    ["an unknown kind", [{ scope: { kind: "glob", path: "*" }, mode: "edit_body" }]],
    ["an unknown mode", [{ scope: { kind: "dir", path: "" }, mode: "write" }]],
    ["a missing mode", [{ scope: { kind: "dir", path: "" } }]],
    ["a file scope with an empty path", [{ scope: { kind: "file", path: "" }, mode: "create" }]],
    ["a symbol scope without a name", [{ scope: { kind: "symbol", path: "a" }, mode: "create" }]],
    ["a non-string path", [{ scope: { kind: "dir", path: 1 }, mode: "create" }]],
  ])("rejects %s", (_label, value) => {
    expect(parseScopes(value).ok).toBe(false);
  });
});
