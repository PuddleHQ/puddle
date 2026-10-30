// SPDX-License-Identifier: GPL-3.0-or-later
// What the UI promises about how it is used, read from computed styles and box sizes (never from
// screenshots): every control is at least 24 px (WCAG 2.5.8), the page keeps its layout at 200 %
// zoom on a 1280 px window, motion stops when the system asks for less of it, a select's text fits
// under OS text scaling, a newer record type in the audit log doesn't break the activity page, and
// the first-run flow can be finished from the keyboard alone. Each test has a backend of its own.
import type { Locator, Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { axeViolations } from "./support";

const ROUTES = [
  "/workspaces",
  "/workspaces/web-shop",
  "/workspaces/web-shop/settings",
  "/workspaces/web-shop/network",
  "/workspaces/web-shop/ports",
  "/workspaces/web-shop/git",
  "/workspaces/web-shop/environment",
  "/workspaces/web-shop/shell-init",
  "/inbox",
  "/rules",
  "/activity",
  "/identities",
  "/identities/1",
  "/settings",
  "/settings/network-health",
  "/welcome",
  "/welcome/check",
  "/welcome/certificates",
  "/welcome/connect",
  "/welcome/look",
  "/welcome/workspace",
] as const;

async function open(
  page: Page,
  backend: Backend,
  path: string,
  density: "comfortable" | "compact" = "comfortable",
) {
  await backend.control.reset("lived-in");
  if (density === "compact") {
    const saved = await page.request.put("/api/settings", {
      headers: { Authorization: `Bearer ${backend.token}` },
      data: { ui: { density } },
    });
    expect(saved.ok()).toBe(true);
  }
  await backend.installClock(page);
  await backend.signIn(page);
  await page.goto(path);
  await expect(page.getByRole("heading", { level: 1 }).first()).toBeVisible();
}

/** A click or tap target is a control the user can point at; these are the ones that take input. */
const TARGETS =
  'a[href], button, input:not([type="hidden"]), select, textarea, summary, [role="button"], [role="tab"], [role="menuitem"], [role="switch"], [role="checkbox"], [role="radio"], [role="link"], [tabindex]:not([tabindex="-1"])';

/**
 * The visible targets smaller than 24 x 24 CSS px, as `selector WxH`. Left out, as WCAG 2.5.8 does:
 * a link inside a sentence, and a control that is not drawn (display none, the skip link until it
 * has focus). A checkbox or radio is measured as drawn, so the native 13 px box fails.
 */
function smallTargets(page: Page, within = "body"): Promise<string[]> {
  return page.evaluate(
    ([selector, scope]) => {
      const MIN = 24;
      const small: string[] = [];
      const describe = (el: Element) => {
        const id = el.id ? `#${el.id}` : "";
        const name = (el.getAttribute("aria-label") ?? el.textContent ?? "")
          .trim()
          .replace(/\s+/g, " ")
          .slice(0, 30);
        return `${el.tagName.toLowerCase()}${id} "${name}"`;
      };
      for (const el of document.querySelectorAll(
        `:is(${scope}) :is(${selector})`,
      )) {
        const style = getComputedStyle(el);
        if (style.display === "none" || style.visibility === "hidden") continue;
        const box = el.getBoundingClientRect();
        // Not drawn: a closed region, or the skip link's one-pixel clip.
        if (box.width <= 1 || box.height <= 1) continue;
        if (el.tagName === "A" && style.display === "inline") {
          const text = [...(el.parentElement?.childNodes ?? [])].some(
            (n) => n !== el && n.nodeType === 3 && (n.textContent ?? "").trim(),
          );
          if (text) continue;
        }
        if (box.width < MIN - 0.5 || box.height < MIN - 0.5)
          small.push(
            `${describe(el)} ${Math.round(box.width)}x${Math.round(box.height)}`,
          );
      }
      return small;
    },
    [TARGETS, within] as const,
  );
}

test.describe("target size", () => {
  for (const density of ["comfortable", "compact"] as const) {
    test.describe(`in ${density} spacing`, () => {
      for (const route of ROUTES) {
        test(`every control on ${route} is at least 24 px`, async ({
          page,
          backend,
        }) => {
          await open(page, backend, route, density);
          if (density === "compact")
            await expect(page.locator("html")).toHaveAttribute(
              "data-density",
              "compact",
            );
          await expect
            .poll(() => smallTargets(page), { message: route })
            .toEqual([]);
        });
      }
    });
  }

  test("the controls in every dialog a button opens are at least 24 px, in compact spacing", async ({
    page,
    backend,
  }) => {
    test.setTimeout(120_000);
    const opened: string[] = [];
    for (const route of [
      "/workspaces",
      "/workspaces/web-shop/settings",
      "/workspaces/web-shop/environment",
      "/rules",
      "/identities",
      "/settings",
    ]) {
      await open(page, backend, route, "compact");
      const names = await page
        .getByRole("button", { name: /^(Add|New|Create|Edit|Rename|Delete)/ })
        .evaluateAll((els) =>
          els.map((el) =>
            (el.getAttribute("aria-label") ?? el.textContent ?? "").trim(),
          ),
        );
      for (const name of new Set(names)) {
        await page
          .getByRole("button", { name, exact: true })
          .first()
          .click({ timeout: 5000 });
        const dialog = page.locator("[role=dialog], [role=alertdialog]");
        if ((await dialog.count()) === 0) continue;
        opened.push(`${route} ${name}`);
        await expect
          .poll(() => smallTargets(page, "[role=dialog], [role=alertdialog]"), {
            message: `${route}: ${name}`,
          })
          .toEqual([]);
        await page.keyboard.press("Escape");
        await expect(dialog).toHaveCount(0);
      }
    }
    // The walk must have reached dialogs, or it checked nothing.
    expect(opened.length, opened.join("; ")).toBeGreaterThan(5);
  });

  test("the skip link is a 24 px target once it has focus", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "/rules");
    await page.keyboard.press("Tab");
    const link = page.getByRole("link", { name: "Skip to content" });
    await expect(link).toBeFocused();
    const box = await link.boundingBox();
    expect(box?.width).toBeGreaterThanOrEqual(24);
    expect(box?.height).toBeGreaterThanOrEqual(24);
  });
});

