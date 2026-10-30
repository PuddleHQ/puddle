// SPDX-License-Identifier: GPL-3.0-or-later
// The density setting (comfortable / compact) against the real API on the fixture backend:
// chosen in Settings, kept by Puddle, applied live, still accessible in both densities, with the
// layout of each read from computed styles. Waits are on conditions, never on time.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { axeViolations } from "./support";

const DENSITIES = ["comfortable", "compact"] as const;
const SCREENS = [
  { path: "/settings", heading: "Settings" },
  { path: "/workspaces", heading: "Workspaces" },
  { path: "/rules", heading: "Rules" },
  { path: "/activity", heading: "Activity" },
];

const saved = (page: Page) =>
  page.waitForResponse(
    (r) => r.request().method() === "PUT" && r.url().endsWith("/api/settings"),
  );

async function setDensity(page: Page, value: string) {
  await page.getByLabel("Density", { exact: true }).selectOption(value);
  // Choosing what is already stored changes nothing, and Save stays off.
  const save = page.getByRole("button", { name: "Save" });
  if (await save.isEnabled()) {
    const done = saved(page);
    await save.click();
    await done;
  }
}

async function open(page: Page, backend: Backend, path: string) {
  await backend.signIn(page);
  await page.goto(path);
}

const spaceFour = (page: Page) =>
  page.evaluate(() =>
    parseFloat(
      getComputedStyle(document.documentElement).getPropertyValue("--space-4"),
    ),
  );

test.describe("the density setting", () => {
  test("is comfortable by default, changes the whole app live, and Puddle keeps it", async ({
    page,
    backend,
    request,
  }) => {
    await open(page, backend, "/settings");
    await expect(page.getByLabel("Density", { exact: true })).toHaveValue(
      "comfortable",
    );
    await expect(page.locator("html")).not.toHaveAttribute(
      "data-density",
      /.*/,
    );
    const roomy = await spaceFour(page);

    // A marker on the window proves the page is not reloaded by the change.
    await page.evaluate(() => {
      (window as unknown as { __kept: boolean }).__kept = true;
    });
    await setDensity(page, "compact");
    await expect(page.locator("html")).toHaveAttribute(
      "data-density",
      "compact",
    );
    await expect.poll(() => spaceFour(page)).toBeLessThan(roomy);
    await expect
      .poll(() =>
        page.evaluate(() => (window as unknown as { __kept?: boolean }).__kept),
      )
      .toBe(true);

    // The sidebar links go to another screen without a reload and keep it.
    await page
      .getByRole("navigation", { name: "Main" })
      .getByRole("link", { name: /^Rules/ })
      .click();
    await expect(
      page.getByRole("heading", { level: 1, name: "Rules" }),
    ).toBeVisible();
    await expect(page.locator("html")).toHaveAttribute(
      "data-density",
      "compact",
    );

    const view = await request.get("/api/settings", {
      headers: { Authorization: `Bearer ${backend.token}` },
    });
    expect(
      ((await view.json()) as { ui: { density: string } }).ui.density,
    ).toBe("compact");

    // A new launch has a new origin: nothing in the page's storage survives, Puddle remembers.
    await page.evaluate(() => localStorage.clear());
    await page.goto("/settings");
    await expect(page.getByLabel("Density", { exact: true })).toHaveValue(
      "compact",
    );
    await expect(page.locator("html")).toHaveAttribute(
      "data-density",
      "compact",
    );

    await setDensity(page, "comfortable");
    await expect(page.locator("html")).not.toHaveAttribute(
      "data-density",
      /.*/,
    );
    await expect.poll(() => spaceFour(page)).toBe(roomy);
  });

  test("a compact choice is on the page before its body is, so nothing is styled roomy first", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "/settings");
    await setDensity(page, "compact");
    // Records, in document order, when <html> gets the mark and when <body> is parsed.
    await page.addInitScript(() => {
      const seen: string[] = [];
      (window as unknown as { __seen: string[] }).__seen = seen;
      new MutationObserver((records) => {
        for (const r of records) {
          if (r.type === "attributes")
            seen.push(
              `density=${(r.target as Element).getAttribute("data-density")}`,
            );
          for (const node of r.addedNodes)
            if (node.nodeName === "BODY") seen.push("body");
        }
      }).observe(document, {
        subtree: true,
        childList: true,
        attributes: true,
        attributeFilter: ["data-density"],
      });
    });
    await page.goto("/activity");
    await expect(
      page.getByRole("heading", { level: 1, name: "Activity" }),
    ).toBeVisible();
    const seen = await page.evaluate(
      () => (window as unknown as { __seen: string[] }).__seen,
    );
    expect(seen.slice(0, 2)).toEqual(["density=compact", "body"]);
  });

  test("picked while the app is still reading the settings, stays picked", async ({
    page,
    backend,
  }) => {
    // The Settings screen and the app's own start-up both read the settings. The first answer
    // shows the screen; the rest are held until the user has picked, then answered.
    const isRead = (r: { method(): string; url(): string }) =>
      r.method() === "GET" && new URL(r.url()).pathname === "/api/settings";
    let reads = 0;
    let answered = 0;
    let release!: () => void;
    const picked = new Promise<void>((done) => (release = done));
    await page.route(
      (url) => url.pathname === "/api/settings",
      async (route) => {
        if (isRead(route.request()) && ++reads > 1) await picked;
        await route.continue();
      },
    );
    page.on("requestfinished", (r) => {
      if (isRead(r)) answered++;
    });
    await open(page, backend, "/settings");
    await page.getByLabel("Density", { exact: true }).selectOption("compact");
    await expect(page.locator("html")).toHaveAttribute(
      "data-density",
      "compact",
    );
    expect(reads, "reads held back").toBeGreaterThan(1);
    release();
    await expect
      .poll(() => answered, { message: "reads answered" })
      .toBe(reads);

    const done = saved(page);
    await page.getByRole("button", { name: "Save" }).click();
    await done;
    await expect(page.locator("html")).toHaveAttribute(
      "data-density",
      "compact",
    );
    await expect(page.getByLabel("Density", { exact: true })).toHaveValue(
      "compact",
    );
  });

  for (const density of DENSITIES) {
    test(`${density}: axe finds nothing, targets stay at least 24px, focus is visible`, async ({
      page,
      backend,
    }) => {
      await open(page, backend, "/settings");
      await setDensity(page, density);
      for (const { path, heading } of SCREENS) {
        await page
          .getByRole("navigation", { name: "Main" })
          .getByRole("link", { name: new RegExp(`^${heading}`) })
          .click();
        await expect(
          page.getByRole("heading", { level: 1, name: heading }),
        ).toBeVisible();
        await expect
          .poll(() => axeViolations(page), { message: path })
          .toEqual([]);

        const small = () =>
          page.evaluate(() => {
            const out: string[] = [];
            // Native checkboxes are left to axe's spacing rule: they are small in both densities.
            const sel =
              "button, select, input:not([type=hidden], [type=checkbox], [type=radio]), [role=radio]";
            for (const el of document.querySelectorAll(sel)) {
              const r = el.getBoundingClientRect();
              if (r.width === 0 || r.height === 0) continue;
              if (r.height < 24 || r.width < 24)
                out.push(
                  `${el.tagName} ${el.getAttribute("aria-label") ?? el.id} ${r.width}x${r.height}`,
                );
            }
            return out;
          });
        await expect
          .poll(small, { message: `${path} targets under 24px` })
          .toEqual([]);
      }

      // The focus ring is the same two pixels in both densities.
      await page.keyboard.press("Tab");
      await expect
        .poll(() =>
          page.evaluate(() => {
            const s = getComputedStyle(document.activeElement as Element);
            return { width: s.outlineWidth, style: s.outlineStyle };
          }),
        )
        .toEqual({ width: "2px", style: "solid" });
    });
  }
});

