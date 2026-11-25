// SPDX-License-Identifier: AGPL-3.0-or-later
// What the Terminal tab looks like in the installed app's WebView2, read from the page over the DevTools
// Protocol (plain Node 24, no packages). A release build has no debugging port of its own; usable.mjs
// opens one with the WebView2 policy `AdditionalBrowserArguments` (see `debugPortPolicy`).
//
// The report answers: is the emulator there and sized, is its stylesheet applied, does it use the
// terminal font, do ANSI colours differ from the default text, does a mouse drag select, and did the
// page log a CSP or stylesheet error. A grey box with grey text fails the first four.
import { spawnSync } from "node:child_process";
import { connect } from "./cdp.mjs";
import { until } from "./poll.mjs";

const POLICY_KEYS = ["HKCU", "HKLM"].map((hive) => `${hive}\\Software\\Policies\\Microsoft\\Edge\\WebView2\\AdditionalBrowserArguments`);

/** Sets (`port`) or removes (`undefined`) the policy that gives every WebView2 app of this user a DevTools port. */
export function debugPortPolicy(port) {
  for (const key of POLICY_KEYS) {
    if (port === undefined) {
      spawnSync("reg", ["delete", key, "/f"], { stdio: "ignore" });
      continue;
    }
    for (const name of ["*", "puddle-app.exe"]) {
      const r = spawnSync("reg", ["add", key, "/v", name, "/t", "REG_SZ", "/d", `--remote-debugging-port=${port} --remote-allow-origins=*`, "/f"], { encoding: "utf8" });
      if (r.status !== 0) console.log(`     reg add ${key} ${name}: ${r.stderr || r.stdout}`.trim());
    }
  }
}

/** What the WebView2 processes were started with, for the log when no DevTools page shows up. */
export function webviewCommandLines() {
  const r = spawnSync("powershell", ["-NoProfile", "-Command", "Get-CimInstance Win32_Process -Filter \"Name='msedgewebview2.exe'\" | Select-Object -First 3 -ExpandProperty CommandLine"], { encoding: "utf8" });
  return (r.stdout || r.stderr || "").trim().slice(0, 1500);
}