/** The page's overflow in px, and the elements whose right edge is past the window. */
async function overflow(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const root = document.documentElement;
    const width = root.clientWidth;
    const culprits: string[] = [];
    for (const el of document.body.querySelectorAll("*")) {
      const right = el.getBoundingClientRect().right;
      if (right > width + 0.5 && culprits.length < 8)
        culprits.push(
          `${el.tagName.toLowerCase()}${el.id ? `#${el.id}` : ""} right=${Math.round(right)}`,
        );
    }
    return root.scrollWidth - width > 0.5
      ? [`scrolls by ${root.scrollWidth - width}`, ...culprits]
      : [];
  });
}

test.describe("zoomed to 200 % on a 1280 px window", () => {
  // 200 % zoom leaves 640 x 360 CSS px.
  test.use({ viewport: { width: 640, height: 360 } });
  for (const route of ROUTES) {
    test(`${route} has no horizontal scroll bar`, async ({ page, backend }) => {
      await open(page, backend, route);
      await expect.poll(() => overflow(page), { message: route }).toEqual([]);
    });
  }
});

test.describe("reduced motion", () => {
  test.use({ reducedMotion: "reduce" });

  /**
   * The status line of the card that is starting. The page has another `status` before the click (the
   * "requests waiting" banner), so waiting for the first one would not wait for the spinner.
   */
  function busyStatus(page: Page) {
    return page
      .getByRole("article")
      .filter({ hasText: "docs-site" })
      .getByRole("status");
  }

  /** Elements that still move: a transition or animation longer than a frame, or one that never ends. */
  function moving(page: Page): Promise<string[]> {
    return page.evaluate(() => {
      const ms = (list: string) =>
        Math.max(
          ...list.split(",").map((s) => {
            const t = s.trim();
            return t.endsWith("ms") ? parseFloat(t) : parseFloat(t) * 1000;
          }),
        );
      const found: string[] = [];
      for (const el of document.querySelectorAll("*")) {
        const s = getComputedStyle(el);
        const looping =
          s.animationName !== "none" &&
          s.animationIterationCount
            .split(",")
            .some((n) => n.trim() === "infinite");
        if (
          looping ||
          ms(s.transitionDuration) > 1 ||
          (s.animationName !== "none" && ms(s.animationDuration) > 1)
        )
          found.push(
            `${el.tagName.toLowerCase()}.${String((el as HTMLElement).className)} animation ${s.animationName} ${s.animationDuration} x ${s.animationIterationCount}, transition ${s.transitionDuration}`,
          );
      }
      return found;
    });
  }

  test("nothing animates on the screens, and the busy spinner stands still", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "/workspaces");
    for (const route of ["/workspaces", "/inbox", "/rules", "/settings"]) {
      await page.goto(route);
      await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
      await expect.poll(() => moving(page), { message: route }).toEqual([]);
    }
    // A running operation shows a spinner next to its text.
    await page.goto("/workspaces");
    await backend.control.step({ do: "hold_workspaces" });
    await page.getByRole("button", { name: "Start docs-site" }).click();
    await expect(busyStatus(page)).toBeVisible();
    await expect
      .poll(() => moving(page), { message: "while docs-site starts" })
      .toEqual([]);
    // A short transition may still be finishing; a loop never ends, so only loops count.
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            document
              .getAnimations()
              .filter(
                (a) => a.effect?.getComputedTiming().iterations === Infinity,
              ).length,
        ),
      )
      .toBe(0);
  });

  test("with motion allowed, the same spinner does run (the check can fail)", async ({
    page,
    backend,
  }) => {
    await page.emulateMedia({ reducedMotion: "no-preference" });
    await open(page, backend, "/workspaces");
    await backend.control.step({ do: "hold_workspaces" });
    await page.getByRole("button", { name: "Start docs-site" }).click();
    await expect(busyStatus(page)).toBeVisible();
    expect((await moving(page)).join("\n")).toContain("spin");
  });
});

