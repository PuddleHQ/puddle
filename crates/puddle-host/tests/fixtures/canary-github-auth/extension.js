// SPDX-License-Identifier: GPL-3.0-or-later
// Replaces VS Code's `github` authentication provider (the built-in extension is switched off by the test) with one that
// hands out the canary token in PUDDLE_CANARY_TOKEN, to anything that asks: the git extension's askpass in a Remote-SSH
// guest asks through the window, and this is what the window answers. It runs on the Windows side (a "ui" extension).
// PUDDLE_CANARY_LOG names a file that gets one line per request, so the test can show the provider was asked.
const vscode = require("vscode");
const fs = require("node:fs");

// Temporary (debugging the CI run): when and with which options each ask came.
const trace = (line) => {
  const file = process.env.PUDDLE_CANARY_LOG;
  if (file && process.env.PUDDLE_CANARY_TRACE) fs.appendFileSync(`${file}.trace`, `${new Date().toISOString()} ${line}\n`);
};
const note = (line) => {
  const file = process.env.PUDDLE_CANARY_LOG;
  if (file) fs.appendFileSync(file, `${line}\n`);
  trace(line);
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
        getSessions: async (scopes, options) => {
          note(`getSessions ${(scopes ?? []).join(",")}`);
          trace(`options ${JSON.stringify(options ?? null)}`);
          const answer = [session(scopes ?? [])];
          trace(`answered ${JSON.stringify(answer.map((s) => s.scopes))}`);
          return answer;
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
