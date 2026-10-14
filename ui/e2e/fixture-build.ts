// SPDX-License-Identifier: GPL-3.0-or-later
// Playwright's global setup: builds the UI fixture backend once for the whole run and hands the
// executable's path to the workers through the environment (`PUDDLE_E2E_FIXTURE_BIN`).
//
// Each worker used to run `cargo build` itself. Every one of them takes cargo's build-directory
// lock and checks the whole workspace for changes, so on a busy host the 20 workers queued behind
// each other: that step took 2 s alone and up to 15.5 s at load 77, and the worker fixture's
// 30 s limit was then one slow lock holder away. Now the workers only start the binary.
import { buildFixture } from "../scripts/fixture-binary.ts";

export const FIXTURE_BIN_ENV = "PUDDLE_E2E_FIXTURE_BIN";

export default function buildFixtureOnce(): void {
  process.env[FIXTURE_BIN_ENV] = buildFixture();
}
