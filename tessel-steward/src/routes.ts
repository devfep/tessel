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
