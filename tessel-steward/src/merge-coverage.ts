/**
 * Invariant 11 (src/protocol.rs): every scope a submission touched must be covered by the claim.
 * The coordinator checks the `touched` list the agent sends; this module checks the change the
 * steward actually sees, the submitted commit rebased onto main.
 *
 * The check is file level. A `symbol` scope covers only its own file, so which symbol inside the
 * file was edited still rests on the agent's `touched`. File names come from the repo and are
 * untrusted data: they are compared as strings and never put into a command.
 */

export const MODES = ["depend", "edit_body", "edit_signature", "create"] as const;
export type Mode = (typeof MODES)[number];

/** A claimed node of the scope tree. Paths are repo-relative; the repo root is `dir ""`. */
export type ClaimedNode =
  | { kind: "dir"; path: string }
  | { kind: "file"; path: string }
  | { kind: "symbol"; path: string; qualified_name: string };

/** One granted scope of the claim, as the coordinator sends it (`ScopeClaim` in protocol.rs). */
export interface ClaimedScope {
  scope: ClaimedNode;
  mode: Mode;
}

/** Most uncovered files an outcome names; `total` still counts all of them. */
export const MAX_REPORTED_FILES = 50;

/**
 * For each held mode, the modes it permits. Same table as `Mode::permits` in src/protocol.rs,
 * which is the source of truth: change both together (merge-coverage.test.ts pins this one).
 */
const PERMITS: Record<Mode, readonly Mode[]> = {
  depend: ["depend"],
  edit_body: ["depend", "edit_body"],
  edit_signature: ["depend", "edit_body", "edit_signature"],
  create: ["depend", "create"],
};

/** Whether holding a claim in `held` authorises work done in `needed`. */
export function modePermits(held: Mode, needed: Mode): boolean {
  return PERMITS[held].includes(needed);
}

function isMode(value: unknown): value is Mode {
  return MODES.some((mode) => mode === value);
}

function parseNode(value: unknown): ClaimedNode | undefined {
  if (typeof value !== "object" || value === null) {
    return undefined;
  }
  const { kind, path, qualified_name } = value as Record<string, unknown>;
  if (typeof path !== "string") {
    return undefined;
  }
  if (kind === "dir") {
    return { kind, path };
  }
  if (kind === "file" && path !== "") {
    return { kind, path };
  }
  if (kind === "symbol" && path !== "" && typeof qualified_name === "string") {
    return { kind, path, qualified_name };
  }
  return undefined;
}

export type ParsedScopes = { ok: true; scopes: ClaimedScope[] } | { ok: false; error: string };

/** Validates the `scopes` of a merge request: an array of `{ scope, mode }`. */
export function parseScopes(value: unknown): ParsedScopes {
  if (!Array.isArray(value)) {
    return { ok: false, error: "scopes must be an array of the claim's {scope, mode}" };
  }
  const scopes: ClaimedScope[] = [];
  for (const item of value as unknown[]) {
    const { scope, mode } =
      typeof item === "object" && item !== null ? (item as Record<string, unknown>) : {};
    const node = parseNode(scope);
    if (node === undefined || !isMode(mode)) {
      return { ok: false, error: "each scope must be {scope: dir|file|symbol, mode}" };
    }
    scopes.push({ scope: node, mode });
  }
  return { ok: true, scopes };
}

/** A change to one path that needs a claim in a mode that permits `mode`. */
export interface Requirement {
  path: string;
  mode: Mode;
}

/**
 * Reads `git diff --name-status -z -M` output into what the claim must cover: added is `create`,
 * modified is `edit_body`, deleted and type-changed are `edit_signature`, a rename is
 * `edit_signature` on the old path plus `create` on the new one, a copy is `create` on the new
 * path. Returns undefined for any status or shape it does not know, so a surprise is never
 * treated as covered.
 */
export function parseNameStatus(stdout: string): Requirement[] | undefined {
  const tokens = stdout.split("\0");
  if (tokens.at(-1) === "") {
    tokens.pop();
  }
  const out: Requirement[] = [];
  let at = 0;
  while (at < tokens.length) {
    const status = tokens[at++]?.[0];
    const paths: string[] = [];
    const arity = status === "R" || status === "C" ? 2 : 1;
    for (let i = 0; i < arity; i++) {
      const path = tokens[at++];
      if (path === undefined || path === "") {
        return undefined;
      }
      paths.push(path);
    }
    const [first, second] = paths;
    if (first === undefined) {
      return undefined;
    }
    switch (status) {
      case "A":
        out.push({ path: first, mode: "create" });
        break;
      case "M":
        out.push({ path: first, mode: "edit_body" });
        break;
      case "D":
      case "T":
        out.push({ path: first, mode: "edit_signature" });
        break;
      case "R":
        out.push({ path: first, mode: "edit_signature" });
        out.push({ path: second ?? "", mode: "create" });
        break;
      case "C":
        out.push({ path: second ?? "", mode: "create" });
        break;
      default:
        return undefined;
    }
  }
  return out;
}

function covers(node: ClaimedNode, path: string): boolean {
  switch (node.kind) {
    case "dir":
      return node.path === "" || path.startsWith(`${node.path}/`);
    case "file":
    case "symbol":
      return node.path === path;
  }
}

/** The paths, once each and in order, that no claimed scope covers in a permitting mode. */
export function uncoveredPaths(required: readonly Requirement[], claim: readonly ClaimedScope[]) {
  const uncovered = new Set<string>();
  for (const { path, mode } of required) {
    const covered = claim.some((held) => covers(held.scope, path) && modePermits(held.mode, mode));
    if (!covered) {
      uncovered.add(path);
    }
  }
  return [...uncovered];
}
