/** Returns true when the repo is a fork, the only kind an agent may receive a write token for. */
export function isForkRepo(info: { source: string | null }): boolean {
  return info.source !== null;
}
