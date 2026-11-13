// SPDX-License-Identifier: AGPL-3.0-or-later
// Waiting for a spawned fixture backend to be ready, and saying why when it is not. Kept apart
// from e2e/fixture.ts so a vitest test can drive it with fake children.
import { type ChildProcess } from "node:child_process";
import { existsSync, readFileSync, statSync } from "node:fs";
import { loadavg } from "node:os";

/** How long a backend may take to write its connection files, a loaded host included. */
export const START_TIMEOUT_MS = 90_000;

const TAIL_BYTES = 4096;

/** The last bytes the child printed on stdout and stderr, kept for the failure message. */
export class OutputTail {
  private text = "";
  push(chunk: Buffer | string): void {
    this.text = (this.text + chunk.toString()).slice(-TAIL_BYTES);
  }
  toString(): string {
    return this.text.trim();
  }
}

/** How long after the process exits to wait for its stdout and stderr to finish draining. */
const DRAIN_GRACE_MS = 2000;

/** The parsed JSON of a file, or `undefined` while it is missing. */
function readJson(file: string): { url?: string } | undefined {
  try {
    return JSON.parse(readFileSync(file, "utf8")) as { url?: string };
  } catch {
    return undefined;
  }
}

/** The facts a stalled start needs to be read against: the executable and the host's load. */
function hostFacts(executable: string | undefined): string {
  const load = loadavg()
    .map((n) => n.toFixed(1))
    .join(" ");
  const binary =
    executable === undefined
      ? "unknown"
      : existsSync(executable)
        ? `${executable} (${statSync(executable).size} bytes)`
        : `${executable} (missing)`;
  return `executable: ${binary}; host load: ${load}`;
}

export interface StartOptions {
  timeoutMs?: number;
  pollMs?: number;
  /** Named in the failure message. */
  executable?: string;
}

/**
 * Resolves once the backend wrote `<file>.control`, which it does last and renames into place
 * whole, so a file that exists parses.
 * Rejects at once if the process ends first, and after `timeoutMs` otherwise. The error says
 * which, how long it waited, the URLs the backend had announced, and what it printed.
 */
export async function waitForBackend(
  child: ChildProcess,
  file: string,
  output: OutputTail,
  { timeoutMs = START_TIMEOUT_MS, pollMs = 50, executable }: StartOptions = {},
): Promise<void> {
  let ended: string | undefined;
  let endedAt = 0;
  // `exit` can fire before the last stdout and stderr chunks are read, so the message would
  // miss what the backend printed; `close` fires once they are drained.
  let drained = false;
  child.once("exit", (code, signal) => {
    ended = signal === null ? `exit code ${code}` : `signal ${signal}`;
    endedAt = Date.now();
  });
  child.once("close", () => {
    drained = true;
  });
  child.once("error", (error) => {
    ended = `spawn error: ${error.message}`;
    endedAt = Date.now();
    drained = true;
  });
  const began = Date.now();
  for (;;) {
    if (readJson(`${file}.control`) !== undefined) return;
    const elapsed = Date.now() - began;
    // A grandchild holding the pipes open would delay `close`, so the wait for it is bounded.
    const over =
      ended !== undefined &&
      (drained || Date.now() - endedAt >= DRAIN_GRACE_MS);
    if (over || (ended === undefined && elapsed >= timeoutMs)) {
      const api = readJson(file)?.url ?? "not written";
      const control = readJson(`${file}.control`)?.url ?? "not written";
      const reason =
        ended === undefined
          ? `still starting after ${(elapsed / 1000).toFixed(1)} s (limit ${timeoutMs / 1000} s)`
          : `the process ended (${ended}) after ${(elapsed / 1000).toFixed(1)} s`;
      throw new Error(
        [
          `the fixture backend did not start: ${reason}`,
          `API url: ${api}; control url: ${control}; connection file: ${file}`,
          hostFacts(executable),
          `backend output (last ${TAIL_BYTES} bytes):`,
          output.toString() || "(nothing)",
        ].join("\n"),
      );
    }
    await new Promise((resolve) => setTimeout(resolve, pollMs));
  }
}
