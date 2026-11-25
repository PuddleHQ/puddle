// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { type Attempt, navigateWithWatchdog } from "./navigation-watchdog.ts";

/** An attempt that nothing ever settles and whose response head never comes. */
function silent(): Attempt<string> {
  return {
    settled: new Promise(() => undefined),
    answered: Promise.resolve(false),
  };
}

function run(attempt: Attempt<string>) {
  let sent = 0;
  const result = navigateWithWatchdog<string>({
    start: () => {
      sent += 1;
      return attempt;
    },
    what: "opening /admin",
    evidence: () => Promise.resolve("a plain fetch answered 200"),
  });
  return { result, sent: () => sent };
}

describe("navigateWithWatchdog", () => {
  it("returns what an answered navigation returns, and sends it once", async () => {
    const { result, sent } = run({
      settled: Promise.resolve("loaded"),
      answered: Promise.resolve(true),
    });
    await expect(result).resolves.toBe("loaded");
    expect(sent()).toBe(1);
  });

  it("waits for a slow load once the response head came", async () => {
    let finish: (value: string) => void = () => undefined;
    const settled = new Promise<string>((done) => {
      finish = done;
    });
    const { result, sent } = run({ settled, answered: Promise.resolve(true) });
    await Promise.resolve();
    finish("late");
    await expect(result).resolves.toBe("late");
    expect(sent()).toBe(1);
  });

  it("fails by name, with the evidence, when the request gets no response, and does not send it again", async () => {
    const { result, sent } = run(silent());
    await expect(result).rejects.toThrow(
      "opening /admin got no response (a plain fetch answered 200)",
    );
    expect(sent()).toBe(1);
  });

  it("gives a navigation that fails by itself its own error, ", async () => {
    const { result, sent } = run({
      settled: Promise.reject(new Error("net::ERR_CONNECTION_REFUSED")),
      answered: new Promise<boolean>(() => undefined),
    });
    await expect(result).rejects.toThrow("net::ERR_CONNECTION_REFUSED");
    expect(sent()).toBe(1);
  });

  it("does not let the silent try's late abort surface", async () => {
    const aborted = new Promise<string>((_, fail) =>
      setTimeout(() => fail(new Error("page closed")), 5),
    );
    const { result } = run({
      settled: aborted,
      answered: Promise.resolve(false),
    });
    await expect(result).rejects.toThrow("got no response");
    await new Promise((done) => setTimeout(done, 20));
  });
});
