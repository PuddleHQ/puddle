// SPDX-License-Identifier: AGPL-3.0-or-later
// The three interception modes in plain words: one name and one warning each, shared by the
// workspace card, the global setting and the first-run step so they never disagree.
import type { components } from "#lib/api/schema.d.ts";

export type Mode = components["schemas"]["InterceptionMode"];

/** In the order the choice is offered. */
export const MODES: readonly Mode[] = [
  "all",
  "all_except_curated",
  "known_only",
];

/** What the mode is called in a list. */
export const MODE_NAMES: Record<Mode, string> = {
  all: "Decrypt every host",
  all_except_curated: "Decrypt every host except a known list",
  known_only: "Decrypt only hosts Puddle has a reason to read",
};

/** What the mode costs: shown wherever the mode is chosen or in force. */
export const MODE_WARNINGS: Record<Mode, string> = {
  all:
    "Rules on method and path work on every host. A tool with its own " +
    "trust store (a Java program, say) fails on first contact with a host " +
    "until Puddle learns it.",
  all_except_curated:
    "Rules on method and path work on every host except a list of hosts " +
    "known to break, which Puddle passes through. A tool that still " +
    "doesn't trust Puddle's certificate fails once for that host.",
  known_only:
    "This limits the proxy: a rule on any host Puddle has no reason to " +
    "read can only allow or deny the whole host.",
};

/** What the first-run step preselects and skipping the setup takes. */
export const DEFAULT_MODE: Mode = "all_except_curated";

/** What "learn from rejections" does, said next to its switch. */
export const LEARNING_NOTE =
  "When a tool refuses Puddle's certificate for a host, Puddle passes that host " +
  "through for 12 hours and puts it on the learned list of the workspace, where " +
  "you can make it permanent or try decrypting it again.";
