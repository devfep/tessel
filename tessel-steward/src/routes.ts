export type Route =
  | { kind: "create"; repo: string }
  | { kind: "fork"; repo: string; fork: string }
  | { kind: "token"; repo: string }
  | { kind: "test"; repo: string };

export function matchRoute(pathname: string): Route | undefined {
  const [root, repo, action, fork, ...rest] = pathname.split("/").filter(Boolean);
  if (root !== "repos" || repo === undefined || rest.length > 0) {
    return undefined;
  }
  if (action === undefined) {
    return { kind: "create", repo };
  }
  if (action === "tokens" && fork === undefined) {
    return { kind: "token", repo };
  }
  if (action === "test-runs" && fork === undefined) {
    return { kind: "test", repo };
  }
  if (action === "forks" && fork !== undefined) {
    return { kind: "fork", repo, fork };
  }
  return undefined;
}
