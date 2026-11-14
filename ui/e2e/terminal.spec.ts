// SPDX-License-Identifier: AGPL-3.0-or-later
// The Terminal tab against the real API on the fixture backend (scenario `lived-in`: web-shop runs,
// docs-site is stopped). The fixture's shell is a stand-in that echoes what is typed, answers `size`
// with "<rows> <cols>" and ends on `exit N`. Waits are on conditions (the screen's text, a focus).
import type { Locator, Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import {
  axeStops,
  expectFocused,
  staysEmpty,
  watchCsp,
  watchFocus,
} from "./support";

async function open(page: Page, backend: Backend, workspace = "web-shop") {
  await backend.control.reset("lived-in");
  await backend.signIn(page);
  await page.goto(`/workspaces/${workspace}/terminal`);
  await expect(
    page.getByRole("heading", { level: 2, name: "Terminal" }),
  ).toBeVisible();
}

const screen = (page: Page) => page.locator(".xterm-rows");
const input = (page: Page) => page.locator(".xterm-helper-textarea");
const newTerminal = (page: Page) =>
  page.getByRole("button", { name: "New terminal" });

async function start(page: Page) {
  await newTerminal(page).click();
  await expect(page.getByRole("tab", { name: "Terminal 1" })).toBeVisible();
  await expect(screen(page)).toContainText("$");
  await input(page).focus();
}

async function run(page: Page, line: string) {
  await page.keyboard.type(line);
  await page.keyboard.press("Enter");
}

/** What the page's paste handler sees when the user pastes `text` (the webview's paste event). */
/** Moves the pointer onto a link: the emulator redraws its row on hover, so a locator never settles. */
async function pointAt(page: Page, link: Locator) {
  // Keep the box the poll saw: the emulator can replace the span between two reads, so a box read
  // again after the wait can come back null.
  const seen: Array<NonNullable<Awaited<ReturnType<Locator["boundingBox"]>>>> =
    [];
  await expect
    .poll(
      async () => {
        const box = await link.boundingBox();
        if (box) seen.push(box);
        return box;
      },
      { message: "the link is rendered" },
    )
    .not.toBeNull();
  const box = seen[seen.length - 1] as (typeof seen)[number];
  await page.mouse.move(box.x + 6, box.y + box.height / 2);
}

async function paste(page: Page, text: string) {
  await page.evaluate((t) => {
    const data = new DataTransfer();
    data.setData("text/plain", t);
    document.querySelector(".xterm-helper-textarea")?.dispatchEvent(
      new ClipboardEvent("paste", {
        clipboardData: data,
        bubbles: true,
        cancelable: true,
      }),
    );
  }, text);
}

test("is the second tab of the workspace page and offers a terminal in a running workspace", async ({
  page,
  backend,
}) => {
  const csp = await watchCsp(page);
  await open(page, backend);
  const tabs = page.getByRole("navigation", { name: "Workspace sections" });
  await expect(tabs.getByRole("link").nth(1)).toHaveText("Terminal");
  await expect(tabs.getByRole("link", { name: "Terminal" })).toHaveAttribute(
    "aria-current",
    "page",
  );
  await expect(page.getByText(/No terminal is open/)).toBeVisible();
  await start(page);
  await expect(page.getByRole("tab", { name: "Terminal 1" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(
    page.getByRole("group", { name: "Terminal 1, web-shop" }),
  ).toBeVisible();
  await staysEmpty(page, csp);
});

test("a stopped workspace says to start it", async ({ page, backend }) => {
  await open(page, backend, "docs-site");
  await expect(
    page.getByText("Start the workspace to open a terminal."),
  ).toBeVisible();
  const section = page.getByRole("region", { name: "Terminal" });
  await expect(section.getByRole("button", { name: "Start" })).toBeEnabled();
  await expect(newTerminal(page)).toHaveCount(0);
});

test("typed text reaches the shell and its answer is drawn", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await start(page);
  await run(page, "hello");
  await expect(screen(page)).toContainText("$ hello");
});

test("the size follows the area: the host hears the new size after the window changes", async ({
  page,
  backend,
}) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await open(page, backend);
  await start(page);
  await run(page, "size");
  await expect(screen(page)).toContainText(/\d+ \d+/);
  const wide = /(\d+) (\d+)/.exec((await screen(page).textContent()) ?? "");
  await page.setViewportSize({ width: 700, height: 900 });
  await expect(async () => {
    await run(page, "size");
    const text = (await screen(page).textContent()) ?? "";
    const sizes = [...text.matchAll(/(\d+) (\d+)/g)];
    const last = sizes.at(-1);
    expect(Number(last?.[2])).toBeLessThan(Number(wide?.[2]));
  }).toPass();
});

test("keys the browser owns go to the shell: Ctrl+R, Ctrl+W and F5 are cancelled, so the page is not reloaded or closed", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await start(page);
  // The events themselves are kept (a capture listener on the window sees every key first), and
  // read after the emulator and the terminal's key rule ran: `defaultPrevented` is then the state
  // the browser acts on. (A test browser has no accelerators to run, so the cancel is what is
  // measured, on the page's real handlers and the browser's own key events.)
  await page.evaluate(() => {
    const w = window as unknown as {
      __kept: boolean;
      __keys: KeyboardEvent[];
    };
    w.__kept = true;
    w.__keys = [];
    window.addEventListener("keydown", (e) => w.__keys.push(e), true);
  });
  const cancelled = ["Control+r", "Control+w", "Control+p", "F5", "F7"];
  for (const key of [...cancelled, "Tab"]) {
    await page.keyboard.press(key);
  }
  await run(page, "still here");
  await expect(screen(page)).toContainText("still here");
  const seen = await page.evaluate(() =>
    (window as unknown as { __keys: KeyboardEvent[] }).__keys.map((e) => ({
      key: e.key,
      ctrl: e.ctrlKey,
      defaultPrevented: e.defaultPrevented,
    })),
  );
  const prevented = (key: string, ctrl: boolean) =>
    seen.find((k) => k.key.toLowerCase() === key && k.ctrl === ctrl)
      ?.defaultPrevented;
  expect(prevented("r", true), "Ctrl+R cancelled").toBe(true);
  expect(prevented("w", true), "Ctrl+W cancelled").toBe(true);
  expect(prevented("p", true), "Ctrl+P cancelled").toBe(true);
  expect(prevented("f5", false), "F5 cancelled").toBe(true);
  expect(prevented("f7", false), "F7 cancelled").toBe(true);
  await expect
    .poll(() =>
      page.evaluate(() => (window as unknown as { __kept?: boolean }).__kept),
    )
    .toBe(true);
});

test("Ctrl+Shift+C copies the selection as plain text and does nothing without one", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await page.addInitScript(() => {
    (window as unknown as { __copied: string[] }).__copied = [];
    document.addEventListener("copy", (e) => {
      (window as unknown as { __copied: string[] }).__copied.push(
        e.clipboardData?.getData("text/plain") ?? "",
      );
    });
  });
  await page.reload();
  await start(page);
  await run(page, "copy me");
  await page.keyboard.press("Control+Shift+C");
  expect(
    // eslint-disable-next-line no-restricted-syntax -- absence: nothing was selected, so one read after the key press shows the copy handler stayed quiet
    await page.evaluate(
      () => (window as unknown as { __copied: string[] }).__copied,
    ),
  ).toEqual([]);
  await page
    .getByRole("group", { name: "Terminal 1, web-shop" })
    .click({ button: "right" });
  await page.getByRole("menuitem", { name: "Select all" }).click();
  await page.keyboard.press("Control+Shift+C");
  await expect
    .poll(() =>
      page.evaluate(() =>
        (window as unknown as { __copied: string[] }).__copied.join(""),
      ),
    )
    .toContain("copy me");
});

