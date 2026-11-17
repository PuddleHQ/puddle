// SPDX-License-Identifier: AGPL-3.0-or-later
// Test extension for crates/puddle-app/e2e/microsoft-editor.mjs: on every start of the extension host it reads a
// made-up "session" from the editor's secret storage; when there is none it stores one. Each start appends one line
// to /tmp/canary-secret.log in the guest (`stored <id>` or `found <id>`), which the test reads from the guest.
const vscode = require("vscode");
const fs = require("node:fs");
const path = require("node:path");

const KEY = "puddle-test.canary-session";

async function activate(context) {
  const folders = vscode.workspace.workspaceFolders ?? [];
  const log = path.join("/tmp", "canary-secret.log");
  const note = (text) => {
    try {
      fs.appendFileSync(log, `${new Date().toISOString()} ${text}\n`);
    } catch (err) {
      console.error(`canary-secret: ${err}`);
    }
  };
  try {
    const kept = await context.secrets.get(KEY);
    if (kept) {
      note(`found ${JSON.parse(kept).id} folders=${folders.length}`);
    } else {
      const session = { id: `canary-${Date.now().toString(36)}`, account: "canary@example.org" };
      await context.secrets.store(KEY, JSON.stringify(session));
      note(`stored ${session.id} folders=${folders.length}`);
    }
  } catch (err) {
    note(`error ${err}`);
  }
}

module.exports = { activate };
