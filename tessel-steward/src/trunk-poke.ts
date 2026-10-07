import { isValidName, signIdentityToken } from "./identity";
import type { Push } from "./push-event";

const TRUNK_REF = "refs/heads/main";
/**
 * Separates a fork's name from its trunk's: `<trunk>--<agent>`. Nothing stops a trunk from holding
 * it; what bounds the pokes is the coordinator, which ignores a repo it holds no state for.
 */
const FORK_SEPARATOR = "--";
/** The token opens one request, so it only needs to outlive that. */
const POKE_TOKEN_TTL_MS = 60 * 1000;
const COORDINATOR_ORIGIN = "https://coordinator.internal";
const STEWARD_AGENT = "steward";

type PokeEnv = Pick<Env, "IDENTITY_SIGNING_KEY" | "COORDINATOR">;

/**
 * Whether a push moved a trunk's main: the ref is `refs/heads/main` on a repo that is not an
 * agent's fork. A push to a fork, or to any other ref, does not change what the coordinator
 * calls main.
 */
export function movesTrunkMain(push: Push): boolean {
  return push.ref === TRUNK_REF && !push.repo.includes(FORK_SEPARATOR);
}

/**
 * Tells the coordinator for `repo` that its main moved. The coordinator takes the request only as
 * a trigger and reads the head itself through the steward, so the body is empty and nothing the
 * push event carried is forwarded.
 *
 * @throws If `repo` is not a valid name, or the coordinator does not answer 2xx.
 */
export async function pokeTrunkMoved(env: PokeEnv, repo: string): Promise<void> {
  if (!isValidName(repo)) {
    throw new Error(`cannot poke the coordinator for repo ${JSON.stringify(repo)}`);
  }
  const token = await signIdentityToken(env.IDENTITY_SIGNING_KEY, {
    repo,
    agent: STEWARD_AGENT,
    expMs: Date.now() + POKE_TOKEN_TTL_MS,
  });
  const response = await env.COORDINATOR.fetch(`${COORDINATOR_ORIGIN}/repo/${repo}/trunk-moved`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!response.ok) {
    throw new Error(`the coordinator answered ${response.status} for the trunk poke on ${repo}`);
  }
}
