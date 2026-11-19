// SPDX-License-Identifier: AGPL-3.0-or-later
import { type ChildProcess, spawn } from "node:child_process";
import { EventEmitter } from "node:events";
import { PassThrough } from "node:stream";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { OutputTail, waitForBackend } from "./backend-start.ts";

const children: ChildProcess[] = [];
const dirs: string[] = [];
afterEach(() => {
  for (const child of children.splice(0)) child.kill();
  for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true });
});

/** A stand-in backend: a node script, with its output captured as fixture.ts does. */
function start(script: string): {
  file: string;
  output: OutputTail;
  child: ChildProcess;
} {
  const dir = mkdtempSync(join(tmpdir(), "backend-start-"));
  dirs.push(dir);
  const file = join(dir, "connection.json");
  const child = spawn(process.execPath, ["-e", script, file], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  children.push(child);
  const output = new OutputTail();
  for (const stream of [child.stdout, child.stderr]) {
    stream?.on("data", (chunk: Buffer) => output.push(chunk));
  }
  return { file, output, child };
}

/**
 * A backend that is alive and silent, without a process behind it. Starting a real node takes the
 * operating system 50 ms on an idle machine and 2.6 s on a loaded host (measured: the child's own
 * clock said 82 ms from its start to its write, 2550 ms after spawn() returned), so a test that is
 * about the wait must not wait on that. Only the test that needs a real process spawns one.
 */
function silentBackend(): {
  file: string;
  output: OutputTail;
  child: ChildProcess;
} {
  const dir = mkdtempSync(join(tmpdir(), "backend-start-"));
  dirs.push(dir);
  return {
    file: join(dir, "connection.json"),
    output: new OutputTail(),
    child: new EventEmitter() as unknown as ChildProcess,
  };
}

describe("waiting for the fixture backend", () => {
  it("returns once the control file is there", async () => {
    const { file, output, child } = silentBackend();
    const waiting = waitForBackend(child, file, output, { pollMs: 5 });
    await new Promise((resolve) => setTimeout(resolve, 30));
    writeFileSync(`${file}.control`, '{"url":"http://127.0.0.1:1"}');
    await expect(waiting).resolves.toBeUndefined();
  });

  // A real child, so the exit code and the stderr pipe are the operating system's. The bound is
  // explicit because the test waits for a process to start, which a loaded host can take seconds for.
  it(
    "names a backend that ended, with its exit code and what it printed",
    { timeout: 30_000 },
    async () => {
      const { file, output, child } = start(
        `console.error("puddle-ui-fixture: cannot bind 127.0.0.1:9"); process.exit(2)`,
      );
      const failure = await waitForBackend(child, file, output, {
        timeoutMs: 30_000,
      }).catch((error: Error) => error);
      expect(failure).toBeInstanceOf(Error);
      const message = (failure as Error).message;
      expect(message).toContain("the process ended (exit code 2)");
      expect(message).toContain("cannot bind 127.0.0.1:9");
      expect(message).toContain("control url: not written");
      expect(message).toContain("host load:");
    },
  );

  it("keeps output that arrives after the process exit event", async () => {
    const child = Object.assign(new EventEmitter(), {
      stderr: new PassThrough(),
    }) as unknown as ChildProcess;
    const output = new OutputTail();
    (child.stderr as PassThrough).on("data", (chunk: Buffer) =>
      output.push(chunk),
    );
    const waiting = waitForBackend(
      child,
      "/nonexistent/connection.json",
      output,
      {
        pollMs: 5,
      },
    ).catch((error: Error) => error);
    child.emit("exit", 2, null);
    await new Promise((resolve) => setTimeout(resolve, 30));
    (child.stderr as PassThrough).write("late words");
    await new Promise((resolve) => setTimeout(resolve, 30));
    child.emit("close", 2, null);
    expect(((await waiting) as Error).message).toContain("late words");
  });

  it("names a backend that is still starting at the limit, with the URL it announced", async () => {
    const { file, output, child } = silentBackend();
    writeFileSync(file, '{"url":"http://127.0.0.1:4242","token":"t"}');
    output.push("binding\n");
    const failure = await waitForBackend(child, file, output, {
      timeoutMs: 600,
      pollMs: 20,
      executable: process.execPath,
    }).catch((error: Error) => error);
    const message = (failure as Error).message;
    expect(message).toContain("still starting after");
    expect(message).toContain("limit 0.6 s");
    expect(message).toContain("API url: http://127.0.0.1:4242");
    expect(message).toContain("control url: not written");
    expect(message).toContain("binding");
    expect(message).toContain(process.execPath);
  });

  it("names an executable that could not be spawned", async () => {
    const dir = mkdtempSync(join(tmpdir(), "backend-start-"));
    dirs.push(dir);
    const missing = join(dir, "no-such-backend");
    const child = spawn(missing, [], { stdio: ["ignore", "pipe", "pipe"] });
    children.push(child);
    const failure = await waitForBackend(
      child,
      join(dir, "connection.json"),
      new OutputTail(),
      { timeoutMs: 30_000, executable: missing },
    ).catch((error: Error) => error);
    expect(failure).toBeInstanceOf(Error);
    const message = (failure as Error).message;
    expect(message).toContain("the process ended (spawn error:");
    expect(message).toContain("ENOENT");
    expect(message).toContain(`${missing} (missing)`);
  });

  it("does not take a half-written control file for a started backend", async () => {
    const { file, output, child } = silentBackend();
    writeFileSync(`${file}.control`, '{"url":');
    const failure = await waitForBackend(child, file, output, {
      timeoutMs: 300,
      pollMs: 20,
    }).catch((error: Error) => error);
    expect((failure as Error).message).toContain("still starting after");
  });

  it("keeps only the end of a long output", () => {
    const tail = new OutputTail();
    tail.push("a".repeat(10_000));
    tail.push("the end");
    expect(tail.toString().length).toBeLessThanOrEqual(4096);
    expect(tail.toString().endsWith("the end")).toBe(true);
  });
});
