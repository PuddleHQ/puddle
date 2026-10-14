// SPDX-License-Identifier: GPL-3.0-or-later
import * as perfHooks from "node:perf_hooks";
import * as webStreams from "node:stream/web";
import "@testing-library/jest-dom/vitest";

// Vitest's vm pools run each test file in a bare jsdom context, which lacks the Node globals the
// other pools leave in place: the web streams (the event stream reader pipes a response body
// through `TextDecoderStream`) and the performance API (the app marks its first paints with
// `performance.mark`). Node's own implementations are what the app's code meets in every other pool.
const nodeGlobals: Record<string, unknown> = {
  ...webStreams,
  PerformanceEntry: perfHooks.PerformanceEntry,
  PerformanceMark: perfHooks.PerformanceMark,
  PerformanceMeasure: perfHooks.PerformanceMeasure,
  PerformanceObserver: perfHooks.PerformanceObserver,
  PerformanceObserverEntryList: perfHooks.PerformanceObserverEntryList,
  PerformanceResourceTiming: perfHooks.PerformanceResourceTiming,
};
for (const [name, value] of Object.entries(nodeGlobals)) {
  if (!(name in globalThis)) {
    Object.defineProperty(globalThis, name, {
      value,
      configurable: true,
      writable: true,
    });
  }
}
if (typeof globalThis.performance?.mark !== "function") {
  Object.defineProperty(globalThis, "performance", {
    value: perfHooks.performance,
    configurable: true,
    writable: true,
  });
}

// jsdom has no ResizeObserver (Svelte's `bind:clientHeight` needs one); nothing is ever
// resized there, so one that never reports is enough.
if (typeof globalThis.ResizeObserver === "undefined") {
  globalThis.ResizeObserver = class {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  };
}

// The first render and the first role query in a test file build Svelte's and testing-library's
// tables and parse the stylesheet jsdom checks visibility against. That cost fell on whichever
// test ran first, where a loaded host turned it into a timeout; pay it here, before any test's
// clock starts.
{
  const { cleanup, fireEvent, render, screen } =
    await import("@testing-library/svelte");
  const { default: Warmup } = await import("./src/lib/testing/Warmup.svelte");
  render(Warmup);
  await fireEvent.click(screen.getByRole("button", { name: "More" }));
  screen.getByRole("heading", { name: "Warm-up" });
  screen.getAllByRole("listitem");
  screen.getByLabelText("Text");
  cleanup();
}
