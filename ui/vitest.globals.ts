// SPDX-License-Identifier: AGPL-3.0-or-later
// Setup for every test file: what the jsdom context lacks.
import * as perfHooks from "node:perf_hooks";
import * as webStreams from "node:stream/web";
import { vi } from "vitest";

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

// A host stall freezes every worker at once (the lag probe in vitest.setup.ts printed 34-37 s for
// all of them in one run), and whichever test is running then outlasts its 5 s clock though it
// needs 40 ms. The limits stay as they are; a test that timed out gets one more attempt, so a
// stall that already ended does not fail the run and a test that really hangs still fails twice.
// Only a timeout retries: a failed assertion fails at once. It is set here and not in the config
// because a RegExp condition does not survive the config's trip to the workers.
vi.setConfig({ retry: { count: 1, condition: /timed out/i } });
