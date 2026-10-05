import { WorkerEntrypoint } from "cloudflare:workers";

import { handleMergeRequest } from "./merge-request";
import { redactTokens } from "./redact";

/**
 * The entrypoint the coordinator reaches through its `STEWARD` service binding. It has no public
 * URL, so it needs no admin token: only a Worker that binds to it can call it. It accepts one
 * request, `POST` with `{ "repo", "fork", "commit" }`, and answers with the `MergeOutcome`.
 */
export class MergeService extends WorkerEntrypoint<Env> {
  override async fetch(request: Request): Promise<Response> {
    if (request.method !== "POST") {
      return Response.json({ error: "expected POST {repo, fork, commit}" }, { status: 405 });
    }
    const body = await request.json().catch(() => null);
    const repo =
      typeof body === "object" && body !== null ? (body as { repo?: unknown }).repo : undefined;
    if (typeof repo !== "string") {
      return Response.json({ error: "repo must be a repo name" }, { status: 400 });
    }
    try {
      return await handleMergeRequest(this.env, repo, body);
    } catch (error) {
      const reason = redactTokens(error instanceof Error ? error.message : String(error));
      console.error(JSON.stringify({ event: "merge_service_failed", repo, reason }));
      return Response.json({ error: "merge could not be run" }, { status: 502 });
    }
  }
}
