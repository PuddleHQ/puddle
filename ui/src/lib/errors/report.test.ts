// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it, vi } from "vitest";
import {
  DEFAULT_PAUSE_MS,
  ErrorReporter,
  MAX_PER_PAGE,
  reportUncaught,
  type ReporterDeps,
} from "./report.ts";

function harness(
  answer: { status: number; retryAfterS?: number } = { status: 204 },
) {
  let now = 1_000;
  const send = vi.fn<ReporterDeps["send"]>(async () => answer);
  const reporter = new ErrorReporter({
    send,
    route: () => "/workspaces/abc/terminal",
    build: "1700000000000",
    now: () => now,
  });
  return {
    reporter,
    send,
    advance: (ms: number) => {
      now += ms;
    },
  };
}

/** Lets the reporter's send settle. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

function thrown(message: string): Error {
  const error = new TypeError(message);
  error.stack = `TypeError: ${message}\n    at render (/_app/immutable/nodes/31.js:9:7650)`;
  return error;
}

describe("ErrorReporter", () => {
  it("sends what was thrown, where and in which build", async () => {
    const { reporter, send } = harness();
    reporter.report("page", thrown("read only"));
    await settle();
    expect(send).toHaveBeenCalledExactlyOnceWith({
      kind: "page",
      message: "TypeError: read only",
      stack: "  at render (/_app/immutable/nodes/31.js:9:7650)",
      route: "/workspaces/abc/terminal",
      build: "1700000000000",
    });
  });

  it("sends a thrown value that is no Error as text, with no stack", async () => {
    const { reporter, send } = harness();
    reporter.report("unhandled_rejection", "gone");
    await settle();
    expect(send.mock.calls[0]?.[0]).toMatchObject({
      message: "Thrown: gone",
      stack: null,
    });
  });

  it("sends the same error once per page, and no more than its fill", async () => {
    const { reporter, send } = harness();
    for (let i = 0; i < 3; i++) reporter.report("uncaught", thrown("loop"));
    // The same message caught elsewhere is another report.
    reporter.report("page", thrown("loop"));
    expect(send).toHaveBeenCalledTimes(2);
    for (let i = 0; i < MAX_PER_PAGE * 2; i++) {
      reporter.report("uncaught", thrown(`e${i}`));
    }
    expect(send).toHaveBeenCalledTimes(MAX_PER_PAGE);
  });

  it("holds back for as long as the server says when it has had too many", async () => {
    const { reporter, send, advance } = harness({
      status: 429,
      retryAfterS: 30,
    });
    reporter.report("uncaught", thrown("one"));
    await settle();
    reporter.report("uncaught", thrown("two"));
    expect(send).toHaveBeenCalledTimes(1);
    advance(30_000);
    reporter.report("uncaught", thrown("three"));
    expect(send).toHaveBeenCalledTimes(2);
  });

  it("holds back a minute when the server does not say how long", async () => {
    const { reporter, send, advance } = harness({ status: 429 });
    reporter.report("uncaught", thrown("one"));
    await settle();
    advance(DEFAULT_PAUSE_MS - 1);
    reporter.report("uncaught", thrown("two"));
    expect(send).toHaveBeenCalledTimes(1);
    advance(1);
    reporter.report("uncaught", thrown("three"));
    expect(send).toHaveBeenCalledTimes(2);
  });

  it("never throws when the server cannot be reached", async () => {
    const send = vi.fn<ReporterDeps["send"]>(async () => {
      throw new TypeError("Failed to fetch");
    });
    const reporter = new ErrorReporter({ send, route: () => "/", build: "b" });
    expect(() => reporter.report("uncaught", thrown("x"))).not.toThrow();
    await settle();
    expect(send).toHaveBeenCalledOnce();
  });
});

describe("reportUncaught", () => {
  it("reports an uncaught error and an unhandled rejection, and not a resource that failed to load", async () => {
    const { reporter, send } = harness();
    const target = new EventTarget();
    reportUncaught(target as never, reporter);
    const error = thrown("in a handler");
    target.dispatchEvent(
      new ErrorEvent("error", { error, message: error.message }),
    );
    const rejection = new Event("unhandledrejection") as Event & {
      reason: unknown;
    };
    rejection.reason = thrown("in a promise");
    target.dispatchEvent(rejection);
    // An <img> that failed: no error value, no message.
    target.dispatchEvent(new ErrorEvent("error"));
    await settle();
    expect(
      send.mock.calls.map(([report]) => [report.kind, report.message]),
    ).toEqual([
      ["uncaught", "TypeError: in a handler"],
      ["unhandled_rejection", "TypeError: in a promise"],
    ]);
  });

  it("reports a script error that carries only a message", async () => {
    const { reporter, send } = harness();
    const target = new EventTarget();
    reportUncaught(target as never, reporter);
    target.dispatchEvent(new ErrorEvent("error", { message: "Script error." }));
    await settle();
    expect(send.mock.calls[0]?.[0].message).toBe("Thrown: Script error.");
  });
});
