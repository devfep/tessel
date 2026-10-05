const PUSHED_EVENT_TYPE = "cf.artifacts.repo.pushed";

export interface Push {
  namespace: string;
  repo: string;
  ref: string;
  before: string;
  after: string;
  commitIds: string[];
  totalCommits: number;
  commitsTruncated: boolean;
}

export type ParsedPush = { ok: true; push: Push } | { ok: false; reason: string };

type Fields = Record<string, unknown>;

class MalformedEvent extends Error {}

function asFields(value: unknown, name: string): Fields {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new MalformedEvent(`${name} is not an object`);
  }
  return value as Fields;
}

function stringField(fields: Fields, key: string): string {
  const value = fields[key];
  if (typeof value !== "string") {
    throw new MalformedEvent(`${key} is missing or not a string`);
  }
  return value;
}

function commitIds(commits: unknown): string[] {
  if (!Array.isArray(commits)) {
    throw new MalformedEvent("commits is not a list");
  }
  return commits.map((commit) => stringField(asFields(commit, "commit"), "id"));
}

function readPush(body: unknown): Push {
  const event = asFields(body, "message body");
  if (event["type"] !== PUSHED_EVENT_TYPE) {
    throw new MalformedEvent(`event type is ${JSON.stringify(event["type"])}`);
  }
  const source = asFields(event["source"], "source");
  const payload = asFields(event["payload"], "payload");
  const ids = commitIds(payload["commits"]);
  const total = payload["totalCommitsCount"];
  return {
    namespace: stringField(source, "namespace"),
    repo: stringField(source, "repoName"),
    ref: stringField(payload, "ref"),
    before: stringField(payload, "before"),
    after: stringField(payload, "after"),
    commitIds: ids,
    totalCommits: typeof total === "number" ? total : ids.length,
    commitsTruncated: payload["commitsTruncated"] === true,
  };
}

/**
 * Validates a queue message body as an Artifacts `pushed` event.
 *
 * Commit messages and author fields are free text written by agents, so they are
 * dropped here; only identifiers and refs are carried forward.
 *
 * @param body The queue message body, as delivered by the event subscription.
 * @returns The push summary, or the reason the body is not a usable push event.
 */
export function parsePushEvent(body: unknown): ParsedPush {
  try {
    return { ok: true, push: readPush(body) };
  } catch (error) {
    if (error instanceof MalformedEvent) {
      return { ok: false, reason: error.message };
    }
    throw error;
  }
}
