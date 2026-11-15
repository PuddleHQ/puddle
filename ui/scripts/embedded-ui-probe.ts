// SPDX-License-Identifier: AGPL-3.0-or-later
// Fails fast when the fixture backend serves no UI. The page is compiled into the binary and
// cargo caches puddle-api's build, so after `npm run build` the fixture keeps answering 404 on `/`
// until `crates/puddle-api/src/ui.rs` changes; every test then waits for a page that never loads.
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { OutputTail, waitForBackend } from "./backend-start.ts";

/** One line naming the cause and the way out. */
export const STALE_UI_MESSAGE =
  "the fixture serves no UI (GET / answered 404): ui/build is missing or puddle-api's cached build predates it; " +
  "run npm run build, then edit a comment line in crates/puddle-api/src/ui.rs";

/** Throws `STALE_UI_MESSAGE` when `/` of the backend at `url` answers 404. */
export async function assertServesUi(url: string): Promise<void> {
  const response = await fetch(url);
  await response.arrayBuffer();
  if (response.status === 404) throw new Error(STALE_UI_MESSAGE);
}

/**
 * Starts the backend `executable` (after `leadingArgs`) the way the tests do, checks that it
 * serves the UI, and stops it.
 */
export async function probeEmbeddedUi(
  executable: string,
  leadingArgs: string[] = [],
): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "puddle-ui-probe-"));
  const file = join(dir, "connection.json");
  const child = spawn(
    executable,
    [...leadingArgs, "--scenario", "default", "--connection-file", file],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
  const output = new OutputTail();
  for (const stream of [child.stdout, child.stderr]) {
    stream?.on("data", (chunk: Buffer) => output.push(chunk));
  }
  try {
    await waitForBackend(child, file, output, { executable });
    const { url } = JSON.parse(readFileSync(file, "utf8")) as { url: string };
    await assertServesUi(url);
  } finally {
    child.kill();
    rmSync(dir, { recursive: true, force: true });
  }
}
