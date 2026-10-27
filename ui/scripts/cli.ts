// SPDX-License-Identifier: GPL-3.0-or-later
// The entry guard the gate scripts share: run `fn` only when the script is the one node was
// started with, not when a test or another script imports it.
import { resolve } from "node:path";

/**
 * Runs `fn` and stores its result as the process exit code, when `metaFilename` (the caller's
 * `import.meta.filename`) is the script node was started with. `fn` gets the command-line
 * arguments after the script name. With a `name`, an `Error` thrown by
 * `fn` is printed as `<name>: <message>` and the exit code is 2; without one it propagates.
 */
export function runCli(
  metaFilename: string,
  fn: (argv: string[]) => number,
  name?: string,
): void {
  const argv = process.argv.slice(2);
  if (!process.argv[1] || resolve(process.argv[1]) !== metaFilename) return;
  if (name === undefined) {
    process.exitCode = fn(argv);
    return;
  }
  try {
    process.exitCode = fn(argv);
  } catch (e) {
    console.error(`${name}: ${(e as Error).message}`);
    process.exitCode = 2;
  }
}
