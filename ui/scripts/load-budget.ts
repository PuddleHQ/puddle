// SPDX-License-Identifier: GPL-3.0-or-later
// How much longer a UI test may take because the machine is busy. Chromium and the test runner
// share their cores with whatever else runs; at a load of 15 times the core count a one-second
// settings test took 15 to 30 s, and a test of 20 steps went over its 30 s with nothing wrong
// with the page. The test's own limit grows by the factor the load calls for, read when the test
// starts; an idle machine keeps the plain limit.

/** The most a test's limit grows: past this a hang is a hang, however busy the host. */
export const MAX_LOAD_FACTOR = 4;

/**
 * The factor for a one-minute load average on `cpus` logical cores: 1 up to one runnable task per
 * core, then one more for each further core's worth of load, at most `MAX_LOAD_FACTOR`. A reading
 * that is not a usable number (a platform without load averages reports 0) gives 1.
 */
export function loadFactor(load: number, cpus: number): number {
  if (!Number.isFinite(load) || !(cpus > 0)) return 1;
  return Math.min(MAX_LOAD_FACTOR, Math.max(1, Math.ceil(load / cpus)));
}
