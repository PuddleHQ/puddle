// SPDX-License-Identifier: GPL-3.0-or-later
// The guest CPU count the settings accept. The API does not publish the limits, so they are
// written here; `crates/puddle-e2e/tests/ui_cpu_limits.rs` fails when they differ from the host's.

export const CPUS_MIN = 1;
export const CPUS_MAX = 32;

/** What a workspace has without any setting; the host's default count. */
export const CPUS_DEFAULT = 1;

/** The counts to choose from, lowest first. */
export const CPU_COUNTS: readonly number[] = Array.from(
  { length: CPUS_MAX - CPUS_MIN + 1 },
  (_, i) => CPUS_MIN + i,
);

export const cpuLabel = (count: number): string =>
  count === 1 ? "1 CPU" : `${count} CPUs`;

/** Why a change is not instant, for both pages. */
export const CPUS_NOTE =
  "A running workspace keeps the count it started with; a change applies at its next start.";
