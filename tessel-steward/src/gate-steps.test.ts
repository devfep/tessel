import { describe, expect, it } from "vitest";

import type { Captured } from "./container-step";
import {
  BUDGET_EXHAUSTED_MESSAGE,
  parsePeakRss,
  installCommands,
  runWithinBudget,
  testCommands,
  type PlannedCommand,
} from "./gate-steps";
import { GATE_INSTANCE_MEMORY_BYTES, type GateConfig } from "./tessel-config";

const CONFIG: GateConfig = {
  instance: "standard-4",
  installCargo: true,
  installPnpm: ["tessel-steward", "web"],
  tests: [
    { dir: ".", argv: ["cargo", "test", "--workspace", "--locked", "--offline"] },
    { dir: "tessel-steward", argv: ["pnpm", "test"] },
  ],
};

function captured(exitCode: number, stdout = ""): Captured {
  return { exitCode, stdout, stderr: "", stdoutTruncated: false, stderrTruncated: false };
}

describe("installCommands", () => {
  it("installs offline, from the frozen lockfile, with scripts disabled, in each directory", () => {
    expect(installCommands(CONFIG)).toEqual([
      { argv: ["cargo", "fetch", "--locked", "--offline"], dir: "." },
      {
        argv: ["pnpm", "install", "--offline", "--frozen-lockfile", "--ignore-scripts"],
        dir: "tessel-steward",
      },
      {
        argv: ["pnpm", "install", "--offline", "--frozen-lockfile", "--ignore-scripts"],
        dir: "web",
      },
    ]);
  });

  it("installs nothing when the config declares nothing to install", () => {
    expect(installCommands({ ...CONFIG, installCargo: false, installPnpm: [] })).toEqual([]);
  });
});

describe("testCommands", () => {
  it("returns the trunk's commands in order", () => {
    expect(testCommands(CONFIG)).toEqual([
      { argv: ["cargo", "test", "--workspace", "--locked", "--offline"], dir: "." },
      { argv: ["pnpm", "test"], dir: "tessel-steward" },
    ]);
  });
});

describe("runWithinBudget", () => {
  const commands: PlannedCommand[] = [
    { argv: ["a"], dir: "." },
    { argv: ["b"], dir: "." },
    { argv: ["c"], dir: "." },
  ];

  it("gives each command what is left of the budget", async () => {
    let clock = 0;
    const seen: Array<[string, number]> = [];
    const result = await runWithinBudget(
      commands,
      100,
      async (command, timeout) => {
        seen.push([command.argv[0] ?? "", timeout]);
        clock += 30_000;
        return captured(0);
      },
      () => clock,
    );
    expect(seen).toEqual([
      ["a", 95],
      ["b", 65],
      ["c", 35],
    ]);
    expect(result.exitCode).toBe(0);
  });

  it("stops at the first failing command and returns its output", async () => {
    const ran: string[] = [];
    const result = await runWithinBudget(
      commands,
      100,
      async (command) => {
        ran.push(command.argv[0] ?? "");
        return command.argv[0] === "b" ? captured(3, "b failed") : captured(0);
      },
      () => 0,
    );
    expect(ran).toEqual(["a", "b"]);
    expect(result).toMatchObject({ exitCode: 3, stdout: "b failed" });
  });

  it("reports a timeout without running a command when the budget is used up", async () => {
    let clock = 0;
    const ran: string[] = [];
    const result = await runWithinBudget(
      commands,
      100,
      async (command) => {
        ran.push(command.argv[0] ?? "");
        clock += 100_000;
        return captured(0, "a done");
      },
      () => clock,
    );
    expect(ran).toEqual(["a"]);
    expect(result).toMatchObject({
      exitCode: 124,
      stdout: "a done",
      stderr: BUDGET_EXHAUSTED_MESSAGE,
    });
  });

  it("passes with no output when there is nothing to run", async () => {
    const result = await runWithinBudget(
      [],
      10,
      async () => captured(1),
      () => 0,
    );
    expect(result).toEqual(captured(0));
  });
});

const CAP = GATE_INSTANCE_MEMORY_BYTES["standard-4"];

describe("parsePeakRss", () => {
  it("clamps a value above the instance's memory, which only forged code could report", () => {
    expect(CAP).toBe(12 * 1024 ** 3);
    expect(parsePeakRss("999999999999\n", CAP)).toBe(CAP);
    expect(parsePeakRss("999999999999\n", GATE_INSTANCE_MEMORY_BYTES["standard-1"])).toBe(
      4 * 1024 ** 3,
    );
    expect(parsePeakRss("1024\n", CAP)).toBe(1024 * 1024);
  });

  it("returns the largest line in bytes and skips GNU time's other notes", () => {
    expect(parsePeakRss("2048\nCommand exited with non-zero status 1\n98304\n512\n", CAP)).toBe(
      98304 * 1024,
    );
  });

  it("returns null when no line is a plain number, since repo code can write the file", () => {
    for (const text of ["", "\n", "max\n", "-5\n", "1e9\n", "1 2\n", " 7\n", "9".repeat(13)]) {
      expect(parsePeakRss(text, CAP), text).toBeNull();
    }
  });
});