/** The page target whose URL path ends in `/terminal`, or the first page. */
async function terminalTarget(port) {
  const list = await (await fetch(`http://127.0.0.1:${port}/json`, { signal: AbortSignal.timeout(5000) })).json();
  const pages = list.filter((t) => t.type === "page" && t.webSocketDebuggerUrl);
  return pages.find((t) => /\/terminal(\?|#|$)/.test(t.url)) ?? pages[0];
}

/** The page-side read: sizes, styles and the coloured cells; one object. */
export const READ = `(() => {
  const q = (s, r = document) => r.querySelector(s);
  const xterm = q(".xterm");
  const rect = (el) => (el ? (({ width, height }) => ({ width, height }))(el.getBoundingClientRect()) : null);
  const spanOf = (text) => [...document.querySelectorAll(".xterm-rows span")].find((s) => s.textContent === text);
  const style = (el) => (el ? getComputedStyle(el) : null);
  const probe = document.createElement("span");
  probe.style.fontFamily = "var(--font-mono)";
  document.body.append(probe);
  const wanted = getComputedStyle(probe).fontFamily;
  probe.remove();
  const rows = q(".xterm-rows");
  const red = spanOf("COLOR-RED");
  const green = spanOf("COLOR-GREEN");
  return {
    url: location.href,
    xterm: rect(xterm),
    screen: rect(q(".xterm-screen")),
    renderer: q(".xterm-rows") ? "dom" : q("canvas.xterm-link-layer, .xterm-screen canvas") ? "canvas" : "none",
    helperPositioned: style(q(".xterm-helpers"))?.position,
    fontFamily: style(rows)?.fontFamily,
    wantedFontFamily: wanted,
    defaultColour: style(rows)?.color,
    background: style(q(".xterm-scrollable-element, .xterm-viewport") ?? xterm)?.backgroundColor,
    red: red && { colour: style(red).color, classes: red.className },
    green: green && { colour: style(green).color, classes: green.className, weight: style(green).fontWeight },
    redBox: red && (({ x, y, width, height }) => ({ x, y, width, height }))(red.getBoundingClientRect()),
    styleSheets: document.styleSheets.length,
    viewportStyle: q(".xterm-viewport")?.getAttribute("style"),
    viewportBackground: style(q(".xterm-viewport"))?.backgroundColor,
    redInlineStyle: red?.getAttribute("style"),
    rowsHead: [...document.querySelectorAll(".xterm-rows > div")].slice(0, 6).map((d) => d.innerHTML.slice(0, 300)),
    styleElements: [...document.querySelectorAll("style")].map((e) => ({ owner: e.parentElement?.tagName, chars: e.textContent.length, rules: e.sheet?.cssRules.length ?? null, head: e.textContent.slice(0, 200), nonce: e.nonce })),
    themeRules: [...document.querySelectorAll("style")].flatMap((e) => [...(e.sheet?.cssRules ?? [])].map((r) => r.cssText)).filter((t) => /fg-(0|1|2|9) |fg-25[0-5]|bg-(0|1|257|258)|xterm-viewport|scrollable-element \\{|xterm-rows \\{|xterm-screen|\\.xterm \\{|minimum|selection/i.test(t)).slice(0, 25).map((t) => t.slice(0, 220)),
    scrollableStyle: q(".xterm-scrollable-element")?.getAttribute("style"),
    xtermStyle: xterm?.getAttribute("style"),
    probeReads: (() => {
      const pr = q(".probe") ?? q("body > span[aria-hidden=true]");
      if (!pr) return "no .probe";
      const reads = {};
      for (const t of ["--color-surface", "--color-text", "--color-danger"]) {
        pr.style.color = "";
        pr.style.color = "var(" + t + ")";
        const fresh = getComputedStyle(pr).color;
        void pr.offsetWidth;
        const afterLayout = getComputedStyle(pr).color;
        const clone = document.createElement("span");
        clone.style.color = "var(" + t + ")";
        document.body.append(clone);
        const inBody = getComputedStyle(clone).color;
        clone.remove();
        reads[t] = { where: pr.parentElement?.tagName, styleAttr: pr.getAttribute("style"), fresh, afterLayout, inBody, rootValue: getComputedStyle(document.documentElement).getPropertyValue(t), probeValue: getComputedStyle(pr).getPropertyValue(t) };
      }
      pr.style.color = "";
      return { reads, display: getComputedStyle(pr).display, connected: pr.isConnected, ancestors: (() => { const a = []; for (let e = pr.parentElement; e; e = e.parentElement) a.push(e.tagName + "." + e.className + " " + getComputedStyle(e).color + " scheme=" + getComputedStyle(e).colorScheme); return a.slice(0, 8); })() };
    })(),
    xtermClasses: xterm?.className,
    colorScheme: matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light",
    forcedColors: matchMedia("(forced-colors: active)").matches,
    tokens: Object.fromEntries(["--color-surface", "--color-text", "--color-accent", "--color-danger", "--color-success", "--color-warning", "--color-accent-subtle"].map((t) => {
      const p = document.createElement("span");
      p.style.color = "var(" + t + ")";
      document.body.append(p);
      const out = [getComputedStyle(document.documentElement).getPropertyValue(t).trim(), getComputedStyle(p).color];
      p.remove();
      return [t, out];
    })),
  };
})()`;

/** The Windows clipboard's text (`set` writes it first); read through PowerShell, outside the page, so it is what another program would see. */
export function windowsClipboard(set) {
  const script = set === undefined ? "Get-Clipboard -Raw" : `Set-Clipboard -Value '${set.replaceAll("'", "''")}'`;
  const r = spawnSync("powershell", ["-NoProfile", "-Sta", "-Command", script], { encoding: "utf8" });
  if (r.status !== 0) throw new Error(`clipboard ${set === undefined ? "read" : "write"} failed: ${r.stderr || r.stdout}`);
  return set === undefined ? (r.stdout ?? "").replace(/\r?\n$/, "") : "";
}

/** Page-side: the centre of the red word's cell run, the Terminal menu (if open) and what the native menu event did. */
const MENU_READ = `(() => {
  const red = [...document.querySelectorAll(".xterm-rows span")].find((s) => s.textContent === "COLOR-RED");
  const box = red?.getBoundingClientRect();
  const menu = document.querySelector('[role="menu"][aria-label="Terminal actions"]');
  const items = menu ? [...menu.querySelectorAll('[role="menuitem"]')].map((b) => { const r = b.getBoundingClientRect(); return { name: b.textContent.trim(), disabled: b.disabled, x: r.x + r.width / 2, y: r.y + r.height / 2 }; }) : [];
  return {
    word: box && { x: box.x + box.width / 2, y: box.y + box.height / 2, left: box.x, width: box.width },
    menu: Boolean(menu),
    items,
    rows: [...document.querySelectorAll(".xterm-rows > div")].map((d) => d.textContent).join("\\n"),
    nativePrevented: window.__nativeMenuPrevented,
  };
})()`;

/**
 * The right-click menu of the Terminal, driven as a user does it with real mouse events over the DevTools
 * port: drag-select the red word, right-click, Copy must put it on the Windows clipboard; then text put on
 * the Windows clipboard, right-click, Paste must reach the shell. The page prevents the webview's native
 * menu and shows its own (TerminalView.svelte), so that menu is what is asserted; the dump records what the
 * native one did (`nativePrevented`). Returns `{ checks, dump }` like `inspectTerminal`.
 */
export async function inspectContextMenu(port) {
  const target = await until("a DevTools page target", () => terminalTarget(port), 30_000, 1000);
  const page = await connect(target.webSocketDebuggerUrl);
  const dump = { steps: [] };
  const checks = [];
  const note = (name, ok, detail) => checks.push({ name, ok, detail });
  const read = () => page.evaluate(MENU_READ);
  const mouse = (type, x, y, button = "left") => page.send("Input.dispatchMouseEvent", { type, x, y, button, buttons: type === "mouseReleased" ? 0 : button === "left" ? 1 : 2, clickCount: 1 });
  const click = async (x, y, button = "left") => {
    await page.send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
    await mouse("mousePressed", x, y, button);
    await mouse("mouseReleased", x, y, button);
  };
  const openMenu = async (x, y) => {
    await click(x, y, "right");
    return until("the Terminal menu to open", async () => {
      const s = await read();
      return s.menu && s.items.length > 0 ? s : false;
    }, 5000, 200);
  };
  const item = (state, name) => state.items.find((i) => i.name === name);
  try {
    await page.send("Runtime.enable");
    await page.evaluate(`window.addEventListener("contextmenu", (e) => { window.__nativeMenuPrevented = e.defaultPrevented; });`);
    let state = await read();
    if (!state.word) throw new Error("the colour sample is not on screen");
    // Select the word with a drag, as the rendering check does.
    const { word } = state;
    await page.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: word.left + 1, y: word.y });
    await mouse("mousePressed", word.left + 1, word.y);
    for (let i = 1; i <= 6; i++) await mouse("mouseMoved", word.left + 1 + ((word.width + 2) * i) / 6, word.y);
    await mouse("mouseReleased", word.left + word.width + 2, word.y);
    // Copy: right-click on the selected word, choose Copy, read the Windows clipboard from outside.
    windowsClipboard("clipboard-was-not-written");
    try {
      state = await openMenu(word.x, word.y);
      dump.steps.push({ copyMenu: state.items, nativePrevented: state.nativePrevented });
      const copy = item(state, "Copy");
      note("right-click opens the Terminal menu with Copy, Paste, Select all and Clear", ["Copy", "Paste", "Select all", "Clear"].every((n) => item(state, n)), JSON.stringify(state.items.map((i) => i.name)));
      note("Copy is enabled while a word is selected", Boolean(copy && !copy.disabled), JSON.stringify(copy));
      note("the webview's own menu is suppressed (the app's menu stands in)", state.nativePrevented === true, `contextmenu defaultPrevented: ${state.nativePrevented}`);
      if (copy && !copy.disabled) await click(copy.x, copy.y);
      const copied = await until("Copy to reach the Windows clipboard", () => {
        const text = windowsClipboard();
        return text !== "clipboard-was-not-written" ? { text } : false;
      }, 5000, 300).catch(() => ({ text: windowsClipboard() }));
      note("Copy puts the selected word on the Windows clipboard", copied.text === "COLOR-RED", JSON.stringify(copied.text));
    } catch (err) {
      note("right-click Copy works", false, String(err?.message ?? err));
    }
    // Paste: text on the Windows clipboard, right-click, Paste, and it must show at the shell's prompt.
    const marker = `PASTE-MARK-${Date.now() % 100000}`;
    windowsClipboard(marker);
    try {
      state = await openMenu(word.x, word.y);
      const paste = item(state, "Paste");
      note("Paste is offered in the Terminal menu", Boolean(paste && !paste.disabled), JSON.stringify(paste));
      if (paste) await click(paste.x, paste.y);
      const shown = await until("the pasted text to show in the shell", async () => ((await read()).rows.includes(marker) ? true : false), 10_000, 300).catch(() => false);
      if (shown) note("Paste sends the clipboard text to the shell", true, marker);
      else {
        // Not pasted: tell a read that waits at the webview's own clipboard prompt (setting Ask, the default; nobody
        // answers it on the runner, clipboard-permission.mjs) from a refusal or a read that works but never reached the shell.
        const probe = await page.evaluate(`Promise.race([navigator.clipboard.readText().then((t) => "read:" + t, (e) => "rejected:" + e.name + " " + e.message + " focus=" + document.hasFocus()), new Promise((r) => setTimeout(() => r("pending"), 3000))])`, true).catch((e) => `error:${e}`);
        dump.steps.push({ pasteProbe: probe });
        const prompt = probe === "pending";
        note("Paste sends the clipboard text to the shell, or waits at the webview's clipboard prompt (Ask)", prompt, prompt ? "the script read waits at the prompt: setting Ask, nobody to press Allow here, so the shell was not reached (follow-up: run this under Allow)" : `probe ${probe}; rows: ${(await read()).rows.slice(-200)}`);
      }
    } catch (err) {
      note("right-click Paste works", false, String(err?.message ?? err));
    }
    // Leave the prompt as it was: Ctrl+U drops the pasted text from the shell's line.
    await page.send("Input.dispatchKeyEvent", { type: "rawKeyDown", modifiers: 2, key: "u", code: "KeyU", windowsVirtualKeyCode: 85 }).catch(() => {});
    await page.send("Input.dispatchKeyEvent", { type: "keyUp", modifiers: 2, key: "u", code: "KeyU", windowsVirtualKeyCode: 85 }).catch(() => {});
    return { checks, dump };
  } finally {
    page.close();
  }
}

