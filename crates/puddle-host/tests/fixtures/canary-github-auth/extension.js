// SPDX-License-Identifier: AGPL-3.0-or-later
// Replaces VS Code's `github` authentication provider (the built-in extension is switched off by the test) with one that
// hands out the canary token in PUDDLE_CANARY_TOKEN, to anything that asks: the git extension's askpass in a Remote-SSH
// guest asks through the window, and this is what the window answers. It runs on the Windows side (a "ui" extension).
// PUDDLE_CANARY_LOG names a file that gets one line per request, so the test can show the provider was asked.
const vscode = require("vscode");
const fs = require("node:fs");

const note = (line) => {
  const file = process.env.PUDDLE_CANARY_LOG;
  if (file) fs.appendFileSync(file, `${line}\n`);
};

exports.activate = (context) => {
  const token = process.env.PUDDLE_CANARY_TOKEN;
  if (!token) return;
  const changed = new vscode.EventEmitter();
  const session = (scopes) => ({
    id: "puddle-canary",
    accessToken: token,
    account: { id: "puddle-canary", label: "puddle-canary" },
    scopes: [...scopes],
  });
  context.subscriptions.push(
    changed,
    vscode.authentication.registerAuthenticationProvider(
      "github",
      "GitHub (canary)",
      {
        onDidChangeSessions: changed.event,
        getSessions: async (scopes) => {
          note(`getSessions ${(scopes ?? []).join(",")}`);
          return [session(scopes ?? [])];
        },
        createSession: async (scopes) => {
          note(`createSession ${scopes.join(",")}`);
          return session(scopes);
        },
        removeSession: async () => {},
      },
      { supportsMultipleAccounts: false },
    ),
  );
  note("registered");
};
