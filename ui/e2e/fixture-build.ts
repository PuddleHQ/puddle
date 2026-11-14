// SPDX-License-Identifier: AGPL-3.0-or-later
// Playwright's global setup: builds the UI fixture backend once for the whole run and hands the
// executable's path to the workers through the environment (`PUDDLE_E2E_FIXTURE_BIN`).
//
// Each worker used to run `cargo build` itself. Every one of them takes cargo's build-directory
// lock and checks the whole workspace for changes, so on a busy host the 20 workers queued behind
// each other: that step took 2 s alone and up to 15.5 s at load 77, and the worker fixture's
// 30 s limit was then one slow lock holder away. Now the workers only start the binary.
import { probeEmbeddedUi } from "../scripts/embedded-ui-probe.ts";
import { buildFixture } from "../scripts/fixture-binary.ts";

export const FIXTURE_BIN_ENV = "PUDDLE_E2E_FIXTURE_BIN";

export default async function buildFixtureOnce(): Promise<void> {
  const executable = buildFixture();
  // A stale embedded UI would make every test wait for a page that never loads.
  await probeEmbeddedUi(executable);
  process.env[FIXTURE_BIN_ENV] = executable;
}
