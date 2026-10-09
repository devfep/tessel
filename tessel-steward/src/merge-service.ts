import { WorkerEntrypoint } from "cloudflare:workers";

import { handleHeadRequest, handleMergeRequest, handleTrialRequest } from "./merge-request";
import { redactTokens } from "./redact";

const TRIAL_PATH = "/trial";
const HEAD_PATH = "/head";

type Kind = "merge" | "trial" | "head";

function kindOf(pathname: string): Kind {
  if (pathname === TRIAL_PATH) {
    return "trial";
  }
  return pathname === HEAD_PATH ? "head" : "merge";
}

function run(env: Env, kind: Kind, repo: string, body: unknown): Promise<Response> {
  switch (kind) {
    case "trial":
      return handleTrialRequest(env, repo, body);
    case "head":
      return handleHeadRequest(env, repo);
    case "merge":
      return handleMergeRequest(env, repo, body, false);
  }
}

/**
 * The entrypoint the coordinator reaches through its `STEWARD` service binding. It has no public
 * URL, so it needs no admin token: only a Worker that binds to it can call it. It accepts three
 * requests, all `POST`:
 * - any path but `/trial` and `/head`: `{ "repo", "fork", "commit", "scopes" }`, answered with the
 *   `MergeOutcome`.
 * - `/trial`: `{ "repo", "fork", "before", "main", "commit"? }`, answered with the `TrialReport`. A trial
 *   tests a fork's commit on main and never pushes.
 * - `/head`: `{ "repo" }`, answered with `{ "head" }`, the head of the repo's main.
 */
export class MergeService extends WorkerEntrypoint<Env> {
  override async fetch(request: Request): Promise<Response> {
    if (request.method !== "POST") {
      return Response.json(
        {
          error:
            "expected POST {repo, fork, commit, scopes}, POST /trial {repo, fork, before, main} " +
            "or POST /head {repo}",
        },
        { status: 405 },
      );
    }
    const kind = kindOf(new URL(request.url).pathname);
    const body = await request.json().catch(() => null);
    const repo =
      typeof body === "object" && body !== null ? (body as { repo?: unknown }).repo : undefined;
    if (typeof repo !== "string") {
      return Response.json({ error: "repo must be a repo name" }, { status: 400 });
    }
    try {
      return await run(this.env, kind, repo, body);
    } catch (error) {
      if ((error as Partial<ArtifactsError>).code === "NOT_FOUND") {
        return Response.json({ error: "fork or repo not found" }, { status: 404 });
      }
      const reason = redactTokens(error instanceof Error ? error.message : String(error));
      console.error(JSON.stringify({ event: "merge_service_failed", repo, kind, reason }));
      const failed = kind === "head" ? "head could not be read" : `${kind} could not be run`;
      return Response.json({ error: failed }, { status: 502 });
    }
  }
}
