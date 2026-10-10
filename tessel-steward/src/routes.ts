export type Route =
  | { kind: "create"; repo: string }
  | { kind: "fork"; repo: string; fork: string }
  | { kind: "token"; repo: string }
  | { kind: "read-token"; repo: string }
  | { kind: "test-run"; repo: string }
  | { kind: "merge"; repo: string }
  | { kind: "identity"; repo: string; agent: string };

export function matchRoute(pathname: string): Route | undefined {
  const [root, repo, action, fork, leaf, ...rest] = pathname.split("/").filter(Boolean);
  if (root !== "repos" || repo === undefined || rest.length > 0) {
    return undefined;
  }
  if (action === "agents" && fork !== undefined && leaf === "identity") {
    return { kind: "identity", repo, agent: fork };
  }
  if (leaf !== undefined) {
    return undefined;
  }
  if (action === undefined) {
    return { kind: "create", repo };
  }
  if (action === "tokens" && fork === undefined) {
    return { kind: "token", repo };
  }
  if (action === "read-tokens" && fork === undefined) {
    return { kind: "read-token", repo };
  }
  if (action === "test-runs" && fork === undefined) {
    return { kind: "test-run", repo };
  }
  if (action === "merges" && fork === undefined) {
    return { kind: "merge", repo };
  }
  if (action === "forks" && fork !== undefined) {
    return { kind: "fork", repo, fork };
  }
  return undefined;
}

export type DashboardRoute = { kind: "page" | "events" | "summary"; repo: string };

/** Matches `/dashboard/<repo>`, `/dashboard/<repo>/events` and `/dashboard/<repo>/summary`. */
export function matchDashboardRoute(pathname: string): DashboardRoute | undefined {
  const [root, repo, leaf, ...rest] = pathname.split("/").filter(Boolean);
  if (root !== "dashboard" || repo === undefined || rest.length > 0) {
    return undefined;
  }
  if (leaf === undefined) {
    return { kind: "page", repo };
  }
  if (leaf === "events" || leaf === "summary") {
    return { kind: leaf, repo };
  }
  return undefined;
}

export type ReviewRoute =
  | { kind: "page"; repo: string }
  | { kind: "diff" | "decision" | "receipt"; repo: string; claim: number };

/**
 * Matches `/review/<repo>`, and `/review/<repo>/<claim>/` followed by `diff`, `receipt` or
 * `decision`. A claim is a plain decimal id.
 */
export function matchReviewRoute(pathname: string): ReviewRoute | undefined {
  const [root, repo, claim, leaf, ...rest] = pathname.split("/").filter(Boolean);
  if (root !== "review" || repo === undefined || rest.length > 0) {
    return undefined;
  }
  if (claim === undefined) {
    return { kind: "page", repo };
  }
  const id = /^\d+$/.test(claim) ? Number(claim) : Number.NaN;
  if (!Number.isSafeInteger(id) || (leaf !== "diff" && leaf !== "decision" && leaf !== "receipt")) {
    return undefined;
  }
  return { kind: leaf, repo, claim: id };
}
