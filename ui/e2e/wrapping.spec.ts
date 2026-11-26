// SPDX-License-Identifier: AGPL-3.0-or-later
// No word breaks in the middle, no button or column header wraps, on every screen at a normal
// width, at phone width and at the largest text size; and the main sidebar stays in the window
// while the page scrolls. The checks read text boxes, not pixels: each word's client rects must
// share one line, and a button's or header's text must sit on one line.
import type { Page } from "@playwright/test";
import { expect, test } from "./fixture";
import { staysEmpty } from "./support";

const ROUTES = [
  "/workspaces",
  "/workspaces/web-shop",
  "/workspaces/web-shop/settings",
  "/workspaces/web-shop/network",
  "/workspaces/web-shop/settings/ports",
  "/workspaces/web-shop/git",
  "/workspaces/web-shop/settings/environment",
  "/inbox",
  "/rules",
  "/activity",
  "/identities",
  "/settings/appearance",
  "/settings/editor",
  "/settings/editor-and-git",
  "/settings/network",
  "/settings/notifications",
  "/settings/this-computer",
  "/settings/workspaces",
  "/settings/network-health",
  "/settings/app-errors",
] as const;

const SIZES = [
  { name: "1280 px wide", width: 1280, fontSize: "100%" },
  { name: "390 px wide", width: 390, fontSize: "100%" },
  {
    name: "1280 px wide at the largest text size",
    width: 1280,
    fontSize: "200%",
  },
  {
    name: "390 px wide at the largest text size",
    width: 390,
    fontSize: "200%",
  },
] as const;

/** Words split over two lines, and buttons or column headers whose text takes more than one. */
async function wraps(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const found: string[] = [];
    const lines = (range: Range) =>
      new Set(
        Array.from(range.getClientRects())
          .filter((r) => r.width > 0)
          .map((r) => Math.round(r.top / 4)),
      ).size;
    const visible = (el: Element) => {
      const box = el.getBoundingClientRect();
      return box.width > 0 && box.height > 0;
    };
    const walker = document.createTreeWalker(
      document.body,
      NodeFilter.SHOW_TEXT,
    );
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      const parent = node.parentElement;
      if (
        !parent ||
        !visible(parent) ||
        parent.closest("script,style,pre,code")
      )
        continue;
      const text = node.textContent ?? "";
      for (const word of text.matchAll(/\S+/g)) {
        const range = document.createRange();
        range.setStart(node, word.index);
        range.setEnd(node, word.index + word[0].length);
        if (lines(range) > 1)
          found.push(
            `word "${word[0]}" breaks in <${parent.tagName.toLowerCase()}>`,
          );
      }
    }
    const single = "button,[role=button],.btn,th";
    for (const el of document.body.querySelectorAll(single)) {
      if (!visible(el)) continue;
      const range = document.createRange();
      range.selectNodeContents(el);
      if (lines(range) > 1)
        found.push(
          `<${el.tagName.toLowerCase()}> "${(el.textContent ?? "").trim().slice(0, 40)}" takes several lines`,
        );
    }
    return found.slice(0, 10);
  });
}

for (const size of SIZES) {
  test.describe(`at ${size.name}`, () => {
    test.use({ viewport: { width: size.width, height: 900 } });

    for (const route of ROUTES) {
      test(`${route} has no broken word, wrapped button or wrapped column header`, async ({
        page,
        backend,
      }) => {
        await backend.installClock(page);
        await backend.signIn(page);
        await page.goto(route);
        await expect(page.locator("h1").first()).toBeVisible();
        await page.evaluate((value) => {
          document.documentElement.style.fontSize = value;
        }, size.fontSize);
        await staysEmpty(page, () => wraps(page));
      });
    }
  });
}

test.describe("the main sidebar", () => {
  test.use({ viewport: { width: 1280, height: 600 } });

  for (const route of [
    "/settings/appearance",
    "/settings/network-health",
    "/rules",
  ]) {
    test(`${route} stays in the window after scrolling the page`, async ({
      page,
      backend,
    }) => {
      await backend.installClock(page);
      await backend.signIn(page);
      await page.goto(route);
      const sidebar = page.locator(".sidebar");
      await expect(sidebar).toBeVisible();
      // Make the page taller than the window so there is something to scroll.
      await page.evaluate(() => {
        const filler = document.createElement("div");
        filler.style.height = "3000px";
        document.getElementById("main")?.append(filler);
      });
      await page.evaluate(() => window.scrollTo(0, 1500));
      await expect
        .poll(() => page.evaluate(() => window.scrollY))
        .toBeGreaterThan(0);
      const box = await sidebar.boundingBox();
      expect(box?.y).toBeCloseTo(0, 0);
      await expect(
        page.getByRole("link", { name: "Identities", exact: true }).first(),
      ).toBeInViewport();
    });
  }
});