test.describe("OS text scaling", () => {
  // Windows text scaling above 100 % raises the root font size; the controls are sized in rem.
  for (const scale of [125, 150, 200]) {
    test(`text in selects, fields and buttons is not clipped at ${scale} %`, async ({
      page,
      backend,
    }) => {
      let checked = 0;
      for (const route of ["/settings", "/rules", "/workspaces", "/activity"]) {
        await open(page, backend, route);
        await page.evaluate((s) => {
          document.documentElement.style.fontSize = `${s}%`;
        }, scale);
        const clipped = await page.evaluate(() => {
          const out: string[] = [];
          let seen = 0;
          for (const el of document.querySelectorAll(
            'select, button, input:not([type="hidden"]):not([type="checkbox"]):not([type="radio"])',
          )) {
            const box = el.getBoundingClientRect();
            if (box.width <= 1 || box.height <= 1) continue;
            seen += 1;
            const s = getComputedStyle(el);
            // The line this control's text needs: a probe with the same font.
            const probe = document.createElement("span");
            probe.textContent = "Ag";
            probe.style.cssText = `position:absolute;visibility:hidden;font:${s.font};letter-spacing:${s.letterSpacing}`;
            document.body.append(probe);
            const line = probe.getBoundingClientRect().height;
            probe.remove();
            const content =
              el.clientHeight -
              parseFloat(s.paddingTop) -
              parseFloat(s.paddingBottom);
            if (content < line - 0.5)
              out.push(
                `${el.tagName.toLowerCase()} "${(el.getAttribute("aria-label") ?? el.textContent ?? "").trim().slice(0, 24)}": content ${content.toFixed(1)} px, line ${line.toFixed(1)} px`,
              );
          }
          return { out, seen };
        });
        checked += clipped.seen;
        expect(clipped.out, `${route} at ${scale} %`).toEqual([]);
      }
      expect(checked).toBeGreaterThan(10);
    });
  }
});

test.describe("an audit record type the app does not know", () => {
  test("the activity page lists it, opens it raw, and stays accessible", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "/activity");
    await expect(page.locator("tbody tr").first()).toBeVisible();
    const before = await page.locator("tbody tr").count();
    // A newer Puddle writes a record type this build has no model for.
    const future = {
      type: "quantum_tunnel",
      ts: Date.now(),
      workspace_id: "web-shop",
      depth: 3,
    };
    await page.route("**/api/audit?**", async (route) => {
      const response = await route.fetch();
      const body = (await response.json()) as {
        entries: { id: number; record: unknown }[];
      };
      body.entries.unshift({ id: 999_999, record: future });
      await route.fulfill({ response, json: body });
    });
    await page.reload();
    const row = page.locator("tbody tr", { hasText: "quantum_tunnel" });
    await expect(row).toHaveCount(1);
    await expect(row).toContainText("can't describe");
    await expect
      .poll(() => page.locator("tbody tr").count())
      .toBeGreaterThanOrEqual(before);
    // The rest of the page still works: the other rows are there and the new one opens raw.
    await row.getByRole("button").click();
    const raw = page.locator("tbody tr.detail pre");
    expect(JSON.parse((await raw.textContent()) ?? "{}")).toEqual(future);
    await expect.poll(() => axeViolations(page)).toEqual([]);
  });
});

/** Presses Tab until `target` has the focus, so a control the keyboard cannot reach fails the test. */
async function tabTo(
  page: Page,
  target: Locator,
  limit = 60,
  key = "Tab",
): Promise<void> {
  for (let i = 0; i < limit; i += 1) {
    if (await target.evaluate((el) => el === document.activeElement)) return;
    await page.keyboard.press(key);
  }
  const at = await page.evaluate(
    () =>
      `${document.activeElement?.tagName} "${(document.activeElement?.textContent ?? "").trim().slice(0, 40)}"`,
  );
  throw new Error(
    `${String(target)} did not get the focus in ${limit} presses of ${key}; the focus is on ${at}`,
  );
}

