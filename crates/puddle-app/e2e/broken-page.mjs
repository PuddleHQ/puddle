// SPDX-License-Identifier: AGPL-3.0-or-later
// Breaks the main window's page on purpose, over the DevTools port that usable.mjs opens with a WebView2
// policy (see terminal-render.mjs): every request the page makes is refused and the page reloads, so the
// window shows the browser's own error page, with none of Puddle's scripts running. The window's close,
// tray and quit handling lives in the shell, and this is how the installed-app run proves it does not
// need the page. Plain Node 24, no packages.
import { connect } from "./cdp.mjs";
import { until } from "./poll.mjs";

/**
 * The DevTools targets of type page that show a web page: `[{ url, webSocketDebuggerUrl }]`. WebView2 lists its
 * own browser UI (the clipboard permission prompt, `edge://permission-request-dialog/`) as a page too, and a
 * pending Ask prompt sorts first: breaking that target leaves Puddle's page loaded.
 */
async function pages(port) {
  const list = await (await fetch(`http://127.0.0.1:${port}/json`, { signal: AbortSignal.timeout(5000) })).json();
  return list.filter((t) => t.type === "page" && t.webSocketDebuggerUrl && /^https?:/.test(t.url));
}

/**
 * Refuses every request of the first page and reloads it. Resolves once the page's own document was
 * refused, with `{ refused, restore }`: `refused` is the list of refused document URLs, `restore()` lets
 * requests through again and reloads, then closes the session.
 */
export async function breakThePage(port) {
  const target = await until("a DevTools page target", async () => (await pages(port))[0], 30_000, 1000);
  const page = await connect(target.webSocketDebuggerUrl);
  const refused = [];
  page.on("Fetch.requestPaused", (p) => {
    if (p.resourceType === "Document") refused.push(p.request.url);
    page.send("Fetch.failRequest", { requestId: p.requestId, errorReason: "Failed" }).catch(() => {});
  });
  // A dialog the page raises on leaving (beforeunload) holds a reload until it is answered.
  page.on("Page.javascriptDialogOpening", () => page.send("Page.handleJavaScriptDialog", { accept: true }).catch(() => {}));
  await page.send("Page.enable");
  await page.send("Fetch.enable", { patterns: [{ urlPattern: "*", requestStage: "Request" }] });
  await page.send("Page.reload", { ignoreCache: true });
  // A reload that never reached the network (a page that is held, or a webview that did not act on it) is
  // asked again as a navigation to the page's own address.
  await until("the page's document to be refused", async () => refused.length > 0, 10_000, 500).catch(async () => {
    await page.send("Page.navigate", { url: target.url });
  });
  await until("the page's document to be refused", async () => refused.length > 0, 20_000, 500).catch(async (err) => {
    const seen = await pages(port).then((list) => list.map((t) => t.url).join(", "), String);
    page.close();
    throw new Error(`${err.message}; page targets: ${seen}; asked for: ${target.url}`);
  });
  return {
    refused,
    async restore() {
      await page.send("Fetch.disable").catch(() => {});
      await page.send("Page.reload", { ignoreCache: true }).catch(() => {});
      page.close();
    },
  };
}
