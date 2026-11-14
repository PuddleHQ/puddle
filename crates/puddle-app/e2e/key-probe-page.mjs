// SPDX-License-Identifier: AGPL-3.0-or-later
// Serves the terminal key probe: `terminal-keys.html`, and the terminal's own key rule,
// `ui/src/lib/terminal/keys.ts` with its types stripped, as `/keys.js`. The probe decides what to
// cancel with the same `keyAction` the terminal screen calls, so a change to the rule changes what
// the WebView2 and WebKitGTK scripts measure, and the page has no second copy to drift.
import { readFile } from "node:fs/promises";
import { stripTypeScriptTypes } from "node:module";

const html = await readFile(new URL("./terminal-keys.html", import.meta.url));
const keys = stripTypeScriptTypes(await readFile(new URL("../../../ui/src/lib/terminal/keys.ts", import.meta.url), "utf8"));

/** The request handler for `http.createServer`. */
export function probeHandler(_req, res) {
  if (_req.url === "/keys.js") {
    res.writeHead(200, { "Content-Type": "text/javascript; charset=utf-8" });
    res.end(keys);
    return;
  }
  res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
  res.end(html);
}
