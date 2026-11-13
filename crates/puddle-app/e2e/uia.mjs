// SPDX-License-Identifier: AGPL-3.0-or-later
// A UI Automation driver for the installed, release app on Windows: the real window, clicked and read
// the way a person does it (a release build has no DevTools port, so the CDP driver of smoke.mjs cannot
// reach it). One long-lived PowerShell (uia.ps1) answers one JSON request per line. WebView2 exposes the
// page's accessibility tree to UI Automation, so elements are found by their role and accessible name,
// the same names the Playwright suite (ui/e2e) uses. Plain Node, no packages.
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as sleep } from "node:timers/promises";
import { onChildEnd } from "./child-end.mjs";

const here = dirname(fileURLToPath(import.meta.url));

/** A selector: `role` and `name` (exact) or `nameRegex` (a .NET regex), `localized` (Chromium's "heading", "dialog", ...), inside the windows whose title matches `window`. */
export class Uia {
  #child;
  #lines;
  #pending = [];
  #queue = Promise.resolve();

  static async start(appPid) {
    const uia = new Uia();
    uia.#child = spawn("powershell", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", join(here, "uia.ps1"), "-AppProcessId", String(appPid)], {
      stdio: ["pipe", "pipe", "inherit"],
    });
    uia.#lines = createInterface({ input: uia.#child.stdout });
    uia.#lines.on("line", (line) => uia.#pending.shift()?.(line));
    const ready = await new Promise((resolve) => {
      uia.#pending.push(resolve);
      onChildEnd(uia.#child, () => resolve(undefined));
    });
    if (!ready || !JSON.parse(ready).ready) throw new Error("the UI Automation helper did not start");
    return uia;
  }

  /** One request, answered in order; a reply that says `ok: false` throws. */
  call(request, within = 90_000) {
    const run = async () => {
      const reply = await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error(`UI Automation request timed out: ${JSON.stringify(request)}`)), within);
        this.#pending.push((line) => {
          clearTimeout(timer);
          resolve(line);
        });
        this.#child.stdin.write(`${JSON.stringify(request)}\n`);
      });
      const parsed = JSON.parse(reply);
      if (!parsed.ok) throw new Error(`UI Automation: ${parsed.error}`);
      return parsed;
    };
    const result = this.#queue.then(run, run);
    this.#queue = result.catch(() => {});
    return result;
  }

  async windows(window) {
    return [].concat((await this.call({ action: "windows", window })).items ?? []);
  }

  async find(selector) {
    return [].concat((await this.call({ action: "find", ...selector })).items ?? []);
  }

  /** The names under the windows, one `Type | name` per line, for a failure's diagnosis. */
  async dump(window) {
    return [].concat((await this.call({ action: "dump", window })).lines ?? []);
  }

  /** Waits for the element, then clicks it; the reason is named at each call, never a fixed sleep. */
  async click(what, selector, within = 60_000) {
    return until(`${what} to be clickable`, async () => {
      const reply = await this.call({ action: "click", ...selector });
      return reply.found ? reply : false;
    }, within);
  }

  /** Waits for the element and returns it. */
  async waitFor(what, selector, within = 60_000) {
    return until(what, async () => (await this.find(selector)).find((e) => e.enabled !== false) ?? false, within);
  }

  /** Waits for a text field, puts `value` in it and returns what the field then holds. */
  async fill(what, selector, value, within = 60_000) {
    return until(`${what} to take its text`, async () => {
      const reply = await this.call({ action: "set", ...selector, value });
      return reply.found && reply.value === value ? reply : false;
    }, within);
  }

  /** The value a field holds now. */
  async valueOf(selector) {
    const reply = await this.call({ action: "value", ...selector });
    return reply.found ? reply.value : undefined;
  }

  close() {
    this.#child?.kill();
  }
}

/** Polls `probe` until it is truthy, for `within` ms; a probe that throws counts as not yet. */
export async function until(what, probe, within = 60_000, every = 700) {
  const deadline = Date.now() + within;
  let lastError;
  for (;;) {
    const value = await Promise.resolve()
      .then(probe)
      .catch((err) => {
        lastError = err;
      });
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out after ${Math.round(within / 1000)} s waiting for ${what}${lastError ? ` (${lastError.message})` : ""}`);
    await sleep(every);
  }
}