test("the right-click menu offers Copy, Select all and Clear", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await start(page);
  await run(page, "to clear");
  await page
    .getByRole("group", { name: "Terminal 1, web-shop" })
    .click({ button: "right" });
  const menu = page.getByRole("menu", { name: "Terminal actions" });
  await expect(menu.getByRole("menuitem")).toHaveText([
    "Copy",
    "Select all",
    "Clear",
  ]);
  await expect(menu.getByRole("menuitem", { name: "Copy" })).toBeDisabled();
  await menu.getByRole("menuitem", { name: "Clear" }).click();
  await expect(screen(page)).not.toContainText("to clear");
  await page.keyboard.type("after");
  await expect(screen(page)).toContainText("after");
});

test("paste puts the clipboard's text in as typed, and Escape closes the menu", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await start(page);
  await paste(page, "pasted text");
  await expect(screen(page)).toContainText("pasted text");
  await page
    .getByRole("group", { name: "Terminal 1, web-shop" })
    .click({ button: "right" });
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toHaveCount(0);
});

test("a long paste goes through in pieces and the screen keeps up", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await start(page);
  await paste(page, "x".repeat(150_000));
  await run(page, "done");
  await expect(screen(page)).toContainText("done", { timeout: 30_000 });
});

test("a program's title becomes the sub-tab's label and a screen reader still hears Terminal N", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await start(page);
  await paste(page, "\u001b]0;npm run dev\u0007");
  const tab = page.getByRole("tab", { name: "Terminal 1" });
  await expect(tab).toContainText("npm run dev");
});