const firstFamily =(f) => String(f ?? "").split(",")[0].replace(/["'\s]/g, "").toLowerCase();

/**
 * Reads the Terminal page over DevTools port `port`. The terminal must show a line coloured red
 * ("COLOR-RED") and bold green ("COLOR-GREEN") by SGR sequences. Returns `{ checks, dump }`: `checks` is
 * a list of `{ name, ok, detail }`, `dump` the raw reads (computed styles, console) for an artifact.
 */
export async function inspectTerminal(port) {
  const target = await until("a DevTools page target", () => terminalTarget(port), 30_000, 1000).catch((err) => {
    throw new Error(`${err.message}; webview2 command lines: ${webviewCommandLines()}`);
  });
  const page = await connect(target.webSocketDebuggerUrl);
  const console_ = [];
  page.on("Runtime.consoleAPICalled", (p) => console_.push(`${p.type}: ${(p.args ?? []).map((a) => a.value ?? a.description ?? "").join(" ")}`));
  page.on("Runtime.exceptionThrown", (p) => console_.push(`exception: ${p.exceptionDetails?.text} ${p.exceptionDetails?.exception?.description ?? ""}`));
  page.on("Log.entryAdded", (p) => console_.push(`${p.entry.source}/${p.entry.level}: ${p.entry.text} ${p.entry.url ?? ""}`));
  try {
    await page.send("Runtime.enable");
    await page.send("Log.enable");
    await page.send("Page.enable");
    const read = await page.evaluate(READ);
    read.cspHeader = await page.evaluate(`fetch(location.href).then((r) => r.headers.get("content-security-policy"))`).catch((e) => String(e));
    // A real drag over the red word, with real mouse events: the emulator selects it.
    let selection = { cells: 0, width: 0, copied: "" };
    if (read.redBox) {
      const y = read.redBox.y + read.redBox.height / 2;
      const mouse = (type, x, extra = {}) => page.send("Input.dispatchMouseEvent", { type, x, y, button: "left", buttons: type === "mouseReleased" ? 0 : 1, clickCount: 1, ...extra });
      await page.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: read.redBox.x + 1, y });
      await mouse("mousePressed", read.redBox.x + 1);
      for (let i = 1; i <= 6; i++) await mouse("mouseMoved", read.redBox.x + 1 + ((read.redBox.width + 2) * i) / 6);
      await mouse("mouseReleased", read.redBox.x + read.redBox.width + 2);
      selection = await page.evaluate(`(() => {
        const cells = [...document.querySelectorAll(".xterm-selection div")];
        let copied = "";
        const grab = (e) => { copied = e.clipboardData?.getData("text/plain") ?? ""; };
        document.addEventListener("copy", grab);
        document.querySelector(".xterm-helper-textarea")?.focus();
        document.execCommand("copy");
        document.removeEventListener("copy", grab);
        return { cells: cells.length, width: Math.max(0, ...cells.map((c) => c.getBoundingClientRect().width)), copied };
      })()`);
    }
    const complaints = console_.filter((l) => /content security policy|refused to|violat|stylesheet|\.css/i.test(l));
    const sized = (r) => Boolean(r && r.width > 50 && r.height > 20);
    const checks = [
      { name: "the .xterm emulator exists with a size", ok: sized(read.xterm) && sized(read.screen), detail: JSON.stringify({ xterm: read.xterm, screen: read.screen, renderer: read.renderer }) },
      { name: "the emulator's stylesheet is applied", ok: read.helperPositioned === "absolute" && read.styleSheets > 0, detail: `xterm-helpers position ${read.helperPositioned}, ${read.styleSheets} style sheets` },
      { name: "the terminal uses the terminal font", ok: firstFamily(read.fontFamily) !== "" && firstFamily(read.fontFamily) === firstFamily(read.wantedFontFamily), detail: `${read.fontFamily} (wanted ${read.wantedFontFamily})` },
      {
        name: "ANSI colours differ from the default text",
        ok: Boolean(read.red && read.green && read.red.colour !== read.defaultColour && read.green.colour !== read.defaultColour && read.red.colour !== read.green.colour),
        detail: `default ${read.defaultColour}, red ${read.red?.colour} (${read.red?.classes}), green ${read.green?.colour} (${read.green?.classes}), background ${read.background}`,
      },
      { name: "a mouse drag selects the word", ok: selection.cells > 0 && selection.width > 0 && selection.copied === "COLOR-RED", detail: JSON.stringify(selection) },
      { name: "the page logged no CSP or stylesheet error", ok: complaints.length === 0, detail: complaints.slice(0, 3).join(" ¦ ") },
    ];
    return { checks, dump: { read, selection, console: console_ } };
  } finally {
    page.close();
  }
}
