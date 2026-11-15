// SPDX-License-Identifier: AGPL-3.0-or-later
// The guest CPU count the settings accept. The host publishes the upper bound (the global maximum,
// `effective_limits.max_cpus`), so only the smallest count and the default are written here;
// `crates/puddle-e2e/tests/integration/ui_cpu_limits.rs` fails when they differ from the host's.

export const CPUS_MIN = 1;

/** The host's default on a computer with eight or more logical cores; a smaller one gets half its cores. */
export const CPUS_DEFAULT = 4;

/** The counts to choose from, lowest first: 1 up to `max`, and `keep` when it is above that. */
export function cpuCounts(max: number, keep: number | null = null): number[] {
  const top = Math.max(CPUS_MIN, max);
  const counts = Array.from(
    { length: top - CPUS_MIN + 1 },
    (_, i) => CPUS_MIN + i,
  );
  return keep !== null && keep > top ? [...counts, keep] : counts;
}

export const cpuLabel = (count: number): string =>
  count === 1 ? "1 CPU" : `${count} CPUs`;

/** Why a change is not instant, for both pages. */
export const CPUS_NOTE =
  "A running workspace keeps the count it started with; a change applies at its next start.";
