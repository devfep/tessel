import { DurableObject, WorkerEntrypoint } from "cloudflare:workers";

import { isValidName } from "./identity";
import { isAllowedGitRequest } from "./git-gateway-policy";
import { revokeOnce } from "./revoke-once";
import { CONTAINER_CA_CERTIFICATE, WORKSPACE, runPackageStep, runStep } from "./container-step";
import { executeMerge } from "./merge-executor";
import { parseMergeRequest, type MergeOutcome } from "./merge-types";
import { runCloneThenTest, type StepOutcome } from "./run-steps";

const CLONE_TIMEOUT_SECONDS = "240";
const TOKEN_TTL_SECONDS = 300;

/**
 * Result of one test run.
 *
 * `passed` is true only when `step` is "test" and `exitCode` is 0. Steps "clone" and "install"
 * are infrastructure outcomes, not test evidence: "clone" is a failed clone (bad ref, refused
 * request, Artifacts outage); "install" is a repo the runner refused because it declares
 * dependencies (or its package.json could not be read) and this runner cannot install them yet,
 * so its tests never ran; `exitCode` is then the dependency check's and `stderr` is a fixed
 * message written by the coordinator. Exit code 124 or 137 means the step timed out or was
 * killed. `stdout` and `stderr` come from the repo's code and are untrusted data; each is capped
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
  async merge(repo: string, fork: string, commit: string): Promise<MergeOutcome> {
    const parsed = parseMergeRequest({ fork, commit });
    if (!parsed.ok || !isValidName(repo)) {
      throw new Error("merge needs a repo name, a fork name and a 40-hex commit");
    }
    return executeMerge(this.ctx, this.env, repo, parsed.request);
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
      const image = container.images["tests"];
      if (image === undefined) {
        throw new Error('The container image "tests" is not configured');
      }
      container.start({ image, enableInternet: false });
      const result = await this.cloneAndTest(container, remote, ref, revoke);
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
    remote: string,
    ref: string,
    revokeToken: () => Promise<boolean>,
  ): Promise<StepOutcome> {
    return runCloneThenTest((step) => {
      switch (step) {
        case "clone":
          return runStep(
            container,
            "clone",
            CLONE_TIMEOUT_SECONDS,
            ["git", "clone", "--depth=1", `--branch=${ref}`, "--", remote, WORKSPACE],
            { env: { GIT_SSL_CAINFO: CONTAINER_CA_CERTIFICATE } },
          );
        case "install":
        case "test":
          return runPackageStep(container, step);
      }
    }, revokeToken);
  }
}
