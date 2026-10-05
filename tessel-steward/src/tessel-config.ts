import { parse } from "smol-toml";

/** Where a repo declares its gate, at the root of the trunk commit. */
export const TESSEL_TOML_PATH = "tessel.toml";
export const MAX_CONFIG_BYTES = 4096;

/** Instances a repo may ask for. `lite` is not listed: it is what a repo without a gate gets. */
export const GATE_INSTANCES = ["standard-1", "standard-2", "standard-3", "standard-4"] as const;
export type GateInstance = (typeof GATE_INSTANCES)[number];

/**
 * The only gate commands: `pnpm test` and `npm test` exactly, and `cargo test` followed by flags
 * from this list. Nothing that can change which toolchain, config or compiler runs is on it: no
 * `+toolchain`, `--config`, `-Z`, `--manifest-path`, `--target`, no `--` passthrough, and no
 * environment (an argument such as `RUSTC_WRAPPER=x` is not a program). A shell is never allowed.
 */
export const CARGO_TEST_FLAGS: readonly string[] = [
  "--workspace",
  "--locked",
  "--offline",
  "--no-fail-fast",
  "--all-targets",
  "--all-features",
  "--release",
  "--lib",
  "--bins",
  "--tests",
];

const MAX_TESTS = 6;
const MAX_PNPM_DIRS = 4;
const MAX_ARGV = 24;
const MAX_ARG_LENGTH = 200;
/** No quote, space, `$`, `;`, `&`, `|`, backtick, parenthesis or glob character is ever needed. */
const ARG_PATTERN = /^[A-Za-z0-9._/=:@+,%-]+$/;
const DIR_PATTERN = /^[A-Za-z0-9_][A-Za-z0-9._-]*(\/[A-Za-z0-9_][A-Za-z0-9._-]*)*$/;

/** One gate command: `argv` run with `dir` (relative to the repo root) as working directory. */
export interface TestCommand {
  dir: string;
  argv: string[];
}

/** A validated `tessel.toml`. */
export interface GateConfig {
  instance: GateInstance;
  /** Run `cargo fetch --locked --offline` before the tests. */
  installCargo: boolean;
  /** Directories in which the steward runs `pnpm install` (offline, frozen lockfile, no scripts). */
  installPnpm: string[];
  tests: TestCommand[];
}

export type ParsedGateConfig = { ok: true; config: GateConfig } | { ok: false; error: string };

type Table = Record<string, unknown>;

function isTable(value: unknown): value is Table {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function unknownKey(table: Table, allowed: readonly string[]): string | undefined {
  return Object.keys(table).find((key) => !allowed.includes(key));
}

function isAllowedCommand(argv: readonly string[]): boolean {
  const [program, subcommand, ...rest] = argv;
  if (program === "pnpm" || program === "npm") {
    return subcommand === "test" && rest.length === 0;
  }
  return (
    program === "cargo" &&
    subcommand === "test" &&
    rest.every((flag) => CARGO_TEST_FLAGS.includes(flag))
  );
}

function parseDir(value: unknown, where: string): string | { error: string } {
  if (typeof value !== "string" || (value !== "." && !DIR_PATTERN.test(value))) {
    return { error: `${where} must be a relative directory made of plain path segments` };
  }
  return value;
}

function parseArgv(value: unknown, where: string): string[] | { error: string } {
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_ARGV) {
    return { error: `${where} must be an array of 1 to ${MAX_ARGV} strings` };
  }
  const argv: string[] = [];
  for (const item of value as unknown[]) {
    if (typeof item !== "string" || item.length > MAX_ARG_LENGTH || !ARG_PATTERN.test(item)) {
      return { error: `${where} may hold only plain strings without shell characters` };
    }
    argv.push(item);
  }
  if (!isAllowedCommand(argv)) {
    return {
      error: `${where} must be pnpm test, npm test, or cargo test with only ${CARGO_TEST_FLAGS.join(" ")}`,
    };
  }
  return argv;
}

function parseInstall(
  value: unknown,
): { installCargo: boolean; installPnpm: string[] } | { error: string } {
  if (value === undefined) {
    return { installCargo: false, installPnpm: [] };
  }
  if (!isTable(value) || unknownKey(value, ["cargo", "pnpm"]) !== undefined) {
    return { error: "install must be a table with only the keys cargo and pnpm" };
  }
  const { cargo, pnpm } = value;
  if (cargo !== undefined && typeof cargo !== "boolean") {
    return { error: "install.cargo must be true or false" };
  }
  if (pnpm !== undefined && (!Array.isArray(pnpm) || pnpm.length > MAX_PNPM_DIRS)) {
    return { error: `install.pnpm must be an array of at most ${MAX_PNPM_DIRS} directories` };
  }
  const installPnpm: string[] = [];
  for (const entry of (pnpm ?? []) as unknown[]) {
    const dir = parseDir(entry, "install.pnpm");
    if (typeof dir !== "string") {
      return dir;
    }
    installPnpm.push(dir);
  }
  return { installCargo: cargo ?? false, installPnpm };
}

function parseTests(value: unknown): TestCommand[] | { error: string } {
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_TESTS) {
    return { error: `test must be 1 to ${MAX_TESTS} [[test]] tables` };
  }
  const tests: TestCommand[] = [];
  for (const entry of value as unknown[]) {
    if (!isTable(entry) || unknownKey(entry, ["dir", "argv"]) !== undefined) {
      return { error: "each [[test]] may hold only the keys dir and argv" };
    }
    const dir = parseDir(entry["dir"] ?? ".", "test.dir");
    if (typeof dir !== "string") {
      return dir;
    }
    const argv = parseArgv(entry["argv"], "test.argv");
    if (!Array.isArray(argv)) {
      return argv;
    }
    tests.push({ dir, argv });
  }
  return tests;
}

/**
 * Validates the text of a `tessel.toml`. Strict by design: unknown keys, oversized files, a
 * command that is not an argv array of plain strings, or a command outside `CARGO_TEST_FLAGS`
 * make the whole file invalid, because a gate that is half understood is not a gate.
 *
 * ```toml
 * instance = "standard-4"
 *
 * [install]
 * cargo = true
 * pnpm = ["tessel-steward"]
 *
 * [[test]]
 * argv = ["cargo", "test", "--workspace", "--locked", "--offline"]
 * ```
 */
export function parseGateConfig(text: string): ParsedGateConfig {
  if (new TextEncoder().encode(text).length > MAX_CONFIG_BYTES) {
    return { ok: false, error: `${TESSEL_TOML_PATH} is larger than ${MAX_CONFIG_BYTES} bytes` };
  }
  let document: unknown;
  try {
    document = parse(text);
  } catch {
    return { ok: false, error: `${TESSEL_TOML_PATH} is not valid TOML` };
  }
  if (!isTable(document) || unknownKey(document, ["instance", "install", "test"]) !== undefined) {
    return { ok: false, error: "only the keys instance, install and test are known" };
  }
  const instance = GATE_INSTANCES.find((name) => name === document["instance"]);
  if (instance === undefined) {
    return { ok: false, error: `instance must be one of ${GATE_INSTANCES.join(", ")}` };
  }
  const install = parseInstall(document["install"]);
  if ("error" in install) {
    return { ok: false, ...install };
  }
  const tests = parseTests(document["test"]);
  if (!Array.isArray(tests)) {
    return { ok: false, ...tests };
  }
  return { ok: true, config: { instance, ...install, tests } };
}
