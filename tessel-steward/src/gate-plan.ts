import type { Sha } from "./merge-types";
import {
  MAX_CONFIG_BYTES,
  TESSEL_TOML_PATH,
  parseGateConfig,
  type GateConfig,
  type GateInstance,
} from "./tessel-config";

/** Image for repos without a `tessel.toml`: Node only, small enough for a `lite` instance. */
export const LEGACY_IMAGE = "tests";
/** Image for repos with a `tessel.toml`: Rust, Node and pnpm, with dependencies baked in. */
export const TOOLCHAIN_IMAGE = "toolchain";

/**
 * How a run is gated, decided from the trunk commit before the container starts.
 * - `legacy`: the trunk has no `tessel.toml`: `npm test` in a `lite` instance, refusing repos
 *   that declare dependencies.
 * - `invalid`: the trunk has a `tessel.toml` the steward cannot accept. It never falls back to
 *   `npm test`: the run fails at `install` with reason `config`.
 * - `configured`: the trunk's `tessel.toml`.
 */
export type GatePlan =
  | { kind: "legacy" }
  | { kind: "invalid" }
  | { kind: "configured"; config: GateConfig };

/** What a gate plan is read through: the file API of an Artifacts repo handle. */
export type FileSource = Pick<ArtifactsRepo, "readFile">;

/**
 * Reads `tessel.toml` at `commit` of the trunk through the Artifacts binding. The caller passes
 * the repo handle of the trunk (never a fork) and the exact commit the run is based on, so a
 * fork can neither supply nor change the gate it is judged by.
 *
 * A missing file is a `legacy` plan. An oversized, undecodable, unreadable or invalid file is an
 * `invalid` plan. Only an unexpected Artifacts failure throws.
 */
export async function readGatePlan(trunk: FileSource, commit: Sha): Promise<GatePlan> {
  let blob: Blob | null;
  try {
    blob = await trunk.readFile({ ref: commit, path: TESSEL_TOML_PATH });
  } catch (error) {
    if (error instanceof Error && (error as { code?: unknown }).code === "MEMORY_LIMIT") {
      return { kind: "invalid" };
    }
    throw error;
  }
  if (blob === null) {
    return { kind: "legacy" };
  }
  if (blob.size > MAX_CONFIG_BYTES) {
    return { kind: "invalid" };
  }
  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(
      await blob.arrayBuffer(),
    );
  } catch {
    return { kind: "invalid" };
  }
  const parsed = parseGateConfig(text);
  return parsed.ok ? { kind: "configured", config: parsed.config } : { kind: "invalid" };
}

/** Options for `Container.start`: always an explicit instance and never the Internet. */
export interface StartOptions {
  image: string;
  enableInternet: false;
  instance: "lite" | GateInstance;
  entrypoint?: string[];
}

/**
 * PID 1 of the toolchain container: `tini` reaps the processes the gate's tests detach (a daemon
 * whose parent exits is reparented to PID 1). Without a reaper a killed child stays a zombie that
 * `kill -0` still reports alive, and tests that wait for a process to go never finish.
 */
export const TOOLCHAIN_ENTRYPOINT = ["/usr/bin/tini", "--", "sleep", "infinity"];

/**
 * The image and instance for a plan. The `durable_object` scheduling policy defaults to `lite`
 * when `instance` is left out, so every start names its instance.
 *
 * @throws If the image the plan needs is not configured.
 */
export function startOptions(plan: GatePlan, images: Record<string, string>): StartOptions {
  const name = plan.kind === "configured" ? TOOLCHAIN_IMAGE : LEGACY_IMAGE;
  const image = images[name];
  if (image === undefined) {
    throw new Error(`The container image "${name}" is not configured`);
  }
  if (plan.kind === "configured") {
    return {
      image,
      enableInternet: false,
      instance: plan.config.instance,
      entrypoint: TOOLCHAIN_ENTRYPOINT,
    };
  }
  return { image, enableInternet: false, instance: "lite" };
}
