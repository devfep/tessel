import { WorkerEntrypoint } from "cloudflare:workers";

import { isAllowedGitRequest } from "./git-gateway-policy";
import {
  isExpectedPush,
  isReceivePackDiscovery,
  isReceivePackPost,
  parsePushCommands,
  readBodyCapped,
  type ExpectedPush,
} from "./receive-pack-policy";

const MAX_PUSH_BODY_BYTES = 16 * 1024 * 1024;

export interface ReadRoute {
  remote: string;
  token: string;
}

interface ReadGatewayProps {
  routes: ReadRoute[];
}

type PushGatewayProps = ExpectedPush & { token: string };

function withToken(headers: Headers, token: string): Headers {
  const authorized = new Headers(headers);
  authorized.set("Authorization", `Bearer ${token}`);
  return authorized;
}

function forbidden(): Response {
  return new Response("Forbidden", { status: 403 });
}

/**
 * Receives every HTTPS request the merge sandbox makes to the Artifacts git host while main and
 * the fork are being fetched.
 *
 * It forwards only the read requests of `git clone` / `git fetch` for the repos in `routes`, each
 * with its own read token, so no token enters the sandbox and nothing can be pushed.
 */
export class MergeReadGateway extends WorkerEntrypoint<Env, ReadGatewayProps> {
  override async fetch(request: Request): Promise<Response> {
    const route = this.ctx.props.routes.find((candidate) =>
      isAllowedGitRequest(request, candidate.remote),
    );
    if (route === undefined) {
      return forbidden();
    }
    const headers = withToken(request.headers, route.token);
    return fetch(new Request(request, { headers, redirect: "manual" }));
  }
}

/**
 * Receives every HTTPS request the merge sandbox makes to the Artifacts git host while the
 * tested commit is pushed. It is installed only after the tests passed.
 *
 * The sandbox ran the repo's code, so its git is not trusted. This gateway checks the push itself:
 * a single `git-receive-pack` whose one command is exactly `old` -> `new` on `ref` for `remote`,
 * and only then adds the write token. A push of any other commit, ref or old value is refused
 * here, and the server refuses it again if `old` is no longer what main points at.
 */
export class MergePushGateway extends WorkerEntrypoint<Env, PushGatewayProps> {
  override async fetch(request: Request): Promise<Response> {
    const { token, ...expected } = this.ctx.props;
    if (isReceivePackDiscovery(request, expected)) {
      const headers = withToken(request.headers, token);
      return fetch(new Request(request, { headers, redirect: "manual" }));
    }
    if (!isReceivePackPost(request, expected) || request.headers.has("Content-Encoding")) {
      return forbidden();
    }
    const body = await readBodyCapped(request.body, MAX_PUSH_BODY_BYTES);
    if (body === undefined) {
      return new Response("Payload Too Large", { status: 413 });
    }
    const commands = parsePushCommands(body);
    if (commands === undefined || !isExpectedPush(commands, expected)) {
      return forbidden();
    }
    const headers = withToken(request.headers, token);
    return fetch(new Request(request.url, { method: "POST", headers, body, redirect: "manual" }));
  }
}
