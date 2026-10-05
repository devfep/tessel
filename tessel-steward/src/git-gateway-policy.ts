const UPLOAD_PACK_SERVICE = "?service=git-upload-pack";

/**
 * Decides whether a request from the sandbox may reach the Artifacts git host.
 *
 * Only the two requests that `git clone` and `git fetch` make for the one allowed repo pass:
 * `GET <repo>/info/refs?service=git-upload-pack` and `POST <repo>/git-upload-pack`. Pushes
 * (`git-receive-pack`), other repos, other hosts and every other path or query are refused.
 *
 * @param request The method and URL of the outbound request.
 * @param allowedRemote The remote URL of the one repo this run may read.
 * @returns True if the request may be forwarded with the repo token attached.
 */
export function isAllowedGitRequest(
  request: { method: string; url: string },
  allowedRemote: string,
): boolean {
  const remote = new URL(allowedRemote);
  const target = new URL(request.url);
  if (target.protocol !== "https:" || target.host !== remote.host) {
    return false;
  }
  if (request.method === "GET") {
    return (
      target.pathname === `${remote.pathname}/info/refs` && target.search === UPLOAD_PACK_SERVICE
    );
  }
  if (request.method === "POST") {
    return target.pathname === `${remote.pathname}/git-upload-pack` && target.search === "";
  }
  return false;
}
