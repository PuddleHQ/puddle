// SPDX-License-Identifier: AGPL-3.0-or-later
// What every Settings test file does around its tests: reset the stores, mount all ten pages,
// pick a value, save. Import it after the file's vi.mock calls.
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/svelte";
import { afterEach, beforeEach, expect, vi } from "vitest";
import SettingsHost from "#lib/testing/SettingsHost.svelte";
import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import { density } from "#lib/theme/density.svelte.ts";
import { palette } from "#lib/theme/palette.svelte.ts";
import { NO_CUSTOM } from "#lib/theme/palette.ts";
import { theme } from "#lib/theme/theme.svelte.ts";
import Appearance from "./(pages)/appearance/+page.svelte";
import Credentials from "./(pages)/credentials/+page.svelte";
import Editor from "./(pages)/editor/+page.svelte";
import EditorServer from "./(pages)/editor-server/+page.svelte";
import Environment from "./(pages)/environment/+page.svelte";
import Git from "./(pages)/git/+page.svelte";
import Network from "./(pages)/network/+page.svelte";
import Notifications from "./(pages)/notifications/+page.svelte";
import ThisComputer from "./(pages)/this-computer/+page.svelte";
import Workspaces from "./(pages)/workspaces/+page.svelte";
import { api } from "./settings-harness.ts";

export const ALL = [
  Appearance,
  Notifications,
  ThisComputer,
  Workspaces,
  Editor,
  Network,
  Credentials,
  Environment,
  EditorServer,
  Git,
];

export function installReset() {
  // Every test here mounts all ten pages of Settings, so one test costs about a second on an idle
  // host and several times that on a loaded one; the default 5 s limit is under the loaded cost.
  vi.setConfig({ testTimeout: 30_000 });
  beforeEach(() => {
    api.reset();
    globalSettings.view = null;
    globalSettings.consents = null;
    globalSettings.status = "loading";
    globalSettings.reason = null;
    toasts.items = [];
    localStorage.clear();
    delete document.documentElement.dataset["theme"];
    theme.choice = "system";
    delete document.documentElement.dataset["density"];
    density.choice = "comfortable";
    delete document.documentElement.dataset["palette"];
    document.documentElement.removeAttribute("style");
    palette.choice = "pond";
    palette.custom = { ...NO_CUSTOM };
  });
  afterEach(cleanup);
}

/** Every page of Settings at once, in the real frame, so a test reaches any setting by its label. */
export function show() {
  return render(SettingsHost, { props: { pages: ALL } });
}

export async function open() {
  show();
  await screen.findByRole("heading", { level: 2, name: "Appearance" });
}

// The licence test is the first to open "Third-party licences", which needs jsdom's queued `toggle`
// event, vitest's first load of the notices file and the first render of its text. Under load
// those cold steps outlasted that test's one-second wait, so open it once here, before the tests.
export async function warmUp() {
  const slow = { timeout: 30_000 };
  show();
  await screen.findByRole(
    "heading",
    { level: 2, name: "Privacy and about" },
    slow,
  );
  await fireEvent.click(screen.getByText("Third-party licences"));
  await screen.findByLabelText(
    "Third-party licences",
    { selector: "pre" },
    slow,
  );
  cleanup();
}

export async function choose(label: string | RegExp, value: string) {
  const select = screen.getByLabelText(label) as HTMLSelectElement;
  select.value = value;
  await fireEvent.change(select);
}

export const putBodies = () =>
  api.calls
    .map((c, i) => [c, api.bodies[i]] as const)
    .filter(([c]) => c === "PUT /api/settings");

/**
 * Lets every change in flight finish: presses the Save button of a typed value when one shows (the
 * Save button of a field, never a bar), then waits until no row says "Saving".
 */
export async function save(says?: string | RegExp) {
  const typed = screen.queryAllByRole("button", { name: /^Save / });
  for (const button of typed) await fireEvent.click(button);
  await waitFor(() => expect(screen.queryByText(/^Saving/)).toBeNull());
  if (says) await screen.findByText(says);
}
