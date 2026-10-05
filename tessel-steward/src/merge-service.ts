import { WorkerEntrypoint } from "cloudflare:workers";

import { handleMergeRequest, handleTrialRequest } from "./merge-request";
import { redactTokens } from "./redact";

const TRIAL_PATH = "/trial";

/**
 * The entrypoint the coordinator reaches through its `STEWARD` service binding. It has no public
 * URL, so it needs no admin token: only a Worker that binds to it can call it. It accepts two
 * requests, both `POST`:
 * - any path but `/trial`: `{ "repo", "fork", "commit", "scopes" }`, answered with the `MergeOutcome`.
 * - `/trial`: `{ "repo", "fork", "before", "main", "commit"? }`, answered with the `TrialReport`. A trial
 *   tests a fork's commit on main and never pushes.
 */
export class MergeService extends WorkerEntrypoint<Env> {
  override async fetch(request: Request): Promise<Response> {
    if (request.method !== "POST") {
      return Response.json(
        {
          error:
            "expected POST {repo, fork, commit, scopes} or POST /trial {repo, fork, before, main}",
        },
        { status: 405 },
      );
    }
    const trial = new URL(request.url).pathname === TRIAL_PATH;
    const body = await request.json().catch(() => null);
    const repo =
      typeof body === "object" && body !== null ? (body as { repo?: unknown }).repo : undefined;
    if (typeof repo !== "string") {
      return Response.json({ error: "repo must be a repo name" }, { status: 400 });
    }
    try {
      return await (trial
        ? handleTrialRequest(this.env, repo, body)
        : handleMergeRequest(this.env, repo, body));
    } catch (error) {
      if ((error as Partial<ArtifactsError>).code === "NOT_FOUND") {
        return Response.json({ error: "fork or repo not found" }, { status: 404 });
      }
      const reason = redactTokens(error instanceof Error ? error.message : String(error));
      console.error(JSON.stringify({ event: "merge_service_failed", repo, trial, reason }));
      return Response.json(
        { error: trial ? "trial could not be run" : "merge could not be run" },
        { status: 502 },
      );
    }
  }
}
