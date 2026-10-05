/**
 * Wraps a token revocation so it runs at most once and never throws.
 *
 * A failed or refused revocation is passed to `report` and answered with false, instead of
 * replacing the outcome of the run that called it. Later calls do not revoke again and answer
 * with the first call's result, so a backstop call cannot double-revoke or log a false failure.
 *
 * @param revoke Revokes the token; resolves to false when the token was not revoked.
 * @param report Receives the reason a revocation did not happen.
 * @returns A function that revokes on its first call and resolves to whether it succeeded.
 */
export function revokeOnce(
  revoke: () => Promise<boolean>,
  report: (reason: string) => void,
): () => Promise<boolean> {
  let result: Promise<boolean> | undefined;
  const attempt = async (): Promise<boolean> => {
    try {
      if (await revoke()) {
        return true;
      }
      report("revokeToken returned false");
    } catch (error) {
      report(error instanceof Error ? error.message : String(error));
    }
    return false;
  };
  return () => {
    result ??= attempt();
    return result;
  };
}
