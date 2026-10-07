import { DurableObject, WorkerEntrypoint } from "cloudflare:workers";

import { isValidName } from "./identity";
import { isAllowedGitRequest } from "./git-gateway-policy";
import { revokeOnce } from "./revoke-once";
import {
  CONTAINER_CA_CERTIFICATE,
  WORKSPACE,
  runPackageStep,
  runStep,
  userOptions,
} from "./container-step";
import { executeDiff, executeMerge, executeTrial } from "./merge-executor";
import type { DiffOutcome } from "./review-diff";
import {
  parseMergeRequest,
  parseTrialSide,
  type MergeOutcome,
  type TrialOutcome,
} from "./merge-types";
import { readGatePlan, startOptions, type GatePlan } from "./gate-plan";
import { parseSha } from "./merge-types";
import {
  cloneAtCommit,
  runCloneThenTest,
  type Measurement,
  type StepOutcome,
  type StepReason,
} from "./run-steps";
import { STEP_SECONDS } from "./step-budget";

const TOKEN_TTL_SECONDS = 300;

/**
 * Result of one test run.
 *
 * `passed` is true only when `step` is "test" and `exitCode` is 0. Steps "clone" and "install"
 * are infrastructure outcomes, not test evidence: "clone" is a failed clone (bad ref, refused
 * request, Artifacts outage); "install" is a repo the runner refused because it declares
 * dependencies (or its package.json could not be read) and this runner cannot install them yet,
 * so its tests never ran; `exitCode` is then the dependency check's and `stderr` is a fixed
 * message written by the steward. `install` is also the outcome of a gate whose install command
 * failed, or whose test step used up its time share or was killed (exit 124 or 137): never a
 * test failure. `reason` says which, as a fixed value. `measurement` holds the wall time and
 * the container's peak memory of the test step, measured by the steward. `stdout` and `stderr` come from the repo's code and are untrusted data; each is capped
 * at 256 KiB, keeping the end, and `stdoutTruncated` / `stderrTruncated` say when the beginning
 * was dropped.
 */
export interface TestRunResult {
  repo: string;
  ref: string;
  step: "clone" | "install" | "test";
  exitCode: number;
  stdout: string;
  stderr: string;
  stdoutTruncated: boolean;
  stderrTruncated: boolean;
  passed: boolean;
  reason?: StepReason;
  measurement?: Measurement;
}

interface GatewayProps {
  remote: string;
  token: string;
}

/**
 * Receives every HTTPS request the sandbox makes to the Artifacts git host.
 *
 * It forwards only the read requests of `git clone` for the one repo of the run and adds the
 * repo token, so the token never enters the sandbox and the sandbox cannot push or read
 * other repos.
 */
export class ArtifactsGitGateway extends WorkerEntrypoint<Env, GatewayProps> {
  override async fetch(request: Request): Promise<Response> {
    const { remote, token } = this.ctx.props;
    if (!isAllowedGitRequest(request, remote)) {
      return new Response("Forbidden", { status: 403 });
    }
    const headers = new Headers(request.headers);
    headers.set("Authorization", `Bearer ${token}`);
    return fetch(new Request(request, { headers, redirect: "manual" }));
  }
}

/** Runs a repo's test suite in a sandbox that holds no credentials and has no Internet. */
export class TestRunner extends DurableObject<Env> {
  /**
   * Merges `commit` of `fork` into main of `repo`: rebase, test, push with a lease. See
   * `MergeOutcome` for the results and `executeMerge` for the tokens and the sandbox.
   *
   * Call this on a Durable Object instance with a new random name for each merge.
   *
   * @throws If an argument is invalid, `fork` is not a fork of `repo`, the container cannot
   *   start, or a read token could not be revoked.
   */
  async merge(
    repo: string,
    fork: string,
    commit: string,
    scopes: unknown,
    adminMerge: boolean,
  ): Promise<MergeOutcome> {
    const parsed = parseMergeRequest({ fork, commit, scopes });
    if (!parsed.ok || !isValidName(repo)) {
      throw new Error(
        "merge needs a repo name, a fork name, a 40-hex commit and the claim's scopes",
      );
    }
    return executeMerge(this.ctx, this.env, repo, parsed.request, adminMerge === true);
  }

