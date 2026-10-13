import { CONTAINER_CA_CERTIFICATE, execCaptured } from "./container-step";
import type { RecordCommand } from "./evidence-commands";
import type { EvidenceDeps, RecordRepo, RecordUpdate } from "./evidence-steps";
import { MAIN_BRANCH } from "./merge-commands";
import { parseSha, type GitResult, type Sha } from "./merge-types";
import { redactTokens } from "./redact";
import { revokeOnce } from "./revoke-once";

/** A record push is two requests seconds apart, after one fetch; 60 s is the shortest lifetime. */
const RECORD_TOKEN_TTL_SECONDS = 60;
/** The old value git sends for a ref it creates. */
const NO_COMMIT = "0".repeat(40) as Sha;

function codeOf(error: unknown): string | undefined {
  return (error as Partial<ArtifactsError> | null)?.code;
}

function report(event: string, repo: string): (reason: string) => void {
  return (reason) => console.error(JSON.stringify({ event, repo, reason: redactTokens(reason) }));
}

/** Creates `name` with main as its default branch and revokes the token `create` returns. */
async function createRecordRepo(artifacts: Artifacts, name: string): Promise<void> {
  let created: ArtifactsCreateRepoResult;
  try {
    created = await artifacts.create(name, {
      setDefaultBranch: MAIN_BRANCH,
      description: "Written by the Tessel steward only",
    });
  } catch (error) {
    if (codeOf(error) === "ALREADY_EXISTS") {
      return;
    }
    throw error;
  }
  using handle = await artifacts.get(name);
  const revoke = revokeOnce(
    () => handle.revokeToken(created.token),
    report("record_repo_token_revoke_failed", name),
  );
  await revoke();
}

/**
 * Opens the record repo `name`, creating it the first time: a `get` that fails with `NOT_FOUND`,
 * then `create`, then `get` once more. Two merges that race both create it; the second `create`
 * fails with `ALREADY_EXISTS` and its `get` finds the first one's repo.
 *
 * @throws If the repo is a fork: an agent's fork can carry this name, and its owner holds write
 *   tokens for it, so it is never written as a record repo.
 */
export async function openRecordRepo(artifacts: Artifacts, name: string): Promise<ArtifactsRepo> {
  let handle: ArtifactsRepo;
  try {
    handle = await artifacts.get(name);
  } catch (error) {
    if (codeOf(error) !== "NOT_FOUND") {
      throw error;
    }
    await createRecordRepo(artifacts, name);
    handle = await artifacts.get(name);
  }
  const { source } = await handle.info();
  if (source !== null) {
    handle[Symbol.dispose]();
    throw new Error(`${name} is a fork, so it is not written as a record repo`);
  }
  return handle;
}

async function readBytes(blob: Blob | null): Promise<Uint8Array | null> {
  return blob === null ? null : new Uint8Array(await blob.arrayBuffer());
}

function asRecordRepo(handle: ArtifactsRepo, name: string, remote: string): RecordRepo {
  return {
    name,
    remote,
    head: async () => {
      const [newest] = await handle.log({ ref: MAIN_BRANCH, limit: 1 });
      if (newest === undefined) {
        return null;
      }
      const head = parseSha(newest.hash);
      if (head === undefined) {
        throw new Error(`main of ${name} has an unreadable head`);
      }
      return head;
    },
    readTree: (hash) => handle.readTree(hash),
    readCommit: (hash) => handle.readCommit(hash),
    readFile: async (ref, path) => readBytes(await handle.readFile({ ref, path })),
  };
}

/** What the record step reaches in a merge's sandbox. */
export interface RecordSandbox {
  ctx: DurableObjectState;
  env: Env;
  container: Container;
  /** The Artifacts git host the sandbox's HTTPS goes to. */
  host: string;
  fork: ArtifactsRepo;
  user: { user?: string };
}

/**
 * The record step's boundaries in a merge's sandbox, and `dispose`, which releases the record
 * repos it opened. Write tokens are minted per record repo, live in `RecordPushGateway` for one
 * fetch and one push, and are revoked when the push ends.
 */
export function recordDeps(sandbox: RecordSandbox): { deps: EvidenceDeps; dispose(): void } {
  const { ctx, env, container, host, fork, user } = sandbox;
  const handles = new Map<string, ArtifactsRepo>();
  const handleOf = (repo: RecordRepo): ArtifactsRepo => {
    const handle = handles.get(repo.name);
    if (handle === undefined) {
      throw new Error(`${repo.name} was not opened by the record step`);
    }
    return handle;
  };
  const deps: EvidenceDeps = {
    now: () => new Date(),
    openRepo: async (name) => {
      const handle = await openRecordRepo(env.ARTIFACTS, name);
      handles.set(name, handle);
      const { remote } = await handle.info();
      if (new URL(remote).hostname !== host) {
        throw new Error(`${name} is not on the merge's git host`);
      }
      return asRecordRepo(handle, name, remote);
    },
    readForkFile: async (ref, path) => {
      const blob = await fork.readFile({ ref, path });
      if (blob === null) {
        return null;
      }
      return { size: blob.size, bytes: async () => new Uint8Array(await blob.arrayBuffer()) };
    },
    withRecordAccess: async <T>(repo: RecordRepo, update: RecordUpdate, use: () => Promise<T>) => {
      const handle = handleOf(repo);
      const token = await handle.createToken("write", RECORD_TOKEN_TTL_SECONDS);
      const revoke = revokeOnce(
        () => handle.revokeToken(token.id),
        report("token_revoke_failed", repo.name),
      );
      try {
        const gateway = ctx.exports.RecordPushGateway({
          props: {
            remote: repo.remote,
            ref: `refs/heads/${MAIN_BRANCH}`,
            old: update.old ?? NO_COMMIT,
            new: update.new,
            token: token.plaintext,
          },
        });
        await container.interceptOutboundHttps(host, gateway);
        return await use();
      } finally {
        await revoke();
      }
    },
    run: (command) => runRecordCommand(container, command, user),
  };
  return {
    deps,
    dispose: () => {
      for (const handle of handles.values()) {
        handle[Symbol.dispose]();
      }
    },
  };
}

async function runRecordCommand(
  container: Container,
  command: RecordCommand,
  user: { user?: string },
): Promise<GitResult> {
  const captured = await execCaptured(
    container,
    "record",
    String(command.timeoutSeconds),
    command.argv,
    {
      env: { ...command.env, GIT_SSL_CAINFO: CONTAINER_CA_CERTIFICATE },
      stdin: new Blob([command.stdin.slice()]).stream(),
      ...user,
    },
  );
  return {
    ...captured,
    stdout: redactTokens(captured.stdout),
    stderr: redactTokens(captured.stderr),
  };
}
