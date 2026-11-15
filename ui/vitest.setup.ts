// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, configure } from "@testing-library/svelte";
import "@testing-library/jest-dom/vitest";
import process from "node:process";
import { afterAll, afterEach, expect } from "vitest";
import { startLagProbe } from "./src/lib/testing/lag-probe.ts";

// A stall of this worker's event loop over 1 s prints one line, so a timeout in a log shows
// whether the whole worker stalled (host load) or only one test was slow (code). Stopped
// after the file: the jsdom globals are gone by the time a late tick could run.
const stopLagProbe = startLagProbe({
  thresholdMs: 1000,
  intervalMs: 100,
  write: (line) => process.stderr.write(line),
  describe: () =>
    `worker ${process.env["VITEST_POOL_ID"] ?? "?"} ${expect.getState().testPath ?? "(no test running)"}`,
});
afterAll(stopLagProbe);

// findBy and waitFor return the moment their condition holds, so a long bound costs nothing on a
// pass; the default 1000 ms is wall clock too, and a starved worker outlasts it (see testTimeout
// in vite.config.ts).
configure({ asyncUtilTimeout: 30_000 });

// testing-library unmounts after each test only when the test API is global, which this suite
// doesn't use; without it a test's DOM stays in the body for the next test (or repeat) in the file.
afterEach(cleanup);

// The first render and the first role query in a test file build Svelte's and testing-library's
// tables and parse the stylesheet jsdom checks visibility against. That cost fell on whichever
// test ran first, where a loaded host turned it into a timeout; pay it here, before any test's
// clock starts.
{
  const { fireEvent, render, screen } = await import("@testing-library/svelte");
  const { default: Warmup } = await import("./src/lib/testing/Warmup.svelte");
  render(Warmup);
  await fireEvent.click(screen.getByRole("button", { name: "More" }));
  screen.getByRole("heading", { name: "Warm-up" });
  screen.getAllByRole("listitem");
  screen.getByLabelText("Text");
  cleanup();
}