  /**
   * Tries `commit` of `fork` on main of `repo` as it
   * was at `main`, and runs its tests. Never merges, never pushes, never has a write token. See
   * `TrialOutcome` for the results and `executeTrial` for the sandbox.
   *
   * Call this on a Durable Object instance with a new random name for each trial: that is what
   * gives each run its own container.
   *
   * @throws If an argument is invalid, `fork` is not a fork of `repo`, the container cannot
   *   start, or a read token could not be revoked.
   */
  async trial(repo: string, fork: string, main: string, commit: string): Promise<TrialOutcome> {
    const parsed = parseTrialSide({ fork, main, commit });
    if (!parsed.ok || !isValidName(repo)) {
      throw new Error("trial needs a repo name, a fork name, a 40-hex main and a 40-hex commit");
    }
    return executeTrial(this.ctx, this.env, repo, parsed.request);
  }

  /**
   * Reads what `commit` of `fork` changes relative to main of `repo`, for a person to review. Holds
   * read tokens only and runs git only. See `runDiff`.
   *
   * Call this on a Durable Object instance with a new random name for each diff.
   *
   * @throws If an argument is invalid, `fork` is not a fork of `repo`, the container cannot
   *   start, or a read token could not be revoked.
   */
  async diff(repo: string, fork: string, commit: string): Promise<DiffOutcome> {
    const sha = parseSha(commit);
    if (sha === undefined || !isValidName(repo) || !isValidName(fork)) {
      throw new Error("diff needs a repo name, a fork name and a 40-hex commit");
    }
    return executeDiff(this.ctx, this.env, repo, fork, sha);
  }

  /**
   * Clones `ref` of an Artifacts repo into a fresh sandbox and runs `npm test` there.
   *
   * Output is untrusted data from the repo, capped at 256 KiB per stream (the end is kept). Call
   * this on a Durable Object instance with a new random name for each run. The token is revoked
   * after the clone and before any repo code runs.
   *
   * @param repo Name of the Artifacts repo.
   * @param ref Branch or tag to clone.
   * @returns The clone outcome if the clone failed, an "install" outcome if the repo declares
   *   dependencies (or the dependency check did not complete), otherwise the test outcome.
   * @throws If the repo does not exist, the container cannot start, or the token could not be
   *   revoked (no repo code runs in that case).
   */
  async runTests(repo: string, ref: string): Promise<TestRunResult> {
    const container = this.ctx.container;
    if (!container) {
      throw new Error("The container binding is not configured");
    }
    using handle = await this.env.ARTIFACTS.get(repo);
    const token = await handle.createToken("read", TOKEN_TTL_SECONDS);
    const revoke = revokeOnce(
      () => handle.revokeToken(token.id),
      (reason) =>
        console.error(
          JSON.stringify({ event: "token_revoke_failed", repo, tokenId: token.id, reason }),
        ),
    );
    try {
      const { remote } = await handle.info();
      const gateway = this.ctx.exports.ArtifactsGitGateway({
        props: { remote, token: token.plaintext },
      });
      await container.interceptOutboundHttps(new URL(remote).hostname, gateway);
      const commit = await resolveRef(handle, ref);
      const plan = await readGatePlan(handle, commit);
      container.start(startOptions(plan, container.images));
      const result = await this.cloneAndTest(container, plan, remote, { ref, commit }, revoke);
      return { repo, ref, ...result };
    } finally {
      try {
        if (container.running) {
          await container.destroy();
        }
      } catch (error) {
        console.error(
          JSON.stringify({
            event: "container_destroy_failed",
            repo,
            tokenId: token.id,
            reason: error instanceof Error ? error.message : String(error),
          }),
        );
      }
      await revoke();
    }
  }

  private cloneAndTest(
    container: Container,
    plan: GatePlan,
    remote: string,
    source: { ref: string; commit: string },
    revokeToken: () => Promise<boolean>,
  ): Promise<StepOutcome> {
    return runCloneThenTest((step) => {
      switch (step) {
        case "clone": {
          const options = {
            env: { GIT_SSL_CAINFO: CONTAINER_CA_CERTIFICATE },
            ...userOptions(plan),
          };
          return cloneAtCommit(
            () =>
              runStep(
                container,
                "clone",
                String(STEP_SECONDS.clone),
                ["git", "clone", "--depth=1", `--branch=${source.ref}`, "--", remote, WORKSPACE],
                options,
              ),
            () =>
              runStep(
                container,
                "clone",
                String(STEP_SECONDS.local),
                ["git", "-C", WORKSPACE, "rev-parse", "--verify", "HEAD"],
                userOptions(plan),
              ),
            source.commit,
          );
        }
        case "install":
        case "test":
          return runPackageStep(container, plan, step);
      }
    }, revokeToken);
  }
}

async function resolveRef(handle: ArtifactsRepo, ref: string) {
  const [newest] = await handle.log({ ref, limit: 1 });
  const commit = parseSha(newest?.hash);
  if (commit === undefined) {
    throw new Error(`The ref ${ref} could not be resolved to a commit`);
  }
  return commit;
}
