import {
  CONTAINER_CA_CERTIFICATE,
  WORKSPACE,
  execCaptured,
  runPackageStep,
} from "./container-step";
import { MAIN_BRANCH, NETWORK_TIMEOUT_SECONDS, type GitCommand } from "./merge-commands";
import { runMerge, runTrial, type MergeDeps, type TrialDeps } from "./merge-steps";
import {
  isForkOf,
  isSafeBranchName,
  type GitResult,
  parseSha,
  type MergeOutcome,
  type MergeRequest,
  type Sha,
  type TrialOutcome,
  type TrialSide,
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
    case "uncovered":
    case "main_moved":
    case "commit_not_in_fork":
      return outcome;
  }
}

/** Redacts Artifacts tokens from every stream of the trial outcome before it leaves the Worker. */
export function redactTrialOutcome(outcome: TrialOutcome): TrialOutcome {
  switch (outcome.outcome) {
    case "tests_failed":
    case "install":
    case "clone":
      return { ...outcome, result: redactOutput(outcome.result) };
    case "git_failed":
      return { ...outcome, result: redactOutput(outcome.result) };
    case "clean":
    case "conflict":
    case "nothing_to_test":
    case "commit_not_in_fork":
    case "main_unreachable":
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
 * What a trial is given inside the sandbox: the boundaries of a trial and nothing else. There is
 * no repo handle in it, so a trial cannot mint a token of any scope (`merge-trial-types.test.ts`).
 */
export interface TrialSandbox {
  deps: TrialDeps;
}

/** What a merge is given inside the sandbox: a trial's, and the pieces a push needs. */
interface Sandbox extends TrialSandbox {
  container: Container;
  main: ArtifactsRepo;
  mainRemote: string;
  host: string;
}

/**
 * Starts the sandbox for `forkName` of `repo`, calls `use`, and always tears it down. Read tokens
 * for main and the fork are minted first and live in `MergeReadGateway` until the fetch ends.
 * The sandbox has no Internet and no credentials.
 *
 * @throws If the fork is not a fork of `repo`, a repo is missing, the container cannot start,
 *   or a read token could not be revoked (no repo code runs in that case).
 */
async function withSandbox<T>(
  ctx: DurableObjectState,
  env: Env,
  repo: string,
  forkName: string,
  use: (sandbox: Sandbox) => Promise<T>,
): Promise<T> {
  const container = ctx.container;
  if (!container) {
    throw new Error("The container binding is not configured");
  }
  using main = await env.ARTIFACTS.get(repo);
  using fork = await env.ARTIFACTS.get(forkName);
  const [mainInfo, forkInfo] = await Promise.all([main.info(), fork.info()]);
  if (!isForkOf(repo, forkInfo)) {
    throw new Error(`${forkName} is not a fork of ${repo}`);
  }
  if (!isSafeBranchName(forkInfo.defaultBranch)) {
    throw new Error(`${forkName} has an unusable default branch name`);
  }
  const host = new URL(mainInfo.remote).hostname;
  if (new URL(forkInfo.remote).hostname !== host) {
    throw new Error(`${repo} and ${forkName} are not on the same git host`);
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
      [fork, forkInfo.remote, forkName],
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

    const deps: TrialDeps = {
      sources: {
        workspace: WORKSPACE,
        mainRemote: mainInfo.remote,
        forkRemote: forkInfo.remote,
        forkBranch: forkInfo.defaultBranch,
      },
      run: (command) => runGit(container, command),
      revokeReadTokens,
      runPackageStep: async (step) => redactOutput(await runPackageStep(container, step)),
    };
    return await use({ deps, container, main, mainRemote: mainInfo.remote, host });
  } finally {
    await destroyContainer(container, repo);
    await revokeReadTokens();
  }
}

/**
 * Merges the fork's commit into main inside the DO's container. See `runMerge` for the order of
 * steps and `MergeOutcome` for the results.
 *
 * The write token is minted by `withPushAccess` after the tests passed, lives in
 * `MergePushGateway` for the one push, and is revoked straight after. Call this on a Durable
 * Object instance with a new random name for each merge. See `withSandbox` for the rest.
 *
 * Known limit: the repo's tests run as the same user as the rest of the container, so a process
 * they detach (setsid, nohup) can outlive `timeout` and still run when the write token is minted.
 * It cannot use the token: the token is held by `MergePushGateway`, which forwards only the one
 * pinned update, and the outcome is decided by a read of main made by the Worker, not by the
 * sandbox. Isolating the tests under another uid is not built.
 *
 * @throws As `withSandbox` does.
 */
export async function executeMerge(
  ctx: DurableObjectState,
  env: Env,
  repo: string,
  request: MergeRequest,
): Promise<MergeOutcome> {
  return withSandbox(
    ctx,
    env,
    repo,
    request.fork,
    async ({ deps, container, main, mainRemote, host }) => {
      const mergeDeps: MergeDeps = {
        ...deps,
        withPushAccess: async (update, use) => {
          const token = await main.createToken("write", WRITE_TOKEN_TTL_SECONDS);
          const revoke = revokeOnce(
            () => main.revokeToken(token.id),
            reportRevokeFailure(repo, token.id),
          );
          try {
            const gateway = ctx.exports.MergePushGateway({
              props: {
                remote: mainRemote,
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
      return redactOutcome(await runMerge(mergeDeps, request.commit, request.scopes));
    },
  );
}

/**
 * Tries the fork's commit on main at `request.main` inside the DO's
 * container and tests it. See `runTrial` for the steps and `TrialOutcome` for the results.
 * Nothing is pushed and no write
 * token is created: this function never calls `createToken("write")`, and the deps it passes
 * have no way to. Call this on a Durable Object instance with a new random name for each trial.
 *
 * @throws As `withSandbox` does.
 */
export async function executeTrial(
  ctx: DurableObjectState,
  env: Env,
  repo: string,
  request: TrialSide,
): Promise<TrialOutcome> {
  return withSandbox(ctx, env, repo, request.fork, async ({ deps }: TrialSandbox) =>
    redactTrialOutcome(await runTrial(deps, request.main, request.commit)),
  );
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
