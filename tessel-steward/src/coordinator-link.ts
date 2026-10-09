import { signIdentityToken } from "./identity";
import type { UpstreamSocket } from "./sse-relay";

/** A link token opens one request or one socket, so it only needs to outlive that. */
const LINK_TOKEN_TTL_MS = 60 * 1000;
const COORDINATOR_ORIGIN = "https://coordinator.internal";

export type LinkEnv = Pick<Env, "IDENTITY_SIGNING_KEY" | "COORDINATOR">;

/** Authorization headers for `agent` on `repo`, signed here and never sent to a browser. */
export async function coordinatorHeaders(
  env: LinkEnv,
  repo: string,
  agent: string,
): Promise<Headers> {
  const token = await signIdentityToken(env.IDENTITY_SIGNING_KEY, {
    repo,
    agent,
    expMs: Date.now() + LINK_TOKEN_TTL_MS,
  });
  return new Headers({ Authorization: `Bearer ${token}` });
}

/** `GET /repo/<repo>/summary` as `agent`; the caller owns the response. */
export async function fetchSummary(env: LinkEnv, repo: string, agent: string): Promise<Response> {
  const headers = await coordinatorHeaders(env, repo, agent);
  return env.COORDINATOR.fetch(`${COORDINATOR_ORIGIN}/repo/${repo}/summary`, { headers });
}

/**
 * Opens and accepts the coordinator's WebSocket for `agent` on `repo`.
 *
 * @throws If the coordinator refuses the upgrade.
 */
export async function openCoordinatorSocket(
  env: LinkEnv,
  repo: string,
  agent: string,
): Promise<UpstreamSocket> {
  const headers = await coordinatorHeaders(env, repo, agent);
  headers.set("Upgrade", "websocket");
  const upstream = await env.COORDINATOR.fetch(`${COORDINATOR_ORIGIN}/repo/${repo}/ws`, {
    headers,
  });
  const { webSocket } = upstream;
  if (webSocket === null || webSocket === undefined) {
    throw new Error(`the coordinator refused the socket with ${upstream.status}`);
  }
  webSocket.accept();
  return webSocket;
}
