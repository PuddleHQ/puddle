// SPDX-License-Identifier: GPL-3.0-or-later
import AxeBuilder from "@axe-core/playwright";
import {
  expect,
  test as base,
  type Locator,
  type Page,
} from "@playwright/test";
import { readFileSync } from "node:fs";
import { throttleCpu } from "./cpu-throttle.ts";

function connection(): { url: string; token: string } {
  const file = process.env["PUDDLE_E2E_CONNECTION"];
  if (!file)
    throw new Error(
      "PUDDLE_E2E_CONNECTION is not set (see playwright.config.ts)",
    );
  return JSON.parse(readFileSync(file, "utf8")) as {
    url: string;
    token: string;
  };
}

/** What the desktop shell's init script does: hand the page the token before the app starts. */
export async function signIn(page: Page): Promise<void> {
  const { token } = connection();
  await page.addInitScript((t) => {
    (window as unknown as { __PUDDLE__: { token: string } }).__PUDDLE__ = {
      token: t,
    };
  }, token);
}

/** Records Content-Security-Policy violations (there must be none). */
export async function watchCsp(page: Page): Promise<() => Promise<string[]>> {
  await page.addInitScript(() => {
    const seen: string[] = [];
    (window as unknown as { __csp: string[] }).__csp = seen;
    document.addEventListener("securitypolicyviolation", (e) => {
      seen.push(`${e.violatedDirective}: ${e.blockedURI}`);
    });
  });
  return () =>
    page.evaluate(() => (window as unknown as { __csp: string[] }).__csp);
}

type FocusWindow = {
  __focusLog: string[];
  __focusNow: () => string;
};

/**
 * Starts the focus history `expectFocused` prints: every focus change, key press and click on the
 * page, with the time and a short description of the element, oldest dropped past 60 entries.
 * Call it before the first `goto`.
 */
export async function watchFocus(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const log: string[] = [];
    const describe = (n: EventTarget | null) => {
      if (!(n instanceof Element)) return String(n);
      const id = n.id ? `#${n.id}` : "";
      const name = n.getAttribute("aria-label") ?? n.textContent?.trim() ?? "";
      return `${n.tagName.toLowerCase()}${id} "${name.slice(0, 30)}"`;
    };
    const w = window as unknown as FocusWindow;
    w.__focusLog = log;
    w.__focusNow = () => describe(document.activeElement);
    const note = (what: string, target: EventTarget | null, more = "") => {
      log.push(
        `${Math.round(performance.now())}ms ${what} ${describe(target)}${more}`,
      );
      if (log.length > 60) log.shift();
    };
    for (const kind of ["focusin", "focusout"])
      window.addEventListener(kind, (e) => note(kind, e.target), true);
    window.addEventListener(
      "keydown",
      (e) => note("keydown", e.target, ` ${e.key}`),
      true,
    );
    window.addEventListener("click", (e) => note("click", e.target), true);
  });
}

/**
 * `expect(locator).toBeFocused()` that, on a miss, prints what had the focus instead and the focus
 * history, so a failure on a machine we cannot attach to still says who took the focus. Needs
 * `watchFocus(page)` first (the history is empty without it).
 */
export async function expectFocused(
  page: Page,
  locator: Locator,
): Promise<void> {
  try {
    await expect(locator).toBeFocused();
  } catch (error) {
    const seen = await page
      .evaluate(() => {
        const w = window as unknown as Partial<FocusWindow>;
        return {
          active: w.__focusNow?.() ?? "(watchFocus was not called)",
          hasFocus: String(document.hasFocus()),
          history: w.__focusLog ?? [],
        };
      })
      .catch((e: unknown) => ({
        active: `unreadable: ${String(e)}`,
        hasFocus: "unknown",
        history: [] as string[],
      }));
    const report = [
      `focus miss on ${String(locator)}`,
      `active element: ${seen.active} (document.hasFocus(): ${seen.hasFocus})`,
      "focus history, oldest first:",
      ...seen.history.map((line) => `  ${line}`),
    ].join("\n");
    console.error(report);
    await test
      .info()
      .attach("focus-miss", { body: report, contentType: "text/plain" });
    throw error;
  }
}

/**
 * axe with the tags the project's bar names (WCAG 2.0 to 2.2, A and AA).
 *
 * A scan used to cost 1 to 2 s on an idle machine for a page of 200 to 400 elements and 3 to 5 s on the
 * loaded CI host, and a test that scans five states spent most of its 30 s on it. Less than half of
 * that was axe's rules: the builder's default run sends axe's 1.3 MB source to the page twice and
 * opens a second browser page to merge the results. Here the source goes into a document at its
 * first scan only, and the run stays in the page (the builder's legacy mode). The rule set is
 * unchanged.
 */
export async function axeViolations(page: Page) {
  // Legacy mode does not look into cross-origin frames: the app has none, and a page that gains
  // one must say so here instead of passing unscanned.
  if (page.frames().length > 1)
    throw new Error("a frame on the page: axeViolations does not scan frames");
  // The first scan of a document brings axe in; later ones find it there, so the builder is given a
  // script that does nothing instead of the 1.3 MB source.
  const present = await page.evaluate(() => "axe" in window);
  const results = await new AxeBuilder(
    present ? { page, axeSource: "0" } : { page },
  )
    .setLegacyMode()
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
    .analyze();
  // Nothing passed and nothing failed: axe looked at an empty page, which is not a clean scan.
  if (results.passes.length + results.violations.length === 0)
    throw new Error("axe ran no rules on the page");
  return results.violations.map(
    (v) =>
      `${v.id}: ${v.nodes
        .map(
          (n) =>
            `${n.target.join(" ")} (${(n.any[0]?.message ?? n.failureSummary ?? "").replace(/\s+/g, " ")})`,
        )
        .join(", ")}`,
  );
}

/**
 * A walk through states with a scan at some of them, one test for each group of stops.
 *
 * A scan costs seconds on a loaded host, so a test that scans five states runs out of its 30 s
 * there. The walk stays whole (the clicks and page loads are cheap), but each test scans only its
 * group's stops: `scan(page, "stop")` at every stop of the walk, and `scan.done()` at the end.
 * A stop that no group names throws, and so does a group whose stops the walk never reached, so
 * a renamed or dropped stop cannot leave a state unscanned.
 */
export function axeStops(groups: readonly (readonly string[])[]) {
  const named = new Set(groups.flat());
  return {
    groups,
    scanner(group: readonly string[]) {
      const seen = new Set<string>();
      const scan = async (page: Page, stop: string): Promise<void> => {
        if (!named.has(stop))
          throw new Error(`the stop "${stop}" is in no axeStops group`);
        if (!group.includes(stop)) return;
        seen.add(stop);
        expect(await axeViolations(page), stop).toEqual([]);
      };
      scan.done = (): void => {
        expect([...seen].sort(), "the stops the walk reached").toEqual(
          [...group].sort(),
        );
      };
      return scan;
    },
  };
}

export const test = base.extend({
  page: async ({ page, browserName }, use) => {
    await throttleCpu(page, browserName);
    await use(page);
  },
});
export { expect };
