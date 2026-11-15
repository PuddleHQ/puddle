// SPDX-License-Identifier: AGPL-3.0-or-later
import type { ChildProcess } from "node:child_process";
import { EventEmitter } from "node:events";
import { PassThrough } from "node:stream";
import { describe, expect, it } from "vitest";
import { onChildEnd } from "../../crates/puddle-app/e2e/child-end.mjs";

function fakeChild() {
  const stderr = new PassThrough();
  const child = Object.assign(new EventEmitter(), {
    stderr,
  }) as unknown as ChildProcess;
  return { child, stderr };
}

const pause = (ms: number) => new Promise((r) => setTimeout(r, ms));

describe("onChildEnd", () => {
  it("waits for close, so output after exit is read first", async () => {
    const { child, stderr } = fakeChild();
    let log = "";
    stderr.on("data", (c: Buffer) => (log += c));
    let seen: string | undefined;
    onChildEnd(child, () => (seen = log), 5000);
    child.emit("exit", 2, null);
    await pause(20);
    expect(seen).toBeUndefined();
    stderr.write("late words");
    await pause(20);
    child.emit("close", 2, null);
    expect(seen).toBe("late words");
  });

  it("gives up waiting after the grace period when a grandchild holds the pipes", async () => {
    const { child } = fakeChild();
    const calls: unknown[] = [];
    onChildEnd(child, (code) => calls.push(code), 30);
    child.emit("exit", 3, null);
    await pause(80);
    expect(calls).toEqual([3]);
    child.emit("close", 3, null);
    expect(calls).toEqual([3]);
  });

  it("reports a spawn error once", () => {
    const { child } = fakeChild();
    const calls: unknown[] = [];
    onChildEnd(child, (code) => calls.push(code), 5000);
    child.emit("error", new Error("ENOENT"));
    child.emit("close", null, null);
    expect(calls).toEqual([null]);
  });
});
