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

/** Why a repo is on the legacy path: the trunk has no `tessel.toml`, or it is not acceptable. */
export type ConfigIssue = "missing" | "invalid";

/**
 * How a run is gated, decided from the trunk commit before the container starts.
 * - `legacy`: `npm test` in a `lite` instance, refusing repos that declare dependencies.
 * - `configured`: the trunk's `tessel.toml`.
 */
export type GatePlan =
  | { kind: "legacy"; issue: ConfigIssue }
  | { kind: "configured"; config: GateConfig };

/** What a gate plan is read through: the file API of an Artifacts repo handle. */
export type FileSource = Pick<ArtifactsRepo, "readFile">;

/**
 * Reads `tessel.toml` at `commit` of the trunk through the Artifacts binding. The caller passes
 * the repo handle of the trunk (never a fork) and the exact commit the run is based on, so a
 * fork can neither supply nor change the gate it is judged by.
 *
 * A missing, oversized, undecodable or invalid file is a `legacy` plan, which refuses a repo
 * that declares dependencies. Only an unexpected Artifacts failure throws.
 */
export async function readGatePlan(trunk: FileSource, commit: Sha): Promise<GatePlan> {
  let blob: Blob | null;
  try {
    blob = await trunk.readFile({ ref: commit, path: TESSEL_TOML_PATH });
  } catch (error) {
    if (error instanceof Error && (error as { code?: unknown }).code === "MEMORY_LIMIT") {
      return { kind: "legacy", issue: "invalid" };
    }
    throw error;
  }
  if (blob === null) {
    return { kind: "legacy", issue: "missing" };
  }
  if (blob.size > MAX_CONFIG_BYTES) {
    return { kind: "legacy", issue: "invalid" };
  }
  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(
      await blob.arrayBuffer(),
    );
  } catch {
    return { kind: "legacy", issue: "invalid" };
  }
  const parsed = parseGateConfig(text);
  return parsed.ok
    ? { kind: "configured", config: parsed.config }
    : { kind: "legacy", issue: "invalid" };
}

/** Options for `Container.start`: always an explicit instance and never the Internet. */
export interface StartOptions {
  image: string;
  enableInternet: false;
  instance: "lite" | GateInstance;
}

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
  const instance = plan.kind === "configured" ? plan.config.instance : "lite";
  return { image, enableInternet: false, instance };
}
