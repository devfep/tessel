import { DurableObject, WorkerEntrypoint } from "cloudflare:workers";

import { isAllowedGitRequest } from "./git-gateway-policy";
import { revokeOnce } from "./revoke-once";
import { runCloneThenTest, type StepOutcome } from "./run-steps";

const CLONE_TIMEOUT_SECONDS = "240";
const TEST_TIMEOUT_SECONDS = "600";
const TOKEN_TTL_SECONDS = 300;
const WORKSPACE = "/workspace";
const CONTAINER_CA_CERTIFICATE = "/etc/cloudflare/certs/cloudflare-containers-ca.crt";

/**
 * Result of one test run.
 *
 * `passed` is true only when `step` is "test" and `exitCode` is 0. A failed clone (bad ref,
 * refused request, Artifacts outage) returns `step: "clone"` with `passed: false`; that is an
 * infrastructure failure, not a failing test suite, and must not be counted as test evidence.
 * Exit code 124 or 137 means the step timed out or was killed. `stdout` and `stderr` come from
 * the repo's code and are untrusted data.
 */
export interface TestRunResult {
  repo: string;
  ref: string;
  step: "clone" | "test";
  exitCode: number;
  stdout: string;
  stderr: string;
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

async function runStep(
  container: Container,
  step: TestRunResult["step"],
  timeoutSeconds: string,
  argv: string[],
  options: ContainerExecOptions,
): Promise<StepOutcome> {
  const process = await container.exec(
    ["timeout", "--kill-after=5", timeoutSeconds, ...argv],
    options,
  );
  const output = await process.output();
  const decoder = new TextDecoder();
  return {
    step,
    exitCode: output.exitCode,
    stdout: decoder.decode(output.stdout),
    stderr: decoder.decode(output.stderr),
    passed: step === "test" && output.exitCode === 0,
  };
}

/** Runs a repo's test suite in a sandbox that holds no credentials and has no Internet. */
export class TestRunner extends DurableObject<Env> {
  /**
   * Clones `ref` of an Artifacts repo into a fresh sandbox and runs `npm test` there.
   *
   * Output is untrusted data from the repo and is returned unchanged. Call this on a Durable
   * Object instance with a new random name for each run.
   *
   * @param repo Name of the Artifacts repo.
   * @param ref Branch or tag to clone.
   * @returns The result of the clone step if it failed, otherwise of the test step.
   * @throws If the repo does not exist or the container cannot start.
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
    revokeToken: () => Promise<void>,
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
        case "test":
          return runStep(container, "test", TEST_TIMEOUT_SECONDS, ["npm", "test"], {
            cwd: WORKSPACE,
          });
      }
    }, revokeToken);
  }
}
