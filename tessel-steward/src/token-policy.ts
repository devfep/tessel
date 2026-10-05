/** Prefix of `info().source` for a fork of an Artifacts repo. */
const ARTIFACTS_FORK_SOURCE_PREFIX = "artifacts:";

/**
 * Returns true when the repo is a fork of an Artifacts repo, the only kind an agent may receive
 * a write token for. Imported repos (for example "github:owner/repo") and non-forks are not.
 */
export function isForkRepo(info: { source: string | null }): boolean {
  return info.source !== null && info.source.startsWith(ARTIFACTS_FORK_SOURCE_PREFIX);
}

/** The part of an Artifacts repo handle that minting a token uses. */
export interface TokenHandle<Info extends { source: string | null }, Token> {
  info(): Promise<Info>;
  createToken(scope: "write", ttlSeconds: number): Promise<Token>;
}

export type MintOutcome<Info, Token> =
  | { minted: true; info: Info; token: Token }
  | { minted: false };

/**
 * Mints a write token for a fork, reading the repo's info first so that nothing is created if
 * the info call fails or the repo is not a fork.
 *
 * @param handle Handle of the repo.
 * @param ttlSeconds Lifetime of the token.
 * @returns The token and info, or `{ minted: false }` for a repo that is not a fork.
 * @throws If `info()` or `createToken()` throws; no token exists when `info()` throws.
 */
export async function mintForkWriteToken<Info extends { source: string | null }, Token>(
  handle: TokenHandle<Info, Token>,
  ttlSeconds: number,
): Promise<MintOutcome<Info, Token>> {
  const info = await handle.info();
  if (!isForkRepo(info)) {
    return { minted: false };
  }
  const token = await handle.createToken("write", ttlSeconds);
  return { minted: true, info, token };
}
