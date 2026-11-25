// SPDX-License-Identifier: AGPL-3.0-or-later
// Reads the first screen of the installed program the way a person does, through UI Automation:
//
//   node welcome-window.mjs <pid of puddle-app.exe>
//
// Waits up to two minutes for a heading "Welcome to Puddle" in one of the program's windows. Run by
// ci/msi-install-test.ps1 after it opens Puddle from the Start menu shortcut, on a machine where the
// first-run walk has not been done. Prints one `ok` or `FAIL` line (and, on a failure, the window's
// names for the diagnosis); exit 0 when the heading is there, 1 otherwise. Plain Node, no packages.
import { Uia } from "./uia.mjs";

const pid = Number(process.argv[2]);
if (!Number.isInteger(pid) || pid <= 0) {
  console.log("FAIL usage: node welcome-window.mjs <pid>");
  process.exit(1);
}

const uia = await Uia.start(pid);
let code = 0;
try {
  const heading = await uia.waitFor('the "Welcome to Puddle" heading', { localized: "heading", name: "Welcome to Puddle" }, 120_000);
  console.log(`ok   the window shows the heading "${heading.name}"`);
} catch (err) {
  console.log(`FAIL ${err.message}`);
  for (const line of (await uia.dump().catch(() => [])).slice(0, 80)) console.log(`  ${line}`);
  code = 1;
} finally {
  uia.close();
}
process.exit(code);
