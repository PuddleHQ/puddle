// SPDX-License-Identifier: GPL-3.0-or-later
import { resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { runCli } from "./cli.ts";

describe("runCli", () => {
  const argv = process.argv;
  const exitCode = process.exitCode;
  const script = resolve("some", "gate.ts");
  let err: string[];

  beforeEach(() => {
    process.exitCode = undefined;
    err = [];
    vi.spyOn(console, "error").mockImplementation(
      (m: string) => void err.push(m),
    );
  });
  afterEach(() => {
    process.argv = argv;
    process.exitCode = exitCode;
    vi.restoreAllMocks();
  });

  it("sets the exit code to what the function returns when the script is the one node started", () => {
    process.argv = ["node", script];
    runCli(script, () => 1);
    expect(process.exitCode).toBe(1);
  });

  it("hands the function the arguments after the script name", () => {
    process.argv = ["node", script, "--check", "x"];
    const fn = vi.fn(() => 0);
    runCli(script, fn);
    expect(fn).toHaveBeenCalledWith(["--check", "x"]);
  });

  it("does nothing when another script was started", () => {
    process.argv = ["node", resolve("other.ts")];
    const fn = vi.fn(() => 1);
    runCli(script, fn);
    expect(fn).not.toHaveBeenCalled();
    expect(process.exitCode).toBeUndefined();
  });

  it("does nothing when node was started without a script", () => {
    process.argv = ["node"];
    const fn = vi.fn(() => 1);
    runCli(script, fn);
    expect(fn).not.toHaveBeenCalled();
    expect(process.exitCode).toBeUndefined();
  });

  it("lets an error propagate when no name is given", () => {
    process.argv = ["node", script];
    expect(() =>
      runCli(script, () => {
        throw new Error("boom");
      }),
    ).toThrow("boom");
    expect(err).toEqual([]);
  });

  it("prints '<name>: <message>' and exits 2 on an error when a name is given", () => {
    process.argv = ["node", script];
    runCli(
      script,
      () => {
        throw new Error("boom");
      },
      "gate",
    );
    expect(err).toEqual(["gate: boom"]);
    expect(process.exitCode).toBe(2);
  });

  it("keeps the returned exit code when a named function succeeds", () => {
    process.argv = ["node", script];
    runCli(script, () => 0, "gate");
    expect(process.exitCode).toBe(0);
    expect(err).toEqual([]);
  });
});
