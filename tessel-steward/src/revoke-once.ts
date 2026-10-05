/**
 * Wraps a token revocation so it runs at most once and never throws.
 *
 * A failed or refused revocation is passed to `report` instead of replacing the outcome of the
 * run that called it. Later calls do nothing, so a backstop call cannot double-revoke or log a
 * false failure.
 *
 * @param revoke Revokes the token; resolves to false when the token was not revoked.
 * @param report Receives the reason a revocation did not happen.
 * @returns A function that revokes on its first call.
 */
export function revokeOnce(
  revoke: () => Promise<boolean>,
  report: (reason: string) => void,
): () => Promise<void> {
  let attempted = false;
  return async () => {
    if (attempted) {
      return;
    }
    attempted = true;
    try {
      if (!(await revoke())) {
        report("revokeToken returned false");
      }
    } catch (error) {
      report(error instanceof Error ? error.message : String(error));
    }
  };
}