test("the bell marks a sub-tab that is not in view", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await start(page);
  await newTerminal(page).click();
  await expect(page.getByRole("tab", { name: "Terminal 2" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await page.getByRole("tab", { name: "Terminal 1" }).click();
  await expect(screen(page)).toContainText("$");
  await input(page).focus();
  await paste(page, "\u0007");
  await page.getByRole("tab", { name: "Terminal 2" }).click();
  await page.getByRole("tab", { name: "Terminal 1" }).click();
  await expect(page.getByText(/Bell rang\./)).toHaveCount(0);
});

test("a web address opens in the browser on Ctrl+click; a local one says to forward the port", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await start(page);
  await run(page, "https://example.com/docs");
  const link = page
    .locator(".xterm-rows span", { hasText: "https://example.com/docs" })
    .first();
  await pointAt(page, link);
  await expect(page.getByText("https://example.com/docs").last()).toBeVisible();
  const popup = page.waitForEvent("popup");
  await page.keyboard.down("Control");
  await page.mouse.down();
  await page.mouse.up();
  await page.keyboard.up("Control");
  expect((await popup).url()).toContain("example.com");
  await run(page, "http://localhost:3999/x");
  const local = page
    .locator(".xterm-rows span", { hasText: "http://localhost:3999/x" })
    .first();
  await pointAt(page, local);
  await expect(
    page.getByText(/forward port 3999 on the Ports tab/),
  ).toBeVisible();
});

test("an ended terminal keeps its screen and says why, with Restart under a separator", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await start(page);
  await run(page, "before");
  await run(page, "exit 3");
  await expect(page.getByText("The shell exited with status 3.")).toBeVisible();
  await expect(screen(page)).toContainText("before");
  await page.getByRole("button", { name: "Restart" }).click();
  await expect(screen(page)).toContainText("----");
  await expect(screen(page)).toContainText("before");
  await expect(page.getByText("The shell exited with status 3.")).toHaveCount(
    0,
  );
  await input(page).focus();
  await run(page, "again");
  await expect(screen(page)).toContainText("$ again");
});

test("a session that ends without a word is said, and New terminal replaces it", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await start(page);
  await run(page, "vanish");
  await expect(
    page.getByText("The workspace crashed; the terminal ended."),
  ).toBeVisible();
  await expect(
    page.getByText("the terminal's session ended without saying why"),
  ).toBeVisible();
  await page.getByRole("button", { name: "New terminal" }).last().click();
  await expect(page.getByRole("tab", { name: "Terminal 2" })).toBeVisible();
});

