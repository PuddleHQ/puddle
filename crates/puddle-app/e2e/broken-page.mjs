// SPDX-License-Identifier: AGPL-3.0-or-later
// Breaks the main window's page on purpose, over the DevTools port that usable.mjs opens with a WebView2
// policy (see terminal-render.mjs): every request the page makes is refused and the page reloads, so the
// window shows the browser's own error page, with none of Puddle's scripts running. The window's close,
// tray and quit handling lives in the shell, and this is how the installed-app run proves it does not
// need the page. Plain Node 24, no packages.
import { connect } from "./cdp.mjs";
import { until } from "./poll.mjs";

/** The DevTools targets of type page: `[{ url, webSocketDebuggerUrl }]`. */
async function pages(port) {
  const list = await (await fetch(`http://127.0.0.1:${port}/json`, { signal: AbortSignal.timeout(5000) })).json();
  return list.filter((t) => t.type === "page" && t.webSocketDebuggerUrl);
}

/** Whether a page URL is the browser's own error page (what a document that failed to load shows). */
export const isErrorPage = (url) => String(url).startsWith("chrome-error:");

/**
 * Refuses every request of the first page and reloads it. Resolves once the page shows the error page,
 * with `{ url, restore }`: `restore()` lets requests through again and reloads, then closes the session.
 */
export async function breakThePage(port) {
  const target = await until("a DevTools page target", async () => (await pages(port))[0], 30_000, 1000);
  const page = await connect(target.webSocketDebuggerUrl);
  await page.send("Page.enable");
  await page.send("Fetch.enable", { patterns: [{ urlPattern: "*", requestStage: "Request" }] });
  page.on("Fetch.requestPaused", (p) => {
    page.send("Fetch.failRequest", { requestId: p.requestId, errorReason: "Failed" }).catch(() => {});
  });
  await page.send("Page.reload", { ignoreCache: true });
  const shown = await until("the error page", async () => (await pages(port)).find((t) => isErrorPage(t.url)), 30_000, 500);
  return {
    url: shown.url,
    async restore() {
      await page.send("Fetch.disable").catch(() => {});
      await page.send("Page.reload", { ignoreCache: true }).catch(() => {});
      page.close();
    },
  };
}