// Layout checks on computed styles and box sizes, not screenshots: what density changes is the
// spacing tokens, the line height and the control size, and those are read straight from the
// elements, so a new row or a longer text passes while a changed padding, gap or control height
// fails. They need no fonts, so they run on every platform and in every browser project.
const TOKENS = {
  comfortable: {
    s1: 4,
    s2: 8,
    s3: 12,
    s4: 16,
    s6: 24,
    s8: 32,
    control: 32,
    line: 1.5,
  },
  compact: {
    s1: 2,
    s2: 6,
    s3: 8,
    s4: 12,
    s6: 18,
    s8: 24,
    control: 28,
    line: 1.35,
  },
} as const;

// What each screen has on it: its first heading's size (the same in both densities) and the
// controls the checks below measure.
const LAYOUT = {
  "/settings": {
    h1: 32,
    card: true,
    select: true,
    selectWaitsForData: true,
    oneLineButton: true,
  },
  "/workspaces": {
    h1: 24,
    card: true,
    select: false,
    selectWaitsForData: false,
    oneLineButton: true,
  },
  "/rules": {
    h1: 24,
    card: false,
    select: true,
    selectWaitsForData: true,
    oneLineButton: true,
  },
  // The Activity filters are in its header, so they show before the log has loaded.
  "/activity": {
    h1: 24,
    card: false,
    select: true,
    selectWaitsForData: false,
    oneLineButton: false,
  },
} as const;

type Box = {
  height: number;
  padTop: number;
  padLeft: number;
  rowGap: number;
  fontSize: number;
  lineHeight: number;
};