test("a terminal outlives the tab: going to another tab and back keeps the screen", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await start(page);
  await run(page, "remember me");
  await page.getByRole("link", { name: "Ports" }).click();
  await expect(
    page.getByRole("heading", { level: 2, name: "Ports" }),
  ).toBeVisible();
  await page.getByRole("link", { name: "Terminal" }).click();
  await expect(screen(page)).toContainText("remember me");
  await expect(page.getByRole("tab", { name: "Terminal 1" })).toBeVisible();
});

test("closing a sub-tab ends its session and moves focus to a neighbour", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await start(page);
  await newTerminal(page).click();
  await page.getByRole("button", { name: "Close Terminal 2" }).click();
  await expect(page.getByRole("tab")).toHaveCount(1);
  await expect(page.getByRole("tab", { name: "Terminal 1" })).toBeFocused();
});

test("New terminal turns off at 8 terminals and says why", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  for (let i = 0; i < 8; i += 1) await newTerminal(page).click();
  await expect(newTerminal(page)).toBeDisabled();
  await expect(
    page.getByText(/This workspace has 8 terminals open/),
  ).toBeVisible();
});

test("F6 moves focus out of the terminal to the sub-tab row, and Tab goes on from there", async ({
  page,
  backend,
}) => {
  await watchFocus(page);
  await open(page, backend);
  await start(page);
  await page.keyboard.press("F6");
  await expectFocused(page, page.getByRole("tab", { name: "Terminal 1" }));
});

test("Screen reader mode adds the emulator's accessible text", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await start(page);
  await expect(page.locator(".xterm-accessibility")).toHaveCount(0);
  await page.getByRole("checkbox", { name: "Screen reader mode" }).check();
  await expect(page.locator(".xterm-accessibility")).toHaveCount(1);
  await input(page).focus();
  await run(page, "read me");
  await expect(page.locator(".xterm-accessibility")).toContainText("read me");
});

test("the terminal's colours follow the theme", async ({ page, backend }) => {
  await open(page, backend);
  await start(page);
  const background = () =>
    page
      .locator(".xterm-screen")
      .evaluate(
        (el) =>
          getComputedStyle(
            el
              .closest(".xterm")
              ?.querySelector(".xterm-scrollable-element") as HTMLElement,
          ).backgroundColor,
      );
  await page.emulateMedia({ colorScheme: "dark" });
  await expect.poll(background).not.toBe("");
  const dark = await background();
  await page.emulateMedia({ colorScheme: "light" });
  await expect.poll(background).not.toBe(dark);
});

test("at 390 px the page scrolls only up and down", async ({
  page,
  backend,
}) => {
  await page.setViewportSize({ width: 390, height: 800 });
  await open(page, backend);
  await start(page);
  await run(page, "size");
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          document.documentElement.scrollWidth -
          document.documentElement.clientWidth,
      ),
    )
    .toBeLessThanOrEqual(0);
});

const stops = axeStops([["live terminal"], ["ended terminal", "menu"]]);
for (const scheme of ["light", "dark"] as const) {
  for (const group of stops.groups) {
    test(`axe finds nothing in ${scheme}: ${group.join(", ")}`, async ({
      page,
      backend,
    }) => {
      const scan = stops.scanner(group);
      await page.emulateMedia({ colorScheme: scheme });
      const csp = await watchCsp(page);
      await open(page, backend);
      await start(page);
      await run(page, "scan me");
      await expect(screen(page)).toContainText("scan me");
      await scan(page, "live terminal");
      await page
        .getByRole("group", { name: "Terminal 1, web-shop" })
        .click({ button: "right" });
      await expect(page.getByRole("menu")).toBeVisible();
      await scan(page, "menu");
      await page.keyboard.press("Escape");
      await run(page, "exit 0");
      await expect(
        page.getByText("The shell exited with status 0."),
      ).toBeVisible();
      await scan(page, "ended terminal");
      scan.done();
      await staysEmpty(page, csp);
    });
  }
}
