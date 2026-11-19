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

function run(attempts: Attempt<string>[], tries?: number) {
  const warnings: string[] = [];
  let sent = 0;
  const result = navigateWithWatchdog<string>({
    start: () => {
      const next = attempts[sent];
      sent += 1;
      if (next === undefined) throw new Error("sent more often than planned");
      return next;
    },
    what: "opening /admin",
    evidence: () => Promise.resolve("a plain fetch answered 200"),
    warn: (line) => warnings.push(line),
    ...(tries === undefined ? {} : { tries }),
  });
  return { result, warnings, sent: () => sent };
}

describe("navigateWithWatchdog", () => {
  it("returns what an answered navigation returns, and sends it once", async () => {
    const { result, warnings, sent } = run([
      { settled: Promise.resolve("loaded"), answered: Promise.resolve(true) },
    ]);
    await expect(result).resolves.toBe("loaded");
    expect(sent()).toBe(1);
    expect(warnings).toEqual([]);
  });

  it("waits for a slow load once the response head came", async () => {
    let finish: (value: string) => void = () => undefined;
    const settled = new Promise<string>((done) => {
      finish = done;
    });
    const { result, sent } = run([
      { settled, answered: Promise.resolve(true) },
    ]);
    await Promise.resolve();
    finish("late");
    await expect(result).resolves.toBe("late");
    expect(sent()).toBe(1);
  });

  it("sends a navigation again that got no response, and says so", async () => {
    const { result, warnings, sent } = run([
      silent(),
      { settled: Promise.resolve("loaded"), answered: Promise.resolve(true) },
    ]);
    await expect(result).resolves.toBe("loaded");
    expect(sent()).toBe(2);
    expect(warnings).toEqual([
      "opening /admin got no response, sending it again (try 2 of 2; a plain fetch answered 200)",
    ]);
  });

  it("fails by name, with the evidence, when every try is silent", async () => {
    const { result, sent } = run([silent(), silent()]);
    await expect(result).rejects.toThrow(
      "opening /admin got no response in 2 tries (a plain fetch answered 200)",
    );
    expect(sent()).toBe(2);
  });

  it("sends as often as it is told to", async () => {
    const { result, sent } = run(
      [
        silent(),
        silent(),
        { settled: Promise.resolve("loaded"), answered: Promise.resolve(true) },
      ],
      3,
    );
    await expect(result).resolves.toBe("loaded");
    expect(sent()).toBe(3);
  });

  it("gives a navigation that fails by itself its own error, without a second send", async () => {
    const { result, sent } = run([
      {
        settled: Promise.reject(new Error("net::ERR_CONNECTION_REFUSED")),
        answered: new Promise<boolean>(() => undefined),
      },
    ]);
    await expect(result).rejects.toThrow("net::ERR_CONNECTION_REFUSED");
    expect(sent()).toBe(1);
  });

  it("does not let the replaced try's late abort surface", async () => {
    const aborted = new Promise<string>((_, fail) =>
      setTimeout(() => fail(new Error("interrupted by another navigation")), 5),
    );
    const { result } = run([
      { settled: aborted, answered: Promise.resolve(false) },
      { settled: Promise.resolve("loaded"), answered: Promise.resolve(true) },
    ]);
    await expect(result).resolves.toBe("loaded");
    await new Promise((done) => setTimeout(done, 20));
  });
});