// The first rendered element matching `selector`, or null.
async function measure(page: Page, selector: string): Promise<Box | null> {
  return page.evaluate((sel) => {
    const el = [...document.querySelectorAll(sel)].find((e) => {
      const r = e.getBoundingClientRect();
      return r.width > 0 && r.height > 0;
    });
    if (!el) return null;
    const s = getComputedStyle(el);
    const px = (v: string) => (v === "normal" ? 0 : parseFloat(v));
    return {
      height: el.getBoundingClientRect().height,
      padTop: px(s.paddingTop),
      padLeft: px(s.paddingLeft),
      rowGap: px(s.rowGap),
      fontSize: px(s.fontSize),
      lineHeight: px(s.lineHeight),
    };
  }, selector);
}

// The measure of an element that exists only once the screen has loaded its data (the Rules
// filters, the Settings cards): polled until it has a box, never read once.
async function required(page: Page, selector: string): Promise<Box> {
  let box: Box | null = null;
  await expect
    .poll(
      async () => {
        box = await measure(page, selector);
        return box !== null;
      },
      { message: `${selector} is on the screen` },
    )
    .toBe(true);
  return box as unknown as Box;
}

// What each screen reads before it shows its controls.
const DATA = {
  "/settings": "/api/settings",
  "/workspaces": "/api/workspaces",
  "/rules": "/api/rules",
  "/activity": "/api/audit",
} as const;

// Holds the screen's data read until `release()`, so every screen is measured once with its data
// missing, whatever the runner's speed. `reads()` says how many reads it held.
async function holdData(page: Page, pathname: string) {
  let held = 0;
  let release!: () => void;
  const gate = new Promise<void>((done) => (release = done));
  await page.route(
    (url) => url.pathname === pathname,
    async (route) => {
      if (route.request().method() === "GET") {
        held += 1;
        await gate;
      }
      await route.continue();
    },
  );
  return { release, reads: () => held };
}

test.describe("layout by density", () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  for (const density of DENSITIES) {
    for (const { path, heading } of SCREENS) {
      test(`${density}: ${path} spacing, control and text sizes follow the tokens`, async ({
        page,
        backend,
      }) => {
        const t = TOKENS[density];
        const layout = LAYOUT[path as keyof typeof LAYOUT];
        await backend.control.reset("lived-in");
        await backend.installClock(page);
        await open(page, backend, "/settings");
        await setDensity(page, density);
        const data = await holdData(page, DATA[path as keyof typeof DATA]);
        await page.goto(path);
        await expect(
          page.getByRole("heading", { level: 1, name: heading }),
        ).toBeVisible();
        // The heading shows while the data is still on its way: the controls below are not there yet.
        await expect
          .poll(data.reads, { message: "the screen's data read is held" })
          .toBeGreaterThan(0);
        if (layout.selectWaitsForData)
          // eslint-disable-next-line no-restricted-syntax -- the data read is held (asserted just above), so the controls are absent by construction
          expect(await measure(page, "main select")).toBeNull();
        data.release();

        // The page settles on the chosen density once the settings have loaded.
        await expect
          .poll(() => spaceFour(page), { message: "the density has applied" })
          .toBe(t.s4 / 16);

        await expect
          .poll(
            async () => {
              const main = await required(page, "main");
              return [main.padTop, main.padLeft];
            },
            { message: "main padding" },
          )
          .toEqual([t.s6, t.s8]);

        const body = await required(page, "body");
        expect(body.fontSize, "body text size").toBe(16);
        expect(body.lineHeight, "body line height").toBeCloseTo(16 * t.line, 1);

        expect((await required(page, "h1")).fontSize, "heading size").toBe(
          layout.h1,
        );

        const link = await required(page, "nav[aria-label=Main] a");
        expect([link.padTop, link.padLeft], "sidebar link padding").toEqual([
          t.s2,
          t.s3,
        ]);
        expect(link.height, "sidebar link height").toBeCloseTo(
          16 * t.line + 2 * t.s2,
          0,
        );

        const button = await required(page, ".btn");
        expect([button.padTop, button.padLeft], "button padding").toEqual([
          t.s1,
          t.s3,
        ]);
        expect(button.fontSize, "button text size").toBe(14);
        // A label that wraps makes a button taller, never shorter; one line is exactly the minimum.
        if (layout.oneLineButton)
          expect(button.height, "button height").toBe(t.control);
        else
          expect(button.height, "button height").toBeGreaterThanOrEqual(
            t.control,
          );

        if (layout.select) {
          const select = await required(page, "main select");
          expect(select.height, "select height").toBe(t.control);
          expect(select.padLeft, "select padding").toBe(t.s2);
        }

        if (layout.card) {
          const card = await required(page, ".card");
          expect([card.padTop, card.padLeft], "card padding").toEqual([
            t.s4,
            t.s4,
          ]);
          expect(card.rowGap, "card row gap").toBe(t.s3);
        }
      });
    }
  }
});
