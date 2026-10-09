import { fetchSummary, openCoordinatorSocket, type LinkEnv } from "./coordinator-link";

/** How long reading the whole log may take before the page gives up. */
export const LOG_READ_WAIT_MS = 10_000;

function parse(data: unknown): Record<string, unknown> | undefined {
  if (typeof data !== "string") {
    return undefined;
  }
  try {
    const value: unknown = JSON.parse(data);
    return typeof value === "object" && value !== null && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : undefined;
  } catch {
    return undefined;
  }
}

/**
 * Reads the repo's whole event log through the coordinator's `Watch`, as `agent`, up to the last
 * event the summary reports. No `Hello` is sent, so reading writes nothing to the log.
 *
 * Known limit: this replays from seq 0 every time and gives up after `waitMs`, so a very large
 * log will time out. Folding from a summary instead is a later change.
 *
 * @throws If the summary or the socket is refused, the socket closes early, or `waitMs` passes.
 */
export async function readEvents(
  env: LinkEnv,
  repo: string,
  agent: string,
  waitMs: number = LOG_READ_WAIT_MS,
): Promise<unknown[]> {
  const response = await fetchSummary(env, repo, agent);
  if (!response.ok) {
    throw new Error(`the coordinator answered ${response.status} for the summary`);
  }
  const { head_seq: head } = (await response.json()) as { head_seq?: unknown };
  if (typeof head !== "number") {
    return [];
  }
  const socket = await openCoordinatorSocket(env, repo, agent);
  try {
    return await new Promise<unknown[]>((resolve, reject) => {
      const events: unknown[] = [];
      let settled = false;
      const timer = setTimeout(() => reject(new Error("reading the event log timed out")), waitMs);
      const stop = (): void => {
        settled = true;
        clearTimeout(timer);
      };
      const fail = (reason: string) => (): void => {
        stop();
        reject(new Error(reason));
      };
      socket.addEventListener("message", ({ data }) => {
        const message = parse(data);
        if (settled) {
          return;
        }
        const event = message?.["event"] as { seq?: unknown } | undefined;
        if (message?.["type"] === "error") {
          fail(`the coordinator refused the watch: ${String(message["message"])}`)();
        } else if (message?.["type"] === "event" && typeof event?.seq === "number") {
          events.push(event);
          if (event.seq >= head) {
            stop();
            resolve(events);
          }
        }
      });
      socket.addEventListener("close", fail("the coordinator closed the log early"));
      socket.addEventListener("error", fail("the coordinator socket failed"));
      socket.send(JSON.stringify({ type: "watch", from_seq: 0 }));
    });
  } finally {
    socket.close();
  }
}