test.describe("with the keyboard alone", () => {
  test("a new install is led through every step and ends on the workspace list", async ({
    page,
    backend,
  }) => {
    await backend.control.reset("first-run");
    await backend.signIn(page);
    await page.goto("/");
    const heading = (name: string) =>
      page.getByRole("heading", { level: 1, name });
    const next = async (link: string, then: string) => {
      await tabTo(page, page.getByRole("link", { name: link }));
      await page.keyboard.press("Enter");
      await expect(heading(then)).toBeVisible();
      // Moving to a step puts the focus on its heading, so the next Tab starts inside the step.
      await expect(heading(then)).toBeFocused();
    };
    await expect(heading("Welcome to Puddle")).toBeFocused();
    await next("Get started", "System check");
    await expect(page.getByText("No problems found.")).toBeVisible();
    await next("Continue", "Certificates");
    await next("Continue", "How do you want to connect?");

    // Connect: the radios take the arrow keys, and the direct SSH switch asks first and gives the
    // focus back when it is dismissed.
    const directSsh = page.getByRole("checkbox", {
      name: /Allow direct SSH for new workspaces/,
    });
    await tabTo(page, page.getByRole("radio", { checked: true }).first());
    await tabTo(page, directSsh);
    await page.keyboard.press("Space");
    const trust = page.getByRole("alertdialog", {
      name: "Allow direct SSH for new workspaces?",
    });
    await expect(trust).toBeVisible();
    // The dialog takes the focus when it opens; only then does Tab have a trap to be tested against.
    await expect(trust.locator(":focus")).toHaveCount(1);
    await page.keyboard.press("Tab");
    await expect(
      trust.locator(":focus"),
      "the focus stays inside the dialog",
    ).toHaveCount(1);
    await page.keyboard.press("Escape");
    await expect(trust).toHaveCount(0);
    await expect(directSsh).toBeFocused();
    await expect(directSsh).not.toBeChecked();
    await next("Continue", "Look");

    // Look: the theme radios change with the arrow keys, at once.
    const theme = page.getByRole("radio", { checked: true }).first();
    await tabTo(page, theme);
    const before = await page.locator("html").getAttribute("data-theme");
    await page.keyboard.press("ArrowDown");
    await expect
      .poll(() => page.locator("html").getAttribute("data-theme"))
      .not.toBe(before);
    await next("Continue", "Your first workspace");

    // First workspace: the dialog is filled in and sent without the mouse.
    await tabTo(page, page.getByRole("button", { name: "Create a workspace" }));
    await page.keyboard.press("Enter");
    const form = page.getByRole("dialog", { name: "New workspace" });
    await expect(form).toBeVisible();
    await tabTo(page, form.getByLabel(/Git repository/));
    await page.keyboard.type("https://github.com/example/shop-api.git");
    await expect(form.getByLabel("Name")).toHaveValue("shop-api");
    await tabTo(page, form.getByRole("button", { name: "Create workspace" }));
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/workspaces$/);
    await expect(page.getByText("shop-api").first()).toBeVisible();
  });

  test("from the workspace list to the inbox, and a request is decided on its button", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "/workspaces");
    const nav = page.getByRole("navigation", { name: "Main" });
    await tabTo(page, nav.getByRole("link", { name: /^Inbox/ }));
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/inbox$/);
    const rows = page.locator("li.req");
    // The list renders after the page changes: count only once a row is there.
    await expect(rows.first()).toBeVisible();
    const count = await rows.count();
    const allow = rows
      .first()
      .getByRole("button", { name: /^Allow/ })
      .first();
    await tabTo(page, allow);
    // A slow service: the list drops the row (a refresh does) before the answer to the decision
    // arrives, and the page puts the focus back only once it has the answer. Holding the answer
    // makes that window as wide as on a loaded runner, so the check below has to wait for it.
    let release = () => {};
    const held = new Promise<void>((done) => {
      release = done;
    });
    let answered = 0;
    await page.route("**/api/pending/*/approve", async (route) => {
      answered += 1;
      const answer = await route.fetch();
      await held;
      await route.fulfill({ response: answer });
    });
    await page.keyboard.press("Enter");
    await expect(rows).toHaveCount(count - 1);
    release();
    expect(answered, "the decision went through the held route").toBe(1);
    // The focus ends up somewhere the next Tab can continue from, not at the top of the page.
    await expect
      .poll(() =>
        page.evaluate(() => {
          const el = document.activeElement;
          if (!el || el === document.body) return "body";
          return el.closest("li.req, nav") ? "inbox" : `outside: ${el.tagName}`;
        }),
      )
      .toBe("inbox");
  });
});
