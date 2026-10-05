import {
  CONTAINER_CA_CERTIFICATE,
  WORKSPACE,
  execCaptured,
  runPackageStep,
} from "./container-step";
import { MAIN_BRANCH, NETWORK_TIMEOUT_SECONDS, type GitCommand } from "./merge-commands";
import { runMerge, type MergeDeps } from "./merge-steps";
import {
  isForkOf,
  isSafeBranchName,
  type GitResult,
  parseSha,
  type MergeOutcome,
  type MergeRequest,
  type Sha,
} from "./merge-types";
import { redactTokens } from "./redact";
import { revokeOnce } from "./revoke-once";

/**
 * Both read tokens are minted before the container starts and used by two network steps in turn
 * (clone, then fetch), each with its own timeout, so the lifetime covers both plus a margin.
 */
const READ_TOKEN_TTL_SECONDS = 2 * NETWORK_TIMEOUT_SECONDS + 60;
/** The push is two requests seconds apart; 60 s is the shortest lifetime Artifacts allows. */
const WRITE_TOKEN_TTL_SECONDS = 60;

function redactOutput<T extends { stdout: string; stderr: string }>(output: T): T {
  return { ...output, stdout: redactTokens(output.stdout), stderr: redactTokens(output.stderr) };
}

/** Redacts Artifacts tokens from every stream of the outcome before it leaves the Worker. */
export function redactOutcome(outcome: MergeOutcome): MergeOutcome {
  switch (outcome.outcome) {
    case "tests_failed":
    case "install":
    case "clone":
      return { ...outcome, result: redactOutput(outcome.result) };
    case "git_failed":
    case "push_failed":
      return { ...outcome, result: redactOutput(outcome.result) };
    case "merged":
    case "already_merged":
    case "conflict":
    case "main_moved":
    case "commit_not_in_fork":
      return outcome;
  }
}

function reportRevokeFailure(repo: string, tokenId: string): (reason: string) => void {
  return (reason) =>
    console.error(
      JSON.stringify({
        event: "token_revoke_failed",
        repo,
        tokenId,
        reason: redactTokens(reason),
      }),
    );
}

/**
 * Merges the fork's commit into main inside the DO's container. See `runMerge` for the order of
 * steps and `MergeOutcome` for the results.
 *
 * The sandbox has no Internet and no credentials. Read tokens live in `MergeReadGateway` until
 * the fetch ends. The write token is minted by `withPushAccess` after the tests passed, lives
 * in `MergePushGateway` for the one push, and is revoked straight after. Call this on a Durable
 * Object instance with a new random name for each merge.
 *
 * Known limit: the repo's tests run as the same user as the rest of the container, so a process
 * they detach (setsid, nohup) can outlive `timeout` and still run when the write token is minted.
 * It cannot use the token: the token is held by `MergePushGateway`, which forwards only the one
 * pinned update, and the outcome is decided by a read of main made by the Worker, not by the
 * sandbox. Isolating the tests under another uid is not built.
 *
 * @throws If the fork is not a fork of `repo`, a repo is missing, the container cannot start,
 *   or a read token could not be revoked (no repo code runs in that case).
 */
export async function executeMerge(
  ctx: DurableObjectState,
  env: Env,
  repo: string,
  request: MergeRequest,
): Promise<MergeOutcome> {
  const container = ctx.container;
  if (!container) {
    throw new Error("The container binding is not configured");
  }
  using main = await env.ARTIFACTS.get(repo);
  using fork = await env.ARTIFACTS.get(request.fork);
  const [mainInfo, forkInfo] = await Promise.all([main.info(), fork.info()]);
  if (!isForkOf(repo, forkInfo)) {
    throw new Error(`${request.fork} is not a fork of ${repo}`);
  }
  if (!isSafeBranchName(forkInfo.defaultBranch)) {
    throw new Error(`${request.fork} has an unusable default branch name`);
  }
  const host = new URL(mainInfo.remote).hostname;
  if (new URL(forkInfo.remote).hostname !== host) {
    throw new Error(`${repo} and ${request.fork} are not on the same git host`);
  }
  const image = container.images["tests"];
  if (image === undefined) {
    throw new Error('The container image "tests" is not configured');
  }

  const revokers: Array<() => Promise<boolean>> = [];
  const revokeReadTokens = async (): Promise<boolean> => {
    const results = await Promise.all(revokers.map((revoke) => revoke()));
    return results.every(Boolean);
  };
  try {
    const routes: Array<{ remote: string; token: string }> = [];
    for (const [handle, remote, name] of [
      [main, mainInfo.remote, repo],
      [fork, forkInfo.remote, request.fork],
    ] as const) {
      const token = await handle.createToken("read", READ_TOKEN_TTL_SECONDS);
      revokers.push(
        revokeOnce(() => handle.revokeToken(token.id), reportRevokeFailure(name, token.id)),
      );
      routes.push({ remote, token: token.plaintext });
    }
    await container.interceptOutboundHttps(
      host,
      ctx.exports.MergeReadGateway({ props: { routes } }),
    );
    container.start({ image, enableInternet: false });

    const deps: MergeDeps = {
      sources: {
        workspace: WORKSPACE,
        mainRemote: mainInfo.remote,
        forkRemote: forkInfo.remote,
        forkBranch: forkInfo.defaultBranch,
      },
      run: (command) => runGit(container, command),
      revokeReadTokens,
      runPackageStep: async (step) => redactOutput(await runPackageStep(container, step)),
      withPushAccess: async (update, use) => {
        const token = await main.createToken("write", WRITE_TOKEN_TTL_SECONDS);
        const revoke = revokeOnce(
          () => main.revokeToken(token.id),
          reportRevokeFailure(repo, token.id),
        );
        try {
          const gateway = ctx.exports.MergePushGateway({
            props: {
              remote: mainInfo.remote,
              ref: `refs/heads/${MAIN_BRANCH}`,
              old: update.base,
              new: update.head,
              token: token.plaintext,
            },
          });
          await container.interceptOutboundHttps(host, gateway);
          return await use();
        } finally {
          await revoke();
        }
      },
      currentMain: () => readMainHead(main),
    };
    return redactOutcome(await runMerge(deps, request.commit));
  } finally {
    await destroyContainer(container, repo);
    await revokeReadTokens();
  }
}

async function runGit(container: Container, command: GitCommand): Promise<GitResult> {
  const captured = await execCaptured(
    container,
    "git",
    String(command.timeoutSeconds),
    command.argv,
    { env: { ...command.env, GIT_SSL_CAINFO: CONTAINER_CA_CERTIFICATE } },
  );
  return redactOutput(captured);
}

async function readMainHead(main: ArtifactsRepo): Promise<Sha | null> {
  try {
    const [newest] = await main.log({ ref: MAIN_BRANCH, limit: 1 });
    return parseSha(newest?.hash) ?? null;
  } catch (error) {
    console.error(
      JSON.stringify({
        event: "main_read_failed",
        reason: redactTokens(error instanceof Error ? error.message : String(error)),
      }),
    );
    return null;
  }
}

async function destroyContainer(container: Container, repo: string): Promise<void> {
  try {
    if (container.running) {
      await container.destroy();
    }
  } catch (error) {
    console.error(
      JSON.stringify({
        event: "container_destroy_failed",
        repo,
        reason: redactTokens(error instanceof Error ? error.message : String(error)),
      }),
    );
  }
}
