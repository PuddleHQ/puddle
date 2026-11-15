// SPDX-License-Identifier: AGPL-3.0-or-later
import type { ChildProcess } from "node:child_process";

export const DRAIN_GRACE_MS: number;
export function onChildEnd(
  child: ChildProcess,
  done: (code: number | null, signal: NodeJS.Signals | null) => void,
  graceMs?: number,
): void;
