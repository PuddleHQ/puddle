// SPDX-License-Identifier: AGPL-3.0-or-later
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { probeEmbeddedUi, STALE_UI_MESSAGE } from "./embedded-ui-probe.ts";

const dirs: string[] = [];
afterEach(() => {
  for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true });
});

/** A stand-in backend: answers `/` with `status`, then announces itself as the fixture does. */
function standIn(status: number): string {
  const dir = mkdtempSync(join(tmpdir(), "ui-probe-test-"));
  dirs.push(dir);
  const script = join(dir, "backend.mjs");
  writeFileSync(
    script,
    `import { createServer } from "node:http";
import { writeFileSync } from "node:fs";
const file = process.argv[process.argv.indexOf("--connection-file") + 1];
const server = createServer((_, res) => { res.statusCode = ${status}; res.end("x"); });
server.listen(0, "127.0.0.1", () => {
  const url = "http://127.0.0.1:" + server.address().port + "/";
  console.log("listening");
  writeFileSync(file, JSON.stringify({ url, token: "t" }));
  writeFileSync(file + ".control", JSON.stringify({ url }));
});
`,
  );
  return script;
}

describe("probing the fixture's embedded UI", () => {
  it("passes when / is served", async () => {
    await expect(
      probeEmbeddedUi(process.execPath, [standIn(200)]),
    ).resolves.toBeUndefined();
  });

  it("fails with one line naming the cause when / answers 404", async () => {
    const error = await probeEmbeddedUi(process.execPath, [standIn(404)]).then(
      () => undefined,
      (e: Error) => e,
    );
    expect(error?.message).toBe(STALE_UI_MESSAGE);
    expect(error?.message).not.toContain("\n");
    expect(error?.message).toContain("crates/puddle-api/src/ui.rs");
  });
});
